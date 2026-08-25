//! Internal executor types.

use crate::shared::ToolInvocation;

/// Typed outcome of a verifier pass, mirroring the relevant `Verdict`
/// cases the correction loop surfaces. Carries the discriminant that
/// `TurnEvent::Verification { success: bool }` previously flattened,
/// so a consumer can tell "verifier confirmed clean" from "verifier was
/// skipped (tool not available)" from "verifier auto-fixed a problem".
/// WO 45.36.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationOutcome {
    /// Verifier confirmed clean — no issues found.
    Clean,
    /// Verifier skipped (e.g. tool not available, no-op event).
    Skipped,
    /// Verifier found and auto-applied a fix (formatter or text replacement).
    Fixed,
    /// Verifier returned an informational suggestion — no fix applied.
    Suggestion,
    /// Verifier reported an issue it could not fix, or an auto-fix failed.
    Failed,
}

impl VerificationOutcome {
    /// True for outcomes a consumer should treat as "not a verifier
    /// failure" — `Clean`, `Skipped`, `Fixed`, `Suggestion`. `Failed` is
    /// the only failure outcome. Preserves the prior `success: bool`
    /// partition exactly (Skipped was `success: true` before WO 45.36).
    pub fn is_success(self) -> bool {
        !matches!(self, VerificationOutcome::Failed)
    }

    /// Stable lowercase wire label (used in the StreamJson line).
    pub fn label(self) -> &'static str {
        match self {
            VerificationOutcome::Clean => "clean",
            VerificationOutcome::Skipped => "skipped",
            VerificationOutcome::Fixed => "fixed",
            VerificationOutcome::Suggestion => "suggestion",
            VerificationOutcome::Failed => "failed",
        }
    }
}

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
        success: bool,
    },
    Error(String),
    Verification {
        message: String,
        outcome: VerificationOutcome,
        file: Option<std::path::PathBuf>,
        line: Option<u32>,
    },
    CostStats {
        prompt_tokens: usize,
        completion_tokens: usize,
        turn_cost: f64,
        cumulative_cost: f64,
    },

    CacheStats {
        cached_tokens: usize,
        prompt_tokens: usize,
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

    PlanComplete,

    Recovered {
        messages: usize,
    },

    PullProgress {
        status: String,
        completed: Option<u64>,
        total: Option<u64>,
    },

    DoomLoopDetected {
        count: usize,
        tool: String,
        last_error: String,
    },

    /// Emitted when the doom-loop circuit breaker fires — the
    /// cumulative doom-loop hit count has reached `doom_loop_max_hits`.
    /// `action` is either `"auto_plan_mode"` (switched to plan mode)
    /// or `"halt"` (already in plan mode, turn halted).
    DoomLoopRemediation {
        action: String,
        hits: usize,
    },

    /// Emitted when a `FinishReason::Length` continuation round starts.
    /// `round` is 1-indexed (first continuation = 1), `max` is the
    /// configured `max_continuation_rounds`. The TUI renders this as
    /// "⟳ 3/5" in the status bar.
    ContinuationRound {
        round: usize,
        max: usize,
    },

    /// A chunk of PTY output from a running interactive bash command.
    /// Emitted incrementally while the command executes so the TUI can
    /// stream it into the tool-result card. The full output is still
    /// delivered once via `ToolResult` when the command finishes.
    BashPartialOutput(String),

    /// Emitted after a post-turn auto-extraction stored facts in the
    /// memory store. `count` is the total number of facts now in the
    /// store; `turn` is the executor turn counter. The TUI renders
    /// this in the status bar so the operator sees memory grow live.
    MemoryExtracted {
        count: usize,
        turn: u64,
    },

    /// Terminal event: the turn is fully complete (model finished, tools
    /// ran, continuations exhausted, or cancelled). Emitted exactly once
    /// at the end of `run_turn` on every Ok exit path. The TUI uses this
    /// to clear `is_generating` / `streaming` unconditionally — decoupled
    /// from `CostStats`, which is only emitted when the provider supplies
    /// usage data. Without this, providers that send `Done { usage: None }`
    /// leave the TUI stuck "generating" forever.
    TurnComplete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_complete_marker_is_nonempty() {
        assert!(PLAN_COMPLETE_MARKER.starts_with("## "));
    }

    #[test]
    fn compact_hook_stats_fields() {
        let stats = CompactHookStats {
            message_count: 10,
            preserve_recent: 2,
            original_count: 20,
            result_count: 8,
            dropped_tool_results: 3,
            condensed_assistant_turns: 1,
            summarised_messages: 4,
            strategy: "heuristic",
        };
        assert_eq!(stats.message_count, 10);
        assert_eq!(stats.preserve_recent, 2);
        assert_eq!(stats.original_count, 20);
        assert_eq!(stats.result_count, 8);
        assert_eq!(stats.dropped_tool_results, 3);
        assert_eq!(stats.condensed_assistant_turns, 1);
        assert_eq!(stats.summarised_messages, 4);
        assert_eq!(stats.strategy, "heuristic");
    }

    #[test]
    fn compact_hook_stats_is_copy() {
        let stats = CompactHookStats {
            message_count: 1,
            preserve_recent: 0,
            original_count: 1,
            result_count: 1,
            dropped_tool_results: 0,
            condensed_assistant_turns: 0,
            summarised_messages: 0,
            strategy: "exact",
        };
        let _copy = stats;
        let _copy2 = stats;
    }

    #[test]
    fn iteration_outcome_variants() {
        let tool_calls = IterationOutcome::ToolCalls(vec![]);
        assert!(matches!(tool_calls, IterationOutcome::ToolCalls(_)));

        let finished = IterationOutcome::Finished(crate::shared::FinishReason::Stop);
        assert!(matches!(finished, IterationOutcome::Finished(_)));

        let parse_err = IterationOutcome::ParseError;
        assert!(matches!(parse_err, IterationOutcome::ParseError));
    }

    #[test]
    fn approval_decision_variants() {
        let approved = ApprovalDecision::Approved;
        assert!(matches!(approved, ApprovalDecision::Approved));

        let denied = ApprovalDecision::Denied {
            reason: "nope".into(),
        };
        assert!(matches!(denied, ApprovalDecision::Denied { .. }));

        let always = ApprovalDecision::AlwaysApproved;
        assert!(matches!(always, ApprovalDecision::AlwaysApproved));
    }
}
