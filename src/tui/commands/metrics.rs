//! `/metrics` slash command — show operational metrics summary.
//!
//! Prints counts for tool calls, verifier verdicts, turns, and approval
//! decisions from the append-only NDJSON metrics log.

use crate::shared::metrics::{format_summary, summarize};

/// Handle the `/metrics` slash command.
pub fn handle_metrics_command() -> String {
    let summary = summarize();
    format_summary(&summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_command_returns_summary_lines() {
        let out = handle_metrics_command();
        assert!(out.contains("Metrics summary"));
        assert!(out.contains("turns:"));
        assert!(out.contains("tool calls:"));
        assert!(out.contains("verifiers:"));
        assert!(out.contains("approvals:"));
    }
}
