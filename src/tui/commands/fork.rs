//! `/fork` and `/resume` slash-command handlers.
//!
//! These are session-forking primitives: a fork is a snapshot of the
//! conversation log at a chosen message index, persisted to disk as
//! its own NDJSON file, and addressable by a short id. `/resume`
//! swaps the executor's live conversation log for the fork's log so
//! the user can explore an alternate branch.
//!
//! Both handlers are pure: they take `&mut AppState` and a channel
//! sender (or nothing), and return a display string. The TUI event
//! loop is responsible for pushing the string into `state.messages`.

use crate::session::conversation::ConversationLog;
use crate::tui::app::AppState;
use tokio::sync::mpsc;

use super::messages_to_entries;

/// Handle `/fork` command: list forks or create a new one.
pub async fn handle_fork_command(args: &str, state: &mut AppState) -> String {
    let fm = match state.fork_manager.as_mut() {
        Some(fm) => fm,
        None => return "No fork manager available (session not initialized).".into(),
    };

    let trimmed = args.trim();
    if trimmed.eq_ignore_ascii_case("list") || trimmed.is_empty() {
        let forks = fm.list();
        if forks.is_empty() {
            return "No forks created yet. Use `/fork <label> [count]` to create one.".into();
        }
        let mut out = "Session forks:\n".to_string();
        for f in forks {
            out.push_str(&format!(
                "  {} — {} (fork point: {}, created: {})\n",
                f.id, f.label, f.fork_point, f.created_at
            ));
        }
        return out;
    }

    // Parse: label [count]
    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    if parts.is_empty() || parts[0].is_empty() {
        return "Usage: /fork list | /fork <label> [count]".into();
    }

    let label = parts[0];

    // Build a fake ConversationLog from our messages for fork creation
    // We use the conversation log path stored in state
    let log_path = match &state.log_path {
        Some(p) => p.clone(),
        None => return "No log path available. Cannot create fork.".into(),
    };

    // Open the conversation log to read the latest state
    match ConversationLog::open(log_path) {
        Ok(conv_log) => {
            // Fork point: -1 (end) by default, or parse an optional count
            let fork_point: i64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);

            match fm.create_fork(label, &conv_log, fork_point) {
                Ok(fork) => format!(
                    "✅ Created fork {} — \"{}\" at message #{} (path: {})",
                    fork.id,
                    fork.label,
                    fork.fork_point,
                    fork.path.display()
                ),
                Err(e) => format!("Error creating fork: {}", e),
            }
        }
        Err(e) => format!("Error opening conversation log: {}", e),
    }
}

/// Handle `/resume` command: switch the active session to a fork.
///
/// Usage: `/resume <fork-id>`
pub async fn handle_resume_command(
    args: &str,
    state: &mut AppState,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
) -> String {
    let fork_id = args.trim();
    if fork_id.is_empty() {
        return "Usage: /resume <fork-id>\nUse `/fork list` to see available forks.".into();
    }

    let fm = match state.fork_manager.as_mut() {
        Some(fm) => fm,
        None => return "No fork manager available.".into(),
    };

    // Verify the fork exists
    let fork = match fm.get(fork_id) {
        Some(f) => f.clone(),
        None => {
            let available: Vec<&str> = fm.list().iter().map(|f| f.id.as_str()).collect();
            return format!(
                "Fork '{}' not found. Available forks: [{}]",
                fork_id,
                available.join(", ")
            );
        }
    };

    // Open the fork's conversation log
    match ConversationLog::open(fork.path.clone()) {
        Ok(fork_log) => {
            // Reload the TUI's display list from the fork's persisted
            // history BEFORE sending the log to the executor. If the
            // executor swap fails for any reason (e.g. it has shut
            // down), the TUI will at least show the fork's history
            // and the user can see what they were resuming into.
            let entries = messages_to_entries(fork_log.all());
            let entry_count = entries.len();

            // Send the fork log to the executor (swaps in-place)
            if resume_tx.send(fork_log).is_err() {
                return "Error: executor is not running.".into();
            }

            // Reload TUI state from the fork's conversation. We clear
            // everything that came from the OLD session so the user
            // doesn't see stale indicators from a previous turn:
            //
            //   - messages: replaced with the fork's history
            //   - thinking_buffer: any in-flight thinking text is
            //     for the OLD session
            //   - pending_approval: any approval prompt is for a
            //     tool call in the OLD session; the fork's history
            //     may not even have a counterpart
            //   - expanded_tools / notified_jobs: indices/ids from
            //     the OLD session are meaningless
            //   - last_turn_prompt_tokens: 0; the executor will
            //     emit a fresh CostStats on the next turn of the
            //     resumed session
            //   - tokens_sent / tokens_received / cumulative_cost:
            //     these are running-session counters; the resumed
            //     session is logically a new session for accounting
            //     purposes (a re-fork from the resumed session will
            //     record `parent_session` as the original anyway)
            state.messages = entries;
            state.thinking_buffer.clear();
            state.pending_approval = None;
            state.expanded_tools.clear();
            state.notified_jobs.clear();
            state.last_turn_prompt_tokens = 0;
            state.tokens_sent = 0;
            state.tokens_received = 0;
            state.cumulative_cost = 0.0;
            state.turn_cost = 0.0;

            // Update session identity
            state.session_id = format!("{} (fork: {})", state.session_id, fork.id);
            state.log_path = Some(fork.path);

            format!(
                "✅ Resumed fork '{}' — \"{}\" ({} messages reloaded). Type a message to continue.",
                fork.id, fork.label, entry_count,
            )
        }
        Err(e) => format!("Error opening fork log: {}", e),
    }
}
