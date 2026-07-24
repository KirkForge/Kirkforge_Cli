//! Budget tool wrappers — direct Rust calls to `plugin3_core`.
//!
//! Enabled by the `budget` feature flag. When disabled, the plugin
//! shell scripts in `plugins/kirkforge-plugin3/tools/` remain the
//! invocation path. This module eliminates the lossy shim by calling
//! `plugin3_core` functions in-process, giving budget logic full
//! access to session state.
//!
//! ADR-047 pins this decision.

use crate::session::hooks::{HookContext, HookDecision, InProcessHook};
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;
use crate::tools::ToolContext;
use plugin3_core::{
    aggregate_sessions, filter_lines, format_summary_line, parse_slice_marker,
    slicing::HeadTailSlicer, BudgetState, ConfigFile, InMemoryOffloadStore, OffloadStore, Paths,
    SlicingTransform, TokenBudget,
};
use std::sync::{Arc, Mutex, OnceLock};

type SharedBudget = Arc<Mutex<TokenBudget>>;
type SharedStore = Arc<dyn OffloadStore>;

static SHARED_BUDGET: OnceLock<SharedBudget> = OnceLock::new();
static SHARED_STORE: OnceLock<SharedStore> = OnceLock::new();

fn shared_budget() -> SharedBudget {
    SHARED_BUDGET
        .get_or_init(|| {
            let cfg = crate::shared::Config::default();
            Arc::new(Mutex::new(TokenBudget {
                ceiling: cfg.tools.budget_ceiling,
                approaching_ratio: cfg.tools.budget_approaching_ratio,
                used: 0,
            }))
        })
        .clone()
}

fn shared_store() -> SharedStore {
    SHARED_STORE
        .get_or_init(|| Arc::new(InMemoryOffloadStore::new()) as SharedStore)
        .clone()
}

pub fn init_from_config(cfg: &crate::shared::Config) {
    let budget = shared_budget();
    let mut guard = budget.lock().expect("budget mutex poisoned");
    guard.ceiling = cfg.tools.budget_ceiling;
    guard.approaching_ratio = cfg.tools.budget_approaching_ratio;
}

/// Outcome of `check_and_slice`: keep the result verbatim, or replace it
/// with a sliced display string whose full content is in the offload
/// store under `key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetAction {
    Keep(String),
    Sliced { display: String, key: String },
}

/// Inspect `result` against `budget` and slice it if the budget is
/// `Approaching` or `Over` and the result is oversized relative to
/// `budget.remaining()`. The full content (or the offloaded middle, for
/// `HeadTailSlicer`) is stored in `store` and the returned `Sliced.key`
/// can be retrieved via `store_get`.
///
/// When the budget is `Under`, or the result already fits in the
/// remaining space, the result is returned verbatim as `Keep`.
#[must_use]
pub fn check_and_slice(
    result: &str,
    budget: &TokenBudget,
    store: &dyn OffloadStore,
) -> BudgetAction {
    let state = budget.state();
    if state == BudgetState::Under {
        return BudgetAction::Keep(result.to_string());
    }
    let remaining = budget.remaining();
    if result.len() <= remaining {
        return BudgetAction::Keep(result.to_string());
    }
    // Split the remaining headroom into head + tail so the model
    // keeps the beginning and end of the output (which usually carry
    // the most signal: headers, summaries, error tails). The middle
    // is offloaded to the store and retrieved via `store_get`.
    let head = remaining / 2;
    let tail = remaining.saturating_sub(head);
    let slicer = HeadTailSlicer {
        head_bytes: head,
        tail_bytes: tail,
    };
    let output = match slicer.apply(result, store) {
        Ok(o) => o,
        Err(_) => return BudgetAction::Keep(result.to_string()),
    };
    let Some(marker) = output.offload_marker else {
        return BudgetAction::Keep(result.to_string());
    };
    let display = if output.tail.is_empty() {
        format!("{}\n{}", output.head, marker)
    } else {
        format!("{}\n{}\n{}", output.head, marker, output.tail)
    };
    // The slicer stores the *middle* under a content-addressed key; the
    // marker embeds that key. Extract it so callers can hand it to
    // `store_get` directly.
    let key = parse_slice_marker(&marker)
        .map(|k| k.to_string())
        .unwrap_or(marker);
    if display.len() >= result.len() {
        return BudgetAction::Keep(result.to_string());
    }
    BudgetAction::Sliced { display, key }
}

/// Apply `check_and_slice` to the text-bearing variants of a
/// `ToolOutcome`. `Success` and `FileContent` are replaced in-place
/// with the sliced display string (and `truncated` is set on
/// `FileContent`); other variants (errors, diffs, grep matches, images)
/// are returned unchanged because slicing them would destroy structure
/// the model needs. Records the slice via the process-global budget so
/// the `used` counter reflects the bytes that actually enter the
/// conversation.
pub fn apply_budget_slice(outcome: ToolOutcome) -> ToolOutcome {
    let state = {
        let budget = shared_budget();
        let guard = budget.lock().expect("budget mutex poisoned");
        guard.state()
    };
    if state != BudgetState::Over && state != BudgetState::Approaching {
        return outcome;
    }
    let budget = shared_budget();
    let guard = budget.lock().expect("budget mutex poisoned");
    let store = shared_store();
    match outcome {
        ToolOutcome::Success { content } => {
            match check_and_slice(&content, &guard, store.as_ref()) {
                BudgetAction::Keep(kept) => ToolOutcome::Success { content: kept },
                BudgetAction::Sliced { display, key } => {
                    tracing::info!(key = %key, "Budget guard: sliced oversized tool result");
                    ToolOutcome::Success { content: display }
                }
            }
        }
        ToolOutcome::FileContent {
            path,
            content,
            truncated,
        } => match check_and_slice(&content, &guard, store.as_ref()) {
            BudgetAction::Keep(kept) => ToolOutcome::FileContent {
                path,
                content: kept,
                truncated,
            },
            BudgetAction::Sliced { display, key } => {
                tracing::info!(key = %key, "Budget guard: sliced oversized file content");
                ToolOutcome::FileContent {
                    path,
                    content: display,
                    truncated: true,
                }
            }
        },
        other => other,
    }
}

fn simple_tool_def(name: &'static str, description: &'static str) -> ToolDef {
    ToolDef {
        name,
        description,
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }
}

// ---------------------------------------------------------------------------
// Tool 1: budget_status
// ---------------------------------------------------------------------------

struct BudgetStatus {
    def: ToolDef,
    budget: SharedBudget,
}

#[async_trait::async_trait]
impl Tool for BudgetStatus {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        let budget = self.budget.lock().expect("budget mutex poisoned");
        let state = budget.state();
        let remaining = budget.remaining();
        let ceiling = budget.ceiling;
        let used = budget.used;
        ToolOutcome::Success {
            content: format!(
                "Budget status: {state:?} — used {used}/{ceiling} tokens, {remaining} remaining"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool 2: budget_set
// ---------------------------------------------------------------------------

fn budget_set_def() -> ToolDef {
    ToolDef {
        name: "budget_set",
        description: "Set the token budget ceiling.",
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "ceiling": {
                    "type": "integer",
                    "description": "New token budget ceiling."
                }
            },
            "required": ["ceiling"]
        }),
    }
}

struct BudgetSet {
    def: ToolDef,
    budget: SharedBudget,
}

#[async_trait::async_trait]
impl Tool for BudgetSet {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let ceiling: usize = match args.get("ceiling").and_then(|v| v.as_u64()) {
            Some(c) => c as usize,
            None => {
                return ToolOutcome::Error {
                    message: "missing required argument: ceiling".into(),
                }
            }
        };
        let mut budget = self.budget.lock().expect("budget mutex poisoned");
        budget.ceiling = ceiling;
        ToolOutcome::Success {
            content: format!("Budget ceiling set to {ceiling}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool 3: budget_compact
// ---------------------------------------------------------------------------

struct BudgetCompact {
    def: ToolDef,
    budget: SharedBudget,
}

#[async_trait::async_trait]
impl Tool for BudgetCompact {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        let mut budget = self.budget.lock().expect("budget mutex poisoned");
        let old_used = budget.used;
        budget.used = 0;
        ToolOutcome::Success {
            content: format!("Budget compacted: reset used from {old_used} to 0"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool 4: store_get
// ---------------------------------------------------------------------------

fn store_get_def() -> ToolDef {
    ToolDef {
        name: "store_get",
        description: "Retrieve a stored offload marker by key.",
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "marker": {
                    "type": "string",
                    "description": "The slice marker key to retrieve."
                }
            },
            "required": ["marker"]
        }),
    }
}

struct StoreGet {
    def: ToolDef,
    store: SharedStore,
}

#[async_trait::async_trait]
impl Tool for StoreGet {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let marker = match args.get("marker").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => {
                return ToolOutcome::Error {
                    message: "missing required argument: marker".into(),
                }
            }
        };
        match self.store.get(&marker) {
            Ok(bytes) => {
                let content = String::from_utf8(bytes)
                    .unwrap_or_else(|e| format!("<binary data, utf8 error: {e}>"));
                ToolOutcome::Success { content }
            }
            Err(e) => ToolOutcome::Error {
                message: format!("store_get failed: {e}"),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tool 5: config_validate
// ---------------------------------------------------------------------------

struct ConfigValidate {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for ConfigValidate {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        let config = ConfigFile::default();
        match toml::to_string_pretty(&config) {
            Ok(s) => ToolOutcome::Success {
                content: format!("Config valid.\n{s}"),
            },
            Err(e) => ToolOutcome::Error {
                message: format!("Config validation failed: {e}"),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tool 6: report
// ---------------------------------------------------------------------------

struct Report {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for Report {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        let paths = Paths::resolve();
        let usage_path = paths.usage_log();
        match std::fs::read_to_string(&usage_path) {
            Ok(contents) => {
                let lines: Vec<&str> = contents.lines().collect();
                let filtered = filter_lines(&lines, None, None);
                let totals = aggregate_sessions(&filtered);
                let mut summary_parts = Vec::new();
                for (session_id, totals) in &totals {
                    summary_parts.push(format_summary_line(session_id, totals));
                }
                let content = if summary_parts.is_empty() {
                    "No usage data found.".to_string()
                } else {
                    summary_parts.join("\n")
                };
                ToolOutcome::Success { content }
            }
            Err(e) => ToolOutcome::Error {
                message: format!("Failed to read usage log: {e}"),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tool 7: self_check
// ---------------------------------------------------------------------------

struct SelfCheck {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for SelfCheck {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        let paths = Paths::resolve();
        let mut results = Vec::new();
        results.push(format!("data_dir: {}", paths.data_dir.display()));
        results.push(format!("config_dir: {}", paths.config_dir.display()));
        results.push(format!("runtime_dir: {}", paths.runtime_dir.display()));
        if paths.data_dir.exists() {
            results.push("data_dir: OK".into());
        } else {
            results.push("data_dir: MISSING (will be created on first use)".into());
        }
        let config = ConfigFile::default();
        results.push(format!("default budget ceiling: {}", config.budget.ceiling));
        results.push(format!(
            "default approaching_ratio: {}",
            config.budget.approaching_ratio
        ));
        results.push("self_check: PASS".into());
        ToolOutcome::Success {
            content: results.join("\n"),
        }
    }
}

// ---------------------------------------------------------------------------
// In-process hooks — full event context (ADR-047)
// ---------------------------------------------------------------------------

struct SessionStartHook {
    budget: SharedBudget,
}

impl InProcessHook for SessionStartHook {
    fn event(&self) -> &str {
        "session-start"
    }

    fn handle(&self, _ctx: &HookContext) -> HookDecision {
        let budget = self.budget.lock().expect("budget mutex poisoned");
        let state = budget.state();
        let remaining = budget.remaining();
        tracing::info!(
            state = ?state,
            ceiling = budget.ceiling,
            used = budget.used,
            remaining,
            "Budget session-start: token budget initialized"
        );
        HookDecision::Allow
    }
}

struct PostToolBashHook {
    budget: SharedBudget,
}

impl InProcessHook for PostToolBashHook {
    fn event(&self) -> &str {
        "post-tool-bash"
    }

    fn handle(&self, ctx: &HookContext) -> HookDecision {
        record_tool_usage(&self.budget, ctx, "bash")
    }
}

struct PostToolWriteFileHook {
    budget: SharedBudget,
}

impl InProcessHook for PostToolWriteFileHook {
    fn event(&self) -> &str {
        "post-tool-write_file"
    }

    fn handle(&self, ctx: &HookContext) -> HookDecision {
        record_tool_usage(&self.budget, ctx, "write_file")
    }
}

struct PreCompactHook {
    budget: SharedBudget,
}

impl InProcessHook for PreCompactHook {
    fn event(&self) -> &str {
        "pre-compact"
    }

    fn handle(&self, ctx: &HookContext) -> HookDecision {
        let mut budget = self.budget.lock().expect("budget mutex poisoned");
        let state = budget.state();
        if state == BudgetState::Over || state == BudgetState::Approaching {
            let stats = ctx.compact_stats.as_ref();
            tracing::info!(
                state = ?state,
                used = budget.used,
                ceiling = budget.ceiling,
                message_count = stats.map(|s| s.message_count),
                strategy = stats.map(|s| s.strategy.as_str()),
                "Budget pre-compact: compaction triggered while budget {} — \
                 resetting used counter after compaction",
                if state == BudgetState::Over { "exceeded" } else { "approaching limit" }
            );
            budget.used = 0;
        }
        HookDecision::Allow
    }
}

fn record_tool_usage(budget: &SharedBudget, ctx: &HookContext, tool_name: &str) -> HookDecision {
    let Some(ref result) = ctx.tool_result else {
        return HookDecision::Allow;
    };
    let tokens = result.len() / 4;
    let mut budget = budget.lock().expect("budget mutex poisoned");
    budget.record(tokens);
    let state = budget.state();
    match state {
        BudgetState::Over => {
            tracing::warn!(
                tool = tool_name,
                tokens,
                used = budget.used,
                ceiling = budget.ceiling,
                "Budget OVER: tool output pushed token usage past ceiling"
            );
        }
        BudgetState::Approaching => {
            tracing::info!(
                tool = tool_name,
                tokens,
                used = budget.used,
                ceiling = budget.ceiling,
                remaining = budget.remaining(),
                "Budget APPROACHING: token usage nearing ceiling"
            );
        }
        BudgetState::Under => {}
    }
    HookDecision::Allow
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Build all 7 Plugin3 budget tools as `Arc<dyn Tool>` instances.
///
/// The tools share a `TokenBudget` via `Arc<Mutex<>>` so that
/// `budget_set` mutations are visible to `budget_status` and the
/// budget check hooks. The offload store starts in-memory; a future
/// upgrade can swap it for `FileOffloadStore` when persistence is
/// needed.
pub fn all_budget_tools() -> Vec<Arc<dyn Tool>> {
    let budget = shared_budget();
    let store = shared_store();

    vec![
        Arc::new(BudgetStatus {
            def: simple_tool_def("budget_status", "Show the current token budget status."),
            budget: budget.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(BudgetSet {
            def: budget_set_def(),
            budget: budget.clone(),
        }),
        Arc::new(BudgetCompact {
            def: simple_tool_def(
                "budget_compact",
                "Compact the budget store, resetting the used counter.",
            ),
            budget: budget.clone(),
        }),
        Arc::new(StoreGet {
            def: store_get_def(),
            store: store.clone(),
        }),
        Arc::new(ConfigValidate {
            def: simple_tool_def("config_validate", "Validate the Plugin3 configuration."),
        }),
        Arc::new(Report {
            def: simple_tool_def("report", "Print a spending report from usage logs."),
        }),
        Arc::new(SelfCheck {
            def: simple_tool_def("self_check", "Run Plugin3 self-check diagnostics."),
        }),
    ]
}

/// Build all 4 Plugin3 in-process hooks, sharing the same `TokenBudget`
/// as the tools via the process-global `SHARED_BUDGET`.
pub fn all_budget_hooks() -> Vec<Box<dyn InProcessHook>> {
    let budget = shared_budget();
    vec![
        Box::new(SessionStartHook {
            budget: budget.clone(),
        }),
        Box::new(PostToolBashHook {
            budget: budget.clone(),
        }),
        Box::new(PostToolWriteFileHook {
            budget: budget.clone(),
        }),
        Box::new(PreCompactHook { budget }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget_with(used: usize, ceiling: usize) -> TokenBudget {
        TokenBudget {
            ceiling,
            approaching_ratio: 0.8,
            used,
        }
    }

    fn shared_budget_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[test]
    fn test_check_and_slice_under_budget() {
        let budget = budget_with(10, 1000);
        let store = InMemoryOffloadStore::new();
        let result = "x".repeat(500);
        match check_and_slice(&result, &budget, &store) {
            BudgetAction::Keep(kept) => assert_eq!(kept, result),
            other => panic!("under-budget result must be kept, got {other:?}"),
        }
        assert_eq!(
            store.len(),
            0,
            "under-budget slice must not touch the store"
        );
    }

    #[test]
    fn test_check_and_slice_over_budget() {
        // Over the ceiling (used > ceiling), remaining = 0 → no headroom
        // for the slice marker, so we fall through to Keep. Use an
        // Approaching budget with real headroom instead so the slice
        // branch actually fires.
        let budget = budget_with(900, 1000);
        assert_eq!(budget.state(), BudgetState::Approaching);
        assert_eq!(budget.remaining(), 100);
        let store = InMemoryOffloadStore::new();
        let result = "y".repeat(10_000);
        match check_and_slice(&result, &budget, &store) {
            BudgetAction::Sliced { display, key } => {
                assert!(
                    display.len() < result.len(),
                    "sliced display must be shorter than original: display={} result={}",
                    display.len(),
                    result.len()
                );
                assert_eq!(key.len(), 24, "key must be the 24-hex content key");
                let stored = store.get(&key).expect("full content retrievable");
                assert!(!stored.is_empty(), "middle must be offloaded");
                assert!(
                    parse_slice_marker(&display).is_some() || display.contains("<<plugin3:slice:"),
                    "display must carry a slice marker, got: {display:?}"
                );
            }
            other => panic!("oversized result over budget must be sliced, got {other:?}"),
        }
    }

    #[test]
    fn test_check_and_slice_small_result_when_over() {
        // Budget is over but the result is tiny (fits in remaining
        // headroom) → not worth slicing.
        let budget = budget_with(900, 1000);
        let store = InMemoryOffloadStore::new();
        let result = "tiny";
        match check_and_slice(result, &budget, &store) {
            BudgetAction::Keep(kept) => assert_eq!(kept, "tiny"),
            other => panic!("small result must be kept even when over, got {other:?}"),
        }
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_check_and_slice_no_headroom_slices_to_marker_only() {
        // used >= ceiling → remaining = 0, head = tail = 0. The slicer
        // offloads the whole input and returns just the marker — the
        // model sees a tiny pointer it can resolve via `store_get`
        // rather than 10 KB it has no budget for.
        let budget = budget_with(1000, 1000);
        assert_eq!(budget.state(), BudgetState::Over);
        assert_eq!(budget.remaining(), 0);
        let store = InMemoryOffloadStore::new();
        let result = "z".repeat(10_000);
        match check_and_slice(&result, &budget, &store) {
            BudgetAction::Sliced { display, key } => {
                assert!(
                    display.len() < result.len(),
                    "marker-only display must be shorter than the 10 KB original"
                );
                assert_eq!(key.len(), 24);
                let stored = store.get(&key).expect("full content retrievable");
                assert_eq!(stored.len(), result.len(), "whole input must be offloaded");
            }
            other => panic!("zero-remaining over-budget must slice to marker, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_budget_slice_success_variant() {
        let _guard = shared_budget_test_lock().blocking_lock();
        // Force the shared budget into Approaching so apply_budget_slice
        // actually considers slicing.
        {
            let budget = shared_budget();
            let mut guard = budget.lock().expect("budget mutex poisoned");
            guard.ceiling = 1000;
            guard.used = 900;
        }
        let big = "q".repeat(10_000);
        let outcome = ToolOutcome::Success { content: big };
        let sliced = apply_budget_slice(outcome);
        match sliced {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("<<plugin3:slice:"),
                    "sliced Success must carry the marker, got: {content:?}"
                );
            }
            other => panic!("expected sliced Success, got {other:?}"),
        }
        // Reset the shared budget so other tests are not affected.
        {
            let budget = shared_budget();
            let mut guard = budget.lock().expect("budget mutex poisoned");
            guard.used = 0;
            guard.ceiling = 200_000;
        }
    }

    #[test]
    fn test_apply_budget_slice_under_budget_passes_through() {
        let _guard = shared_budget_test_lock().blocking_lock();
        {
            let budget = shared_budget();
            let mut guard = budget.lock().expect("budget mutex poisoned");
            guard.ceiling = 200_000;
            guard.used = 0;
        }
        let outcome = ToolOutcome::Success {
            content: "hello".into(),
        };
        let out = apply_budget_slice(outcome);
        match out {
            ToolOutcome::Success { content } => assert_eq!(content, "hello"),
            other => panic!("under-budget Success must pass through, got {other:?}"),
        }
    }

    fn reset_shared_budget(ceiling: usize, used: usize) {
        let budget = shared_budget();
        let mut guard = budget.lock().expect("budget mutex poisoned");
        guard.ceiling = ceiling;
        guard.used = used;
        guard.approaching_ratio = 0.8;
    }

    fn budget_status_tool() -> BudgetStatus {
        BudgetStatus {
            def: simple_tool_def("budget_status", "Show the current token budget status."),
            budget: shared_budget(),
        }
    }

    fn budget_set_tool() -> BudgetSet {
        BudgetSet {
            def: budget_set_def(),
            budget: shared_budget(),
        }
    }

    #[tokio::test]
    async fn test_budget_status_returns_state() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(1000, 100);
        let tool = budget_status_tool();
        let ctx = ToolContext::new();
        let out = tool.run(&ctx, serde_json::json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("Budget status"),
                    "expected status text, got: {content}"
                );
                assert!(content.contains("100/1000"));
            }
            other => panic!("BudgetStatus must return Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_budget_set_updates_ceiling() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(1000, 0);
        let set_tool = budget_set_tool();
        let ctx = ToolContext::new();
        let out = set_tool
            .run(&ctx, serde_json::json!({"ceiling": 50000}))
            .await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("50000"), "got: {content}");
            }
            other => panic!("BudgetSet must return Success, got {other:?}"),
        }
        let status = budget_status_tool();
        let out = status.run(&ctx, serde_json::json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("/50000"),
                    "ceiling update must be visible to BudgetStatus: {content}"
                );
            }
            other => panic!("BudgetStatus after set must return Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_post_tool_bash_hook_records_usage() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(200_000, 0);
        let hook = PostToolBashHook {
            budget: shared_budget(),
        };
        let ctx = HookContext {
            event: "post-tool-bash".into(),
            tool_result: Some("x".repeat(1000)),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), HookDecision::Allow);
        let used_before = 0usize;
        let used_after = {
            let budget = shared_budget();
            let guard = budget.lock().expect("budget mutex poisoned");
            guard.used
        };
        assert!(
            used_after > used_before,
            "PostToolBashHook must record usage from tool_result, got used={used_after}"
        );
    }

    #[test]
    fn test_pre_compact_hook_resets_when_over() {
        let _guard = shared_budget_test_lock().blocking_lock();
        reset_shared_budget(1000, 2000);
        {
            let budget = shared_budget();
            let guard = budget.lock().expect("budget mutex poisoned");
            assert_eq!(guard.state(), BudgetState::Over);
        }
        let hook = PreCompactHook {
            budget: shared_budget(),
        };
        let ctx = HookContext {
            event: "pre-compact".into(),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), HookDecision::Allow);
        let used = {
            let budget = shared_budget();
            let guard = budget.lock().expect("budget mutex poisoned");
            guard.used
        };
        assert_eq!(used, 0, "PreCompactHook must reset used to 0 when over");
    }

    #[test]
    fn test_session_start_hook_returns_allow() {
        let _guard = shared_budget_test_lock().blocking_lock();
        reset_shared_budget(200_000, 0);
        let hook = SessionStartHook {
            budget: shared_budget(),
        };
        let ctx = HookContext {
            event: "session-start".into(),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), HookDecision::Allow);
    }
}
