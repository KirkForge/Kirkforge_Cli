//! Internal executor types.

use crate::shared::ToolInvocation;

pub(crate) enum IterationOutcome {
    ToolCalls(Vec<ToolInvocation>),

    Finished(crate::shared::FinishReason),

    ParseError,
}
pub(crate) enum ApprovalDecision {
    Approved,
    Denied { reason: String },
    AlwaysApproved,
}
/// Marker emitted by the model at the end of a plan-mode turn. The
/// executor detects this string in the assistant content and surfaces a
/// `TurnEvent::PlanComplete` so the TUI can ask the user to approve
/// exiting plan mode.
pub(crate) const PLAN_COMPLETE_MARKER: &str = "## Plan Complete — ready to implement";

/// Statistics passed to compaction lifecycle hooks (`pre-compact` / `post-compact`).
#[derive(Debug, Clone, Copy)]
pub struct CompactHookStats {
    pub(crate) message_count: usize,
    pub(crate) preserve_recent: usize,
    pub(crate) original_count: usize,
    pub(crate) result_count: usize,
    pub(crate) dropped_tool_results: usize,
    pub(crate) condensed_assistant_turns: usize,
    pub(crate) summarised_messages: usize,
    pub(crate) tokens_before: usize,
    pub(crate) tokens_after: usize,
    pub(crate) strategy: &'static str,
}
#[derive(Debug)]
pub enum TurnEvent {
    Token(String),
    Thinking(String),
    ToolStart {
        name: String,
        args: serde_json::Value,
    },
    ToolResult {
        name: String,
        output: String,
        /// Whether the tool call actually succeeded. `false` covers all
        /// denial paths (path guard, deny list, read-before-edit gate,
        /// approval-deny, dangerous-command block) as well as the tool
        /// itself returning a `ToolOutcome::Error`. The non-interactive
        /// JSON summary uses this to populate the `success` field on
        /// `ToolCallRecord` truthfully (was hardcoded `vec![]` in the
        /// previous implementation — see GPT 5.5 review finding #13).
        success: bool,
    },
    Error(String),
    Verification {
        message: String,
        success: bool,
    },
    CostStats {
        prompt_tokens: usize,
        completion_tokens: usize,
        turn_cost: f64,
        cumulative_cost: f64,
    },

    /// Prompt-cache performance for the turn. Emitted when the adapter
    /// reports cache-read tokens so the TUI/status bar can surface KV-cache
    /// hit counts and verify that the prompt cache stem is actually being
    /// reused by the provider.
    CacheStats {
        cached_tokens: usize,
        prompt_tokens: usize,
        /// Estimated size of the stable prompt-cache stem (system prompt +
        /// tool definitions) in tokens. Useful for tuning cache-hit rates.
        stem_tokens: usize,
    },

    CompactionReport {
        new_messages: Vec<crate::shared::Message>,
        dropped_tool_results: usize,
        condensed_assistant_turns: usize,
        original_count: usize,
        compacted_count: usize,
        tokens_before: usize,
        tokens_after: usize,
    },

    /// Emitted when the assistant's response contains the plan-complete
    /// marker while plan mode is active. The TUI should prompt the user
    /// to approve exiting plan mode (e.g. via `/implement`).
    PlanComplete,

    /// Emitted when the conversation log was corrupt on open and the
    /// executor recovered from the most recent intact checkpoint.
    /// Carries the number of restored messages so the TUI can show a
    /// concise status line.
    Recovered {
        /// Number of messages restored from checkpoint.
        messages: usize,
    },

    /// Progress of an asynchronous Ollama model pull triggered by
    /// `/model <name>` when the model is missing locally. Rendered
    /// in the TUI as a live progress bar.
    PullProgress {
        /// Human-readable status string from the Ollama `/api/pull`
        /// stream (e.g. "pulling manifest", "downloading").
        status: String,
        /// Completed bytes so far; `None` when the server has not
        /// reported a numeric value yet.
        completed: Option<u64>,
        /// Total bytes to download; `None` when the total is unknown.
        total: Option<u64>,
    },
}
