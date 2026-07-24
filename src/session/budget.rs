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
    aggregate_sessions, filter_lines, format_summary_line, BudgetState, ConfigFile,
    InMemoryOffloadStore, OffloadStore, Paths, TokenBudget,
};
use std::sync::{Arc, Mutex, OnceLock};

type SharedBudget = Arc<Mutex<TokenBudget>>;
type SharedStore = Arc<dyn OffloadStore>;

static SHARED_BUDGET: OnceLock<SharedBudget> = OnceLock::new();

fn shared_budget() -> SharedBudget {
    SHARED_BUDGET
        .get_or_init(|| Arc::new(Mutex::new(TokenBudget::default())))
        .clone()
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
    let store: SharedStore = Arc::new(InMemoryOffloadStore::new());

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
