//! Budget tool wrappers — direct Rust calls to `kf_budget_core`.
//!
//! This module calls `kf_budget_core` functions in-process, giving
//! budget logic full access to session state.
//!
//! ADR-047 pins this decision.

use crate::session::hooks::{HookContext, PostHook};
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;
use crate::tools::ToolContext;
use kf_budget_core::{
    aggregate_sessions, filter_lines, format_summary_line, parse_slice_marker,
    slicing::HeadTailSlicer, BudgetState, ConfigFile, InMemoryOffloadStore, OffloadStore, Paths,
    SlicingTransform, TokenBudget,
};
use std::sync::{Arc, Mutex, OnceLock};

pub type SharedBudget = Arc<Mutex<TokenBudget>>;
pub type SharedStore = Arc<dyn OffloadStore>;

/// Per-session budget constructor (WO 22.6-R2).
pub fn new_session_budget(cfg: &crate::shared::Config) -> SharedBudget {
    Arc::new(Mutex::new(TokenBudget {
        ceiling: cfg.tools.budget_ceiling,
        approaching_ratio: cfg.tools.budget_approaching_ratio,
        used: 0,
    }))
}

/// Per-session offload store constructor with a cap of 1000 entries.
// ponytail: per-session store, cap 1000 entries, evict FIFO if throughput matters
pub fn new_session_store() -> SharedStore {
    Arc::new(InMemoryOffloadStore::new_with_cap(1000)) as SharedStore
}

/// Initialize an existing budget from config.
pub fn init_from_config(budget: &SharedBudget, cfg: &crate::shared::Config) {
    let mut guard = budget.lock().unwrap_or_else(|e| e.into_inner());
    guard.ceiling = cfg.tools.budget_ceiling;
    guard.approaching_ratio = cfg.tools.budget_approaching_ratio;
}

#[cfg(test)]
static SHARED_BUDGET: OnceLock<SharedBudget> = OnceLock::new();
#[cfg(test)]
static SHARED_STORE: OnceLock<SharedStore> = OnceLock::new();

#[cfg(test)]
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

#[cfg(test)]
fn shared_store() -> SharedStore {
    SHARED_STORE
        .get_or_init(|| Arc::new(InMemoryOffloadStore::new()) as SharedStore)
        .clone()
}

// ── Sliced-event coordination (WO 8.6) ─────────────────────────────────
//
// The budget guard and Stratum (input-side compression) are folded
// separately (ADR-046 + ADR-047) but are not coordinated. WO 8.6
// wires them together: when the budget slices a tool result, Stratum
// is asked to compress the sliced display so the model sees a single
// post-coordination size, and `budget.used` reflects the post-Stratum
// size. The dispatch is a sync registered-listener model — not the
// removed async EventBus — because the slice path is itself sync and
// the in-process test runtime is single-threaded (a bus roundtrip
// would require `block_in_place` and panic per
// `AGENTS.md` §7).

/// Payload of a `BudgetSliced` notification. Carries the pre- and
/// post-slice byte sizes, the offload-store key for the original
/// middle, and the sliced display that entered the conversation.
///
/// Listeners (e.g. the Stratum compression hook) receive this and may
/// return a replacement string. If they do, the budget records the
/// post-compression size in `used` so the conversation token count
/// reflects what the model actually sees.
#[derive(Debug, Clone)]
pub struct BudgetSlicedEvent {
    pub original_size: usize,
    pub sliced_size: usize,
    pub key: String,
    pub sliced_display: String,
}

/// A sync listener registered on the budget's slice path. Receives
/// the [`BudgetSlicedEvent`] and returns an optional replacement
/// string. Returning `Some` swaps the sliced display for the
/// replacement; `None` leaves it unchanged.
pub type BudgetSlicedListener = Arc<dyn Fn(BudgetSlicedEvent) -> Option<String> + Send + Sync>;

// ceiling: append-only, no bounded eviction. Safe because dispatch
// uses "first-returns-Some wins" semantics — stale listeners after a
// plugin reload are unreachable dead code, not duplicate dispatches.
// Upgrade path: if reload-heavy sessions leak meaningful memory, scope
// into SessionStores or add a generation counter that invalidates stale
// entries on the next push.
static SLICED_LISTENERS: OnceLock<Mutex<Vec<BudgetSlicedListener>>> = OnceLock::new();

fn sliced_listeners() -> &'static Mutex<Vec<BudgetSlicedListener>> {
    SLICED_LISTENERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a listener that fires on every successful slice. Intended
/// for the Stratum compression hook. The listener is invoked
/// synchronously from `apply_budget_slice` after the slice decision
/// is made but before the post-coordination `used` adjustment.
///
/// Listeners accumulate across plugin reloads (append-only). The
/// dispatch loop short-circuits on the first `Some` return, so
/// earlier listeners shadow later ones — duplicate registrations
/// are harmless but waste an `Arc` allocation per reload.
pub fn register_sliced_listener(listener: BudgetSlicedListener) {
    let mut guard = sliced_listeners().lock().unwrap_or_else(|e| e.into_inner());
    guard.push(listener);
}

/// Number of registered sliced listeners — for tests.
#[cfg(test)]
pub fn sliced_listener_count() -> usize {
    let guard = sliced_listeners().lock().unwrap_or_else(|e| e.into_inner());
    guard.len()
}

#[cfg(test)]
pub fn clear_sliced_listeners() {
    let mut guard = sliced_listeners().lock().unwrap_or_else(|e| e.into_inner());
    guard.clear();
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
        Err(e) => {
            tracing::warn!("budget slicer error, keeping full result: {e}");
            return BudgetAction::Keep(result.to_string());
        }
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
/// conversation. When sliced, dispatches a `BudgetSlicedEvent` to any
/// registered listener (e.g. Stratum) and uses the listener's
/// replacement string if it returns one (WO 8.6).
///
/// The post-tool hook (`record_tool_usage` in this module) records
/// the token count of whatever content the `ToolOutcome`
/// carries. When a listener compresses the sliced display, the
/// returned `ToolOutcome` already carries the compressed content, so
/// the post-tool hook records the post-compression tokens
/// automatically — no extra `used` bookkeeping is needed in this
/// path. When the `stratum` feature is enabled, also calls into
/// Stratum to auto-escalate `Lite → Full` when the budget is
/// `Approaching`.
pub fn apply_budget_slice(
    outcome: ToolOutcome,
    budget: &SharedBudget,
    store: &SharedStore,
) -> ToolOutcome {
    let state = {
        let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
        guard.state()
    };
    if state != BudgetState::Over && state != BudgetState::Approaching {
        return outcome;
    }
    if state == BudgetState::Approaching {
        maybe_escalate_stratum();
    }
    let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
    match outcome {
        ToolOutcome::Success { content } => {
            let original_size = content.len();
            let action = check_and_slice(&content, &guard, store.as_ref());
            drop(guard);
            match action {
                BudgetAction::Keep(kept) => ToolOutcome::Success { content: kept },
                BudgetAction::Sliced { display, key } => {
                    let replacement = dispatch_sliced(BudgetSlicedEvent {
                        original_size,
                        sliced_size: display.len(),
                        key: key.clone(),
                        sliced_display: display.clone(),
                    });
                    if let Some(new_display) = replacement {
                        let before = display.len();
                        let after = new_display.len();
                        tracing::info!(
                            key = %key,
                            before,
                            after,
                            "Budget guard: slice compressed by registered listener"
                        );
                        ToolOutcome::Success {
                            content: new_display,
                        }
                    } else {
                        tracing::info!(key = %key, "Budget guard: sliced oversized tool result");
                        ToolOutcome::Success { content: display }
                    }
                }
            }
        }
        ToolOutcome::FileContent {
            path,
            content,
            truncated,
        } => {
            let original_size = content.len();
            let action = check_and_slice(&content, &guard, store.as_ref());
            drop(guard);
            match action {
                BudgetAction::Keep(kept) => ToolOutcome::FileContent {
                    path,
                    content: kept,
                    truncated,
                },
                BudgetAction::Sliced { display, key } => {
                    let replacement = dispatch_sliced(BudgetSlicedEvent {
                        original_size,
                        sliced_size: display.len(),
                        key: key.clone(),
                        sliced_display: display.clone(),
                    });
                    if let Some(new_display) = replacement {
                        let before = display.len();
                        let after = new_display.len();
                        tracing::info!(
                            key = %key,
                            before,
                            after,
                            "Budget guard: file-content slice compressed by registered listener"
                        );
                        ToolOutcome::FileContent {
                            path,
                            content: new_display,
                            truncated: true,
                        }
                    } else {
                        tracing::info!(key = %key, "Budget guard: sliced oversized file content");
                        ToolOutcome::FileContent {
                            path,
                            content: display,
                            truncated: true,
                        }
                    }
                }
            }
        }
        other => other,
    }
}

/// Dispatch a `BudgetSlicedEvent` to all registered listeners. The
/// first listener that returns `Some` wins; the rest are skipped for
/// this event. Returns `None` if no listener returned a replacement.
fn dispatch_sliced(event: BudgetSlicedEvent) -> Option<String> {
    let listeners = sliced_listeners().lock().unwrap_or_else(|e| e.into_inner());
    for listener in listeners.iter() {
        if let Some(replacement) = listener(event.clone()) {
            return Some(replacement);
        }
    }
    None
}

/// Auto-escalate Stratum's session mode from `Lite` to `Full` when
/// the budget is `Approaching`. No-op when the `stratum` feature is
/// off, when the session mode is already `Full`/`Ultra`/`Off`, or
/// when the budget is `Over` (which already implies aggressive
/// intervention via the slicing path itself). WO 8.6.
fn maybe_escalate_stratum() {
    #[cfg(feature = "stratum")]
    {
        use kf_compress_core::mode::Mode;
        let current = crate::session::stratum::current_session_mode();
        if current == Mode::Lite {
            crate::session::stratum::set_session_mode(Mode::Full);
            tracing::info!(
                from = "lite",
                to = "full",
                "Budget Approaching: auto-escalated Stratum mode"
            );
        }
    }
    #[cfg(not(feature = "stratum"))]
    {
        let _ = (); // stratum feature off: no escalation
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
        let budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
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

#[cfg(feature = "stratum")]
struct StoreGet {
    def: ToolDef,
    store: SharedStore,
    stratum_store: Arc<kf_compress_core::store::InMemoryOffloadStore>,
}

#[cfg(not(feature = "stratum"))]
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
        // Primary: the budget HeadTailSlicer store.
        if let Ok(bytes) = self.store.get(&marker) {
            let content = String::from_utf8(bytes)
                .unwrap_or_else(|e| format!("<binary data, utf8 error: {e}>"));
            return ToolOutcome::Success { content };
        }
        // Fallback: the Stratum CompressionPipeline store. The model sees
        // markers from both paths; both must resolve (WO 20.11.0 CRIT-2).
        // ponytail: ceiling — only the in-process Stratum store is consulted.
        // If a future FileOffloadStore or cross-process store is added, this
        // lookup must learn about it too.
        #[cfg(feature = "stratum")]
        if let Some(content) = <kf_compress_core::store::InMemoryOffloadStore as kf_compress_core::store::OffloadStore>::get(
            &*self.stratum_store,
            &marker,
        ) {
            return ToolOutcome::Success { content };
        }
        ToolOutcome::Error {
            message: format!("store_get failed: marker '{marker}' not in any offload store"),
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

impl PostHook for SessionStartHook {
    fn event(&self) -> &str {
        "session-start"
    }

    fn handle(&self, _ctx: &HookContext) -> Result<(), String> {
        let budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
        let state = budget.state();
        let remaining = budget.remaining();
        tracing::info!(
            state = ?state,
            ceiling = budget.ceiling,
            used = budget.used,
            remaining,
            "Budget session-start: token budget initialized"
        );
        Ok(())
    }
}

struct PostToolBashHook {
    budget: SharedBudget,
}

impl PostHook for PostToolBashHook {
    fn event(&self) -> &str {
        "post-tool-bash"
    }

    fn handle(&self, ctx: &HookContext) -> Result<(), String> {
        record_tool_usage(&self.budget, ctx, "bash")
    }
}

struct PostToolWriteFileHook {
    budget: SharedBudget,
}

impl PostHook for PostToolWriteFileHook {
    fn event(&self) -> &str {
        "post-tool-write_file"
    }

    fn handle(&self, ctx: &HookContext) -> Result<(), String> {
        record_tool_usage(&self.budget, ctx, "write_file")
    }
}

struct PreCompactHook {
    budget: SharedBudget,
}

impl PostHook for PreCompactHook {
    fn event(&self) -> &str {
        "pre-compact"
    }

    fn handle(&self, ctx: &HookContext) -> Result<(), String> {
        let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
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
            drop(budget);
            // WO 8.6: pre-compact with budget pressure is the
            // natural escalation point for Stratum. Future
            // post-compaction tool outputs will be compressed
            // more aggressively. Idempotent — already at Full
            // or Ultra is a no-op.
            maybe_escalate_stratum();
        }
        Ok(())
    }
}

fn record_tool_usage(
    budget: &SharedBudget,
    ctx: &HookContext,
    tool_name: &str,
) -> Result<(), String> {
    let Some(ref result) = ctx.tool_result else {
        return Ok(());
    };
    let tokens = crate::session::prompt::count_tokens(result);
    let mut budget = budget.lock().unwrap_or_else(|e| e.into_inner());
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
    Ok(())
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Build all 7 budget tools as `Arc<dyn Tool>` instances.
///
/// The tools share a `TokenBudget` via `Arc<Mutex<>>` so that
/// `budget_set` mutations are visible to `budget_status` and the
/// budget check hooks. The offload store starts in-memory; a future
/// upgrade can swap it for `FileOffloadStore` when persistence is
/// needed.
#[cfg(all(feature = "budget", feature = "stratum"))]
pub fn all_budget_tools(
    budget: &SharedBudget,
    store: &SharedStore,
    stratum_store: Arc<kf_compress_core::store::InMemoryOffloadStore>,
) -> Vec<Arc<dyn Tool>> {
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
            stratum_store,
        }),
        Arc::new(ConfigValidate {
            def: simple_tool_def("config_validate", "Validate the budget configuration."),
        }),
        Arc::new(Report {
            def: simple_tool_def("report", "Print a spending report from usage logs."),
        }),
        Arc::new(SelfCheck {
            def: simple_tool_def("self_check", "Run budget self-check diagnostics."),
        }),
    ]
}

#[cfg(all(feature = "budget", not(feature = "stratum")))]
pub fn all_budget_tools(budget: &SharedBudget, store: &SharedStore) -> Vec<Arc<dyn Tool>> {
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
            def: simple_tool_def("config_validate", "Validate the budget configuration."),
        }),
        Arc::new(Report {
            def: simple_tool_def("report", "Print a spending report from usage logs."),
        }),
        Arc::new(SelfCheck {
            def: simple_tool_def("self_check", "Run budget self-check diagnostics."),
        }),
    ]
}

/// Budget in-process hooks (all observational/post-hooks), sharing the same
/// `TokenBudget` as the tools.
pub fn budget_hooks(budget: &SharedBudget) -> Vec<Box<dyn PostHook>> {
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
        Box::new(PreCompactHook {
            budget: budget.clone(),
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::hooks::PostHook;

    fn test_stratum_store() -> Arc<kf_compress_core::store::InMemoryOffloadStore> {
        Arc::new(kf_compress_core::store::InMemoryOffloadStore::new())
    }

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
                    parse_slice_marker(&display).is_some()
                        || display.contains("<<kf-budget:slice:"),
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
            let mut guard = budget.lock().unwrap_or_else(|e| e.into_inner());
            guard.ceiling = 1000;
            guard.used = 900;
        }
        let big = "q".repeat(10_000);
        let outcome = ToolOutcome::Success { content: big };
        let sliced = apply_budget_slice(outcome, &shared_budget(), &shared_store());
        match sliced {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("<<kf-budget:slice:"),
                    "sliced Success must carry the marker, got: {content:?}"
                );
            }
            other => panic!("expected sliced Success, got {other:?}"),
        }
        // Reset the shared budget so other tests are not affected.
        {
            let budget = shared_budget();
            let mut guard = budget.lock().unwrap_or_else(|e| e.into_inner());
            guard.used = 0;
            guard.ceiling = 200_000;
        }
    }

    #[test]
    fn test_apply_budget_slice_under_budget_passes_through() {
        let _guard = shared_budget_test_lock().blocking_lock();
        {
            let budget = shared_budget();
            let mut guard = budget.lock().unwrap_or_else(|e| e.into_inner());
            guard.ceiling = 200_000;
            guard.used = 0;
        }
        let outcome = ToolOutcome::Success {
            content: "hello".into(),
        };
        let out = apply_budget_slice(outcome, &shared_budget(), &shared_store());
        match out {
            ToolOutcome::Success { content } => assert_eq!(content, "hello"),
            other => panic!("under-budget Success must pass through, got {other:?}"),
        }
    }

    fn reset_shared_budget(ceiling: usize, used: usize) {
        let budget = shared_budget();
        let mut guard = budget.lock().unwrap_or_else(|e| e.into_inner());
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
        assert_eq!(hook.handle(&ctx), Ok(()));
        let used_before = 0usize;
        let used_after = {
            let budget = shared_budget();
            let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
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
            let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(guard.state(), BudgetState::Over);
        }
        let hook = PreCompactHook {
            budget: shared_budget(),
        };
        let ctx = HookContext {
            event: "pre-compact".into(),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), Ok(()));
        let used = {
            let budget = shared_budget();
            let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
            guard.used
        };
        assert_eq!(used, 0, "PreCompactHook must reset used to 0 when over");
    }

    #[test]
    fn test_session_start_hook_returns_ok() {
        let _guard = shared_budget_test_lock().blocking_lock();
        reset_shared_budget(200_000, 0);
        let hook = SessionStartHook {
            budget: shared_budget(),
        };
        let ctx = HookContext {
            event: "session-start".into(),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), Ok(()));
    }

    #[tokio::test]
    async fn test_check_and_slice_result_fits_exactly_returns_keep() {
        let budget = budget_with(900, 1000);
        let store = InMemoryOffloadStore::new();
        let result = "x".repeat(100);
        match check_and_slice(&result, &budget, &store) {
            BudgetAction::Keep(kept) => assert_eq!(kept.len(), 100),
            other => panic!("exact-fit result must be kept, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_check_and_slice_over_budget_zero_remaining_slices_to_marker_only() {
        let budget = TokenBudget {
            ceiling: 1000,
            approaching_ratio: 0.5,
            used: 950,
        };
        assert_eq!(budget.state(), BudgetState::Approaching);
        let store = InMemoryOffloadStore::new();
        let result = "y".repeat(50);
        match check_and_slice(&result, &budget, &store) {
            BudgetAction::Keep(kept) => assert_eq!(kept, result),
            other => panic!("fits-in-remaining must be kept, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_budget_action_equality_keep() {
        assert_eq!(
            BudgetAction::Keep("x".into()),
            BudgetAction::Keep("x".into())
        );
        assert_ne!(
            BudgetAction::Keep("x".into()),
            BudgetAction::Keep("y".into())
        );
    }

    #[tokio::test]
    async fn test_budget_action_equality_sliced() {
        assert_eq!(
            BudgetAction::Sliced {
                display: "d".into(),
                key: "k".into(),
            },
            BudgetAction::Sliced {
                display: "d".into(),
                key: "k".into(),
            }
        );
        assert_ne!(
            BudgetAction::Sliced {
                display: "d".into(),
                key: "k".into(),
            },
            BudgetAction::Sliced {
                display: "d".into(),
                key: "z".into(),
            }
        );
    }

    #[tokio::test]
    async fn test_budget_action_ne_between_variants() {
        assert_ne!(
            BudgetAction::Keep("x".into()),
            BudgetAction::Sliced {
                display: "x".into(),
                key: "k".into(),
            }
        );
    }

    #[tokio::test]
    async fn test_init_from_config_updates_shared_budget() {
        let _guard = shared_budget_test_lock().lock().await;
        let mut cfg = crate::shared::Config::default();
        cfg.tools.budget_ceiling = 12_345;
        cfg.tools.budget_approaching_ratio = 0.9;
        init_from_config(&shared_budget(), &cfg);
        let budget = shared_budget();
        let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(guard.ceiling, 12_345);
        assert_eq!(guard.approaching_ratio, 0.9);
        drop(guard);
        reset_shared_budget(200_000, 0);
    }

    #[tokio::test]
    async fn test_budget_set_missing_ceiling_arg_returns_error() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(1000, 0);
        let set_tool = budget_set_tool();
        let ctx = ToolContext::new();
        let out = set_tool.run(&ctx, serde_json::json!({})).await;
        match out {
            ToolOutcome::Error { message } => {
                assert!(message.contains("ceiling"), "got: {message}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_budget_set_non_integer_ceiling_returns_error() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(1000, 0);
        let set_tool = budget_set_tool();
        let ctx = ToolContext::new();
        let out = set_tool
            .run(&ctx, serde_json::json!({"ceiling": "not-a-number"}))
            .await;
        assert!(matches!(out, ToolOutcome::Error { .. }));
    }

    #[tokio::test]
    async fn test_budget_compact_resets_used_to_zero() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(1000, 500);
        let compact = BudgetCompact {
            def: simple_tool_def("budget_compact", "Compact the budget store."),
            budget: shared_budget(),
        };
        let ctx = ToolContext::new();
        let out = compact.run(&ctx, serde_json::json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("reset"), "got: {content}");
                assert!(content.contains("500"));
            }
            other => panic!("expected Success, got {other:?}"),
        }
        let used = {
            let budget = shared_budget();
            let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
            guard.used
        };
        assert_eq!(used, 0);
    }

    #[tokio::test]
    async fn test_store_get_missing_marker_arg_returns_error() {
        let store_get = StoreGet {
            def: store_get_def(),
            store: shared_store(),
            #[cfg(feature = "stratum")]
            stratum_store: test_stratum_store(),
        };
        let ctx = ToolContext::new();
        let out = store_get.run(&ctx, serde_json::json!({})).await;
        assert!(matches!(out, ToolOutcome::Error { .. }));
    }

    #[tokio::test]
    async fn test_store_get_unknown_marker_returns_error() {
        let store_get = StoreGet {
            def: store_get_def(),
            store: shared_store(),
            #[cfg(feature = "stratum")]
            stratum_store: test_stratum_store(),
        };
        let ctx = ToolContext::new();
        let out = store_get
            .run(&ctx, serde_json::json!({"marker": "no-such-key"}))
            .await;
        match out {
            ToolOutcome::Error { message } => assert!(message.contains("store_get")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Regression for WO 20.11.0 CRIT-2: a marker written by the Stratum
    /// CompressionPipeline (kf_compress_core::store) must be retrievable
    /// via `store_get`, which lives in the budget path. Previously the two
    /// stores were disjoint, so Stratum-emitted markers were dead pointers.
    #[tokio::test]
    #[cfg(feature = "stratum")]
    async fn test_store_get_resolves_stratum_offload_marker() {
        use kf_compress_core::store::OffloadStore as _;
        let stratum_store = test_stratum_store();
        let payload = "stratum-offloaded payload body";
        let key = stratum_store.put(payload);
        let store_get = StoreGet {
            def: store_get_def(),
            store: shared_store(),
            stratum_store,
        };
        let ctx = ToolContext::new();
        let out = store_get
            .run(&ctx, serde_json::json!({"marker": key}))
            .await;
        match out {
            ToolOutcome::Success { content } => assert_eq!(content, payload),
            other => panic!("expected Success for stratum marker, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_config_validate_returns_success_with_default_config() {
        let validate = ConfigValidate {
            def: simple_tool_def("config_validate", "Validate config."),
        };
        let ctx = ToolContext::new();
        let out = validate.run(&ctx, serde_json::json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("Config valid"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_self_check_returns_paths_and_pass() {
        let check = SelfCheck {
            def: simple_tool_def("self_check", "Diagnostics."),
        };
        let ctx = ToolContext::new();
        let out = check.run(&ctx, serde_json::json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("data_dir"), "got: {content}");
                assert!(content.contains("PASS"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_record_tool_usage_no_result_returns_allow() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(200_000, 0);
        let budget = shared_budget();
        let ctx = HookContext {
            event: "post-tool-bash".into(),
            tool_result: None,
            ..Default::default()
        };
        let decision = record_tool_usage(&budget, &ctx, "bash");
        assert_eq!(decision, Ok(()));
        let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(guard.used, 0, "no result should not record usage");
    }

    #[tokio::test]
    async fn test_record_tool_usage_empty_result_records_zero() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(200_000, 0);
        let budget = shared_budget();
        let ctx = HookContext {
            event: "post-tool-bash".into(),
            tool_result: Some(String::new()),
            ..Default::default()
        };
        let decision = record_tool_usage(&budget, &ctx, "bash");
        assert_eq!(decision, Ok(()));
        let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(guard.used, 0, "empty result records 0 tokens");
    }

    #[tokio::test]
    async fn test_record_tool_usage_pushes_into_approaching() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(1000, 800);
        let budget = shared_budget();
        let big = "x".repeat(200);
        let ctx = HookContext {
            event: "post-tool-write_file".into(),
            tool_result: Some(big),
            ..Default::default()
        };
        let _ = record_tool_usage(&budget, &ctx, "write_file");
        let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(guard.state(), BudgetState::Approaching);
    }

    #[tokio::test]
    async fn test_post_tool_write_file_hook_records_usage() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(200_000, 0);
        let hook = PostToolWriteFileHook {
            budget: shared_budget(),
        };
        let ctx = HookContext {
            event: "post-tool-write_file".into(),
            tool_result: Some("x".repeat(400)),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), Ok(()));
        let budget = shared_budget();
        let guard = budget.lock().unwrap_or_else(|e| e.into_inner());
        assert!(guard.used > 0, "write_file hook must record usage");
    }

    #[tokio::test]
    async fn test_dispatch_sliced_no_listeners_returns_none() {
        let _guard = shared_budget_test_lock().lock().await;
        clear_sliced_listeners();
        let event = BudgetSlicedEvent {
            original_size: 100,
            sliced_size: 50,
            key: "k".into(),
            sliced_display: "d".into(),
        };
        assert!(dispatch_sliced(event).is_none());
    }

    #[tokio::test]
    async fn test_dispatch_sliced_listener_returns_none_propagates_none() {
        let _guard = shared_budget_test_lock().lock().await;
        clear_sliced_listeners();
        register_sliced_listener(std::sync::Arc::new(|_event: BudgetSlicedEvent| None));
        let event = BudgetSlicedEvent {
            original_size: 100,
            sliced_size: 50,
            key: "k".into(),
            sliced_display: "d".into(),
        };
        assert!(dispatch_sliced(event).is_none());
        clear_sliced_listeners();
    }

    #[tokio::test]
    async fn test_dispatch_sliced_first_listener_wins() {
        let _guard = shared_budget_test_lock().lock().await;
        clear_sliced_listeners();
        register_sliced_listener(std::sync::Arc::new(|event: BudgetSlicedEvent| {
            Some(format!("first:{}", event.sliced_size))
        }));
        register_sliced_listener(std::sync::Arc::new(|_event: BudgetSlicedEvent| {
            Some("second".to_string())
        }));
        let event = BudgetSlicedEvent {
            original_size: 100,
            sliced_size: 50,
            key: "k".into(),
            sliced_display: "d".into(),
        };
        let result = dispatch_sliced(event);
        assert_eq!(result.as_deref(), Some("first:50"));
        clear_sliced_listeners();
    }

    #[tokio::test]
    async fn test_apply_budget_slice_passes_through_errors_unchanged() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(1000, 950);
        let outcome = ToolOutcome::Error {
            message: "boom".into(),
        };
        let result = apply_budget_slice(outcome, &shared_budget(), &shared_store());
        match result {
            ToolOutcome::Error { message } => assert_eq!(message, "boom"),
            other => panic!("expected Error passthrough, got {other:?}"),
        }
        reset_shared_budget(200_000, 0);
    }

    #[tokio::test]
    async fn test_apply_budget_slice_passes_through_failure_unchanged() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(1000, 950);
        let outcome = ToolOutcome::Failure(crate::shared::ToolError::Cancelled);
        let result = apply_budget_slice(outcome, &shared_budget(), &shared_store());
        assert!(matches!(result, ToolOutcome::Failure(_)));
        reset_shared_budget(200_000, 0);
    }

    #[tokio::test]
    async fn test_apply_budget_slice_file_content_over_sliced() {
        let _guard = shared_budget_test_lock().lock().await;
        clear_sliced_listeners();
        reset_shared_budget(1000, 900);
        let big = "z".repeat(10_000);
        let outcome = ToolOutcome::FileContent {
            path: std::path::PathBuf::from("/tmp/x.rs"),
            content: big,
            truncated: false,
        };
        let result = apply_budget_slice(outcome, &shared_budget(), &shared_store());
        match result {
            ToolOutcome::FileContent {
                content, truncated, ..
            } => {
                assert!(truncated, "sliced file content must set truncated");
                assert!(content.len() < 10_000);
            }
            other => panic!("expected FileContent, got {other:?}"),
        }
        reset_shared_budget(200_000, 0);
        clear_sliced_listeners();
    }

    #[tokio::test]
    async fn test_apply_budget_slice_file_content_under_budget_passes_through() {
        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(200_000, 0);
        let outcome = ToolOutcome::FileContent {
            path: std::path::PathBuf::from("/tmp/x.rs"),
            content: "small".into(),
            truncated: false,
        };
        let result = apply_budget_slice(outcome, &shared_budget(), &shared_store());
        match result {
            ToolOutcome::FileContent {
                content, truncated, ..
            } => {
                assert_eq!(content, "small");
                assert!(!truncated);
            }
            other => panic!("expected FileContent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_all_budget_tools_returns_seven_tools() {
        let tools = all_budget_tools(&shared_budget(), &shared_store(), test_stratum_store());
        assert_eq!(tools.len(), 7);
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(names.contains(&"budget_status"));
        assert!(names.contains(&"budget_set"));
        assert!(names.contains(&"budget_compact"));
        assert!(names.contains(&"store_get"));
        assert!(names.contains(&"config_validate"));
        assert!(names.contains(&"report"));
        assert!(names.contains(&"self_check"));
    }

    #[tokio::test]
    async fn test_budget_hooks_returns_four_hooks() {
        let hooks = budget_hooks(&shared_budget());
        assert_eq!(hooks.len(), 4);
        let events: Vec<&str> = hooks.iter().map(|h| h.event()).collect();
        assert!(events.contains(&"session-start"));
        assert!(events.contains(&"post-tool-bash"));
        assert!(events.contains(&"post-tool-write_file"));
        assert!(events.contains(&"pre-compact"));
    }

    // ── WO 8.6 coordination tests ──────────────────────────────────────

    /// Listener dispatch: a registered listener that returns
    /// `Some(replacement)` replaces the sliced display and the
    /// returned `ToolOutcome` carries the replacement. Verifies
    /// the `BudgetSlicedEvent` carries the original size, the
    /// sliced size, the offload key, and the sliced display
    /// (the listener needs all of them to make a decision).
    #[tokio::test]
    async fn test_apply_budget_slice_dispatches_to_sliced_listener() {
        let _guard = shared_budget_test_lock().lock().await;
        clear_sliced_listeners();
        reset_shared_budget(1000, 900);
        // Register a listener that returns a known short string.
        // We do NOT exercise Stratum here — the test only verifies
        // the budget's dispatch surface.
        let captured: std::sync::Arc<std::sync::Mutex<Option<BudgetSlicedEvent>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        register_sliced_listener(std::sync::Arc::new(move |event: BudgetSlicedEvent| {
            *captured_clone.lock().unwrap() = Some(event.clone());
            Some(format!("compressed:{}", event.sliced_size))
        }));
        let big = "q".repeat(10_000);
        let outcome = ToolOutcome::Success { content: big };
        let sliced = apply_budget_slice(outcome, &shared_budget(), &shared_store());
        match sliced {
            ToolOutcome::Success { content } => {
                assert!(
                    content.starts_with("compressed:"),
                    "sliced Success must carry the listener replacement, got: {content:?}"
                );
            }
            other => panic!("expected sliced Success, got {other:?}"),
        }
        let event = captured
            .lock()
            .unwrap()
            .clone()
            .expect("listener received event");
        assert_eq!(event.original_size, 10_000);
        assert!(event.sliced_size > 0 && event.sliced_size < 10_000);
        assert_eq!(event.key.len(), 24, "key must be the 24-hex content key");
        assert!(
            event.sliced_display.contains("<<kf-budget:slice:"),
            "sliced_display must carry the slice marker, got: {:?}",
            event.sliced_display
        );
        clear_sliced_listeners();
    }

    /// Auto-escalation: when budget is `Approaching` and Stratum
    /// is in `Lite`, `apply_budget_slice` escalates the session
    /// mode to `Full`. Gated by the `stratum` feature because the
    /// escalation target lives in the stratum module.
    #[cfg(feature = "stratum")]
    #[tokio::test]
    async fn test_apply_budget_slice_auto_escalates_lite_to_full_on_approaching() {
        use crate::session::stratum::{current_session_mode, set_session_mode};
        use kf_compress_core::mode::Mode;

        let _guard = shared_budget_test_lock().lock().await;
        clear_sliced_listeners();
        reset_shared_budget(1000, 850);
        // Force Approaching (850/1000 = 0.85 ≥ 0.8) and seed the
        // session mode as Lite.
        set_session_mode(Mode::Lite);
        assert_eq!(current_session_mode(), Mode::Lite);
        // Run a slice path. The result content doesn't matter
        // for escalation — it triggers on state == Approaching.
        let big = "x".repeat(10_000);
        let _ = apply_budget_slice(
            ToolOutcome::Success { content: big },
            &shared_budget(),
            &shared_store(),
        );
        assert_eq!(
            current_session_mode(),
            Mode::Full,
            "Approaching budget must auto-escalate Stratum from Lite to Full"
        );
        // Reset to default and clean up listeners.
        set_session_mode(Mode::Full);
        clear_sliced_listeners();
    }

    /// `PreCompactHook` with budget pressure must escalate the
    /// Stratum session mode. Idempotent: re-running when the
    /// mode is already `Full` is a no-op.
    #[cfg(feature = "stratum")]
    #[tokio::test]
    async fn test_pre_compact_hook_runs_stratum_compression() {
        use crate::session::stratum::{current_session_mode, set_session_mode};
        use kf_compress_core::mode::Mode;

        let _guard = shared_budget_test_lock().lock().await;
        reset_shared_budget(1000, 850);
        // Seed session mode as Lite.
        set_session_mode(Mode::Lite);
        assert_eq!(current_session_mode(), Mode::Lite);

        let hook = PreCompactHook {
            budget: shared_budget(),
        };
        let ctx = HookContext {
            event: "pre-compact".into(),
            compact_stats: Some(crate::session::hooks::CompactHookStatsData {
                message_count: 10,
                preserve_recent: 5,
                original_count: 20,
                result_count: 12,
                dropped_tool_results: 0,
                condensed_assistant_turns: 1,
                summarised_messages: 0,
                strategy: "summarize".into(),
            }),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), Ok(()));
        assert_eq!(
            current_session_mode(),
            Mode::Full,
            "PreCompactHook with Approaching budget must escalate Stratum from Lite to Full"
        );
        // Idempotent: a second PreCompactHook with budget still
        // Approaching must remain at Full.
        assert_eq!(hook.handle(&ctx), Ok(()));
        assert_eq!(current_session_mode(), Mode::Full);

        // Reset.
        set_session_mode(Mode::Full);
        reset_shared_budget(200_000, 0);
    }

    // ponytail: pinned invariant — listeners accumulate but only the
    // first Some-returning listener is called. This tests the safety
    // property that allows append-only SLICED_LISTENERS to be
    // acceptable without bounded eviction.
    #[tokio::test]
    async fn test_sliced_listeners_duplicate_registration_is_safe() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let _guard = shared_budget_test_lock().lock().await;
        clear_sliced_listeners();

        let call_count = Arc::new(AtomicU64::new(0));
        register_sliced_listener(Arc::new({
            let cc = call_count.clone();
            move |_event: BudgetSlicedEvent| {
                cc.fetch_add(1, Ordering::Relaxed);
                Some("winner".to_string())
            }
        }));
        // Duplicate the same logical listener — simulates a plugin reload.
        register_sliced_listener(Arc::new({
            let cc = call_count.clone();
            move |_event: BudgetSlicedEvent| {
                cc.fetch_add(1, Ordering::Relaxed);
                Some("shadowed".to_string())
            }
        }));

        let event = BudgetSlicedEvent {
            original_size: 100,
            sliced_size: 50,
            key: "k".into(),
            sliced_display: "d".into(),
        };
        let result = dispatch_sliced(event);
        assert_eq!(result.as_deref(), Some("winner"));
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            1,
            "only the first listener must fire; duplicates are shadowed"
        );

        clear_sliced_listeners();
    }
}
