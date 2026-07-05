//! `/fork` and `/resume` slash-command handlers.
//!
//! These are session-forking primitives: a fork is a snapshot of the
//! conversation log at a chosen message index, persisted to disk as
//! its own NDJSON file, and addressable by a short id. `/resume`
//! swaps the executor's live conversation log for the fork's log so
//! the user can explore an alternate branch.
//!
//! With the daemon follow-up, `/resume` with no arguments opens the
//! recent-session picker overlay and lets the user resume any of the
//! last 5 sessions tracked by the session daemon.
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
        Ok((conv_log, _outcome)) => {
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
                Err(e) => format!("Error creating fork: {e}"),
            }
        }
        Err(e) => format!("Error opening conversation log: {e}"),
    }
}

/// Handle `/resume` command: switch the active session to a fork, or
/// open the recent-session picker when called with no arguments.
///
/// Usage:
///   `/resume`            — pick a recent session from the daemon's list
///   `/resume <fork-id>` — resume a session fork
pub async fn handle_resume_command(
    args: &str,
    state: &mut AppState,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
) -> String {
    let trimmed = args.trim();

    if trimmed.is_empty() {
        // ── No args: show the recent-session picker overlay. ──
        return match load_recent_sessions_for_picker().await {
            Ok(sessions) if sessions.is_empty() => {
                "No recent sessions found. Use `/resume <fork-id>` to resume a fork, or `/fork list` to see forks.".into()
            }
            Ok(sessions) => {
                state.session_picker =
                    Some(crate::tui::components::session_picker::SessionPicker::new(sessions));
                "Select a recent session to resume (Enter to confirm, q/Esc to cancel).".into()
            }
            Err(e) => format!("Could not load recent sessions: {e}"),
        };
    }

    // ── With args: treat as a fork id. ──
    let fm = match state.fork_manager.as_mut() {
        Some(fm) => fm,
        None => return "No fork manager available.".into(),
    };

    // Verify the fork exists
    let fork = match fm.get(trimmed) {
        Some(f) => f.clone(),
        None => {
            let available: Vec<&str> = fm.list().iter().map(|f| f.id.as_str()).collect();
            return format!(
                "Fork '{}' not found. Available forks: [{}]",
                trimmed,
                available.join(", ")
            );
        }
    };

    // Open the fork's conversation log and resume it.
    let fork_log = match ConversationLog::open(fork.path.clone()) {
        Ok((log, _outcome)) => log,
        Err(e) => return format!("Error opening fork log: {e}"),
    };
    resume_conversation_log(fork_log, state, resume_tx).await
}

/// Resume the TUI and executor into a new conversation log.
///
/// Shared helper used by fork resumption, the recent-session picker,
/// and any other path that needs to swap the live conversation. The
/// TUI's message list is reloaded from the log *before* the swap is
/// sent to the executor so the user sees the history even if the
/// executor has already shut down.
pub async fn resume_conversation_log(
    log: ConversationLog,
    state: &mut AppState,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
) -> String {
    // Reload the TUI's display list from the persisted history BEFORE
    // sending the log to the executor. If the executor swap fails for
    // any reason (e.g. it has shut down), the TUI will at least show the
    // history and the user can see what they were resuming into.
    let entries = messages_to_entries(log.all());
    let entry_count = entries.len();

    // Capture identity from the log before we move it into the channel.
    let new_log_path = log.path().clone();
    let new_id = new_log_path
        .file_stem()
        .and_then(|f| f.to_str())
        .map(|s| s.trim_end_matches(".conv").to_string())
        .unwrap_or_else(|| state.session_id.clone());

    // Send the log to the executor (swaps in-place)
    if resume_tx.send(log).is_err() {
        return "Error: executor is not running.".into();
    }

    // Reload TUI state from the new conversation. We clear everything
    // that came from the OLD session so the user doesn't see stale
    // indicators from a previous turn:
    //
    //   - messages: replaced with the new history
    //   - thinking_buffer: any in-flight thinking text is for the OLD session
    //   - pending_approval: any approval prompt is for a tool call in the OLD session
    //   - expanded_tools / notified_jobs: indices/ids from the OLD session are meaningless
    //   - last_turn_prompt_tokens: 0; the executor will emit a fresh CostStats on the next turn
    //   - tokens_sent / tokens_received / cumulative_cost: running counters reset
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

    // Update session identity to the new log path. We keep the original
    // session_id as a parent marker so forks created from this resumed
    // branch still record their provenance.
    state.session_id = format!("{new_id} (resumed)");
    state.log_path = Some(new_log_path.clone());

    // Touch the daemon so this resumed session is now the most recent.
    // `try_touch` logs its own errors; no additional handling needed here.
    crate::daemon::client::try_touch(&new_id, new_log_path.clone()).await;

    format!(
        "✅ Resumed session '{new_id}' — {entry_count} messages reloaded. Type a message to continue.",
    )
}

/// Load the recent sessions tracked by the daemon, falling back to an
/// empty list if the daemon is not reachable.
async fn load_recent_sessions_for_picker(
) -> anyhow::Result<Vec<crate::session::session_index::SessionEntry>> {
    match crate::daemon::client::try_list_recent().await? {
        Some(sessions) => Ok(sessions),
        None => Ok(Vec::new()),
    }
}
