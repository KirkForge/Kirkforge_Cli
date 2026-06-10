//! Stub module — deepseek's in-flight work referenced `compact` from
//! `session/executor.rs` (and re-exported it from `session/prompt/mod.rs`)
//! but the full implementation wasn't included in the saved work.
//! Added at push time with a stub that returns the shape
//! `executor.rs` expects, so `cargo check` passes. The real compaction
//! would drop old tool results, condense old assistant turns, and emit
//! a summary; the TUI's `/compact` slash command is the user-facing
//! entry point.

#![allow(dead_code, unused_imports)]
use crate::shared::Message;

#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub new_messages: Vec<Message>,
    pub dropped_tool_results: usize,
    pub condensed_assistant_turns: usize,
    pub original_count: usize,
    pub compacted_count: usize,
}

/// Stub: returns the input list unchanged with zero counts. The real
/// implementation would do the actual summarization work.
pub fn compact(messages: &[Message]) -> CompactionResult {
    CompactionResult {
        new_messages: messages.to_vec(),
        dropped_tool_results: 0,
        condensed_assistant_turns: 0,
        original_count: messages.len(),
        compacted_count: messages.len(),
    }
}
