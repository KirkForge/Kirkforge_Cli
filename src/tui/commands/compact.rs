//! `/compact` slash-command handler.
//!
//! The actual compaction work happens asynchronously in the executor
//! (it calls `PromptBuilder::compact` and rewrites the NDJSON log
//! atomically). We just kick it off by sending `()` on `compact_tx`
//! and return a status string immediately. When the executor finishes,
//! it emits `TurnEvent::CompactionReport`, which the TUI event loop
//! consumes to rebuild the display list and append a 🧹 status message.

use tokio::sync::mpsc;

/// Handle `/compact` command: trigger a user-driven compaction of the
/// conversation history.
///
/// `args` is accepted for forward-compatibility (e.g. `/compact --force`
/// to skip the "no recent tool results to compact" short-circuit) but is
/// currently ignored. Keeps the signature symmetric with
/// `handle_fork_command` / `handle_resume_command`.
pub async fn handle_compact_command(args: &str, compact_tx: &mpsc::UnboundedSender<()>) -> String {
    // Reserved for future flags; explicit `_args` naming keeps clippy happy
    // and signals the intent without forcing a `let _ = args;` no-op.
    let _ = args;

    match compact_tx.send(()) {
        Ok(()) => "🧹 Compaction requested. The executor will rewrite the conversation log and the chat view will refresh when it finishes.".into(),
        Err(e) => format!("❌ Failed to send compact request to executor: {}", e),
    }
}
