//! `/sessions` slash command — list, prune, and delete prior sessions.
//!
//! Review.md gap #3: the user wants a way to see what sessions
//! exist on disk and clean up old ones. The underlying data is in
//! `~/.local/share/kirkforge/sessions/*.conv.ndjson`; this command
//! is the human interface.
//!
//! Subcommands:
//! - `/sessions`              — list all sessions (newest first)
//! - `/sessions list`         — alias for the bare command
//! - `/sessions prune [N]`    — delete the oldest N, keep 10 most recent
//! - `/sessions prune N keep K` — explicit form
//! - `/sessions delete <id>`  — delete a single session by id or prefix

use crate::session::session_index;
use crate::tui::app::AppState;

/// Handle `/sessions [list|search|prune|delete]`.
pub fn handle_sessions_command(args: &str, _state: &mut AppState) -> String {
    let args = args.trim();
    let mut parts = args.split_whitespace();
    let sub = parts.next().unwrap_or("list");

    match sub {
        "list" | "" => list_sessions_text(),
        "search" => match parts.next() {
            Some(query) => search_sessions_text(query),
            None => "Usage: /sessions search <query>".to_string(),
        },
        "prune" => {
            // /sessions prune [N] [keep K]
            let n: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(5);
            let keep: usize = match parts.next() {
                Some("keep") => parts.next().and_then(|s| s.parse().ok()).unwrap_or(10),
                Some(k) => k.parse().unwrap_or(10),
                None => 10,
            };
            prune_sessions_text(n, keep)
        }
        "delete" | "rm" => match parts.next() {
            Some(id) => delete_session_text(id),
            None => "Usage: /sessions delete <id-or-prefix>".to_string(),
        },
        "help" | "--help" | "-h" => SESSIONS_HELP.to_string(),
        other => format!("Unknown /sessions subcommand: {other}\n\n{SESSIONS_HELP}"),
    }
}

const SESSIONS_HELP: &str = "/sessions — list, search, prune, and delete saved sessions

Usage:
  /sessions                  List all sessions (newest first)
  /sessions list             Same as above
  /sessions search <query>  Search by id, date, or message count
  /sessions prune [N] [keep K]
                             Delete the oldest N sessions, keeping
                             the K most recent. Defaults: N=5, K=10.
  /sessions delete <id>      Delete a single session by id or prefix

Sessions are stored in ~/.local/share/kirkforge/sessions/<id>.conv.ndjson.
Each line in the NDJSON is a JSON message in the conversation.

Tip: combine with /resume <id> to load a prior session into the
current TUI.";

/// Format the search results as a multi-line table.
fn search_sessions_text(query: &str) -> String {
    match session_index::search_sessions(query) {
        Ok(entries) if entries.is_empty() => {
            format!("No sessions matching '{query}'.")
        }
        Ok(entries) => format_session_table(&entries,
            &format!("Search results for '{query}' ({} total):\n", entries.len()),
        ),
        Err(e) => format!("Error searching sessions: {e}"),
    }
}

/// Format the session list as a multi-line table.
fn list_sessions_text() -> String {
    match session_index::list_sessions() {
        Ok(entries) if entries.is_empty() => {
            "No sessions found in ~/.local/share/kirkforge/sessions/.".to_string()
        }
        Ok(entries) => format_session_table(
            &entries,
            &format!("Sessions ({} total, newest first):\n", entries.len()),
        ),
        Err(e) => format!("Error listing sessions: {e}"),
    }
}

fn format_session_table(entries: &[session_index::SessionEntry], header: &str) -> String {
    let mut out = header.to_string();
    out.push_str(&format!(
        "  {:<30}  {:<8}  {:<20}  {}\n",
        "ID", "MSGS", "STARTED", "SIZE"
    ));
    for e in entries {
        let size = human_size(e.size_bytes);
        let started = short_ts(&e.started_at);
        out.push_str(&format!(
            "  {:<30}  {:<8}  {:<20}  {}\n",
            truncate_id(&e.id, 30),
            e.message_count,
            started,
            size,
        ));
    }
    out.push_str("\nUse /sessions delete <id> to remove one, /sessions prune to clean up.");
    out
}

fn prune_sessions_text(delete_count: usize, keep: usize) -> String {
    match session_index::prune_oldest(keep, delete_count) {
        Ok(deleted) if deleted.is_empty() => {
            format!(
                "Nothing to prune. (Have ≤ {} + {} = {} sessions; would not remove anything.)",
                keep,
                delete_count,
                keep + delete_count
            )
        }
        Ok(deleted) => {
            let mut out = format!("Pruned {} session(s):\n", deleted.len());
            for id in &deleted {
                out.push_str(&format!("  ✓ {id}\n"));
            }
            out.push_str(&format!(
                "Kept {keep} most recent. Run /sessions to verify."
            ));
            out
        }
        Err(e) => format!("Error pruning sessions: {e}"),
    }
}

fn delete_session_text(id_or_prefix: &str) -> String {
    // Resolve prefix → exact id (so the user can type
    // "2026-06-10-01" instead of "2026-06-10-session-01").
    let resolved = match session_index::resolve_session_id(id_or_prefix) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return format!(
                "No session found matching '{id_or_prefix}'. Run /sessions to see available ids."
            );
        }
        Err(e) => return format!("Error resolving session id: {e}"),
    };
    let id = resolved
        .file_stem()
        .and_then(|f| f.to_str())
        .unwrap_or(id_or_prefix)
        .trim_end_matches(".conv")
        .to_string();

    match session_index::delete_session(&id) {
        Ok(true) => format!("✓ Deleted session: {id}"),
        Ok(false) => format!("Session '{id}' did not exist (race?)."),
        Err(e) => format!("Error deleting session: {e}"),
    }
}

/// Human-readable byte size: "1.2 KB" / "3.4 MB" / "567 B".
fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Shorten an rfc3339 timestamp to "MM-DD HH:MM" for table density.
fn short_ts(rfc3339: &str) -> String {
    // rfc3339 looks like "2026-06-10T12:34:56-07:00". Take the date
    // and time parts; skip the T.
    if rfc3339.len() >= 16 {
        format!("{} {}", &rfc3339[5..10], &rfc3339[11..16])
    } else {
        rfc3339.to_string()
    }
}

/// Truncate the id for the table column. Just byte-truncate; ids are
/// date+seq ASCII so this is safe.
fn truncate_id(id: &str, max: usize) -> String {
    if id.len() <= max {
        id.to_string()
    } else {
        format!("{}…", &id[..max.saturating_sub(1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `human_size` covers the three bands.
    #[test]
    fn test_human_size_bands() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
    }

    /// `short_ts` slices out the date and time parts of rfc3339.
    #[test]
    fn test_short_ts() {
        assert_eq!(short_ts("2026-06-10T12:34:56-07:00"), "06-10 12:34");
        // Unparseable: passthrough.
        assert_eq!(short_ts("nope"), "nope");
    }

    /// `truncate_id` shortens and adds an ellipsis.
    #[test]
    fn test_truncate_id() {
        assert_eq!(
            truncate_id("2026-06-10-session-01", 30),
            "2026-06-10-session-01"
        );
        assert!(truncate_id("a".repeat(40).as_str(), 10).ends_with('…'));
    }

    /// `/sessions` with no args → "list" subcommand.
    #[test]
    fn test_handle_sessions_no_args_dispatches_to_list() {
        // We can't easily inject a non-empty sessions dir in unit
        // tests without polluting the real data dir, so just verify
        // the help/empty path returns a string (not a panic).
        let mut state = AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::shared::Config::default(),
        )));
        let out = handle_sessions_command("", &mut state);
        // Either "No sessions found" or the table — both are fine.
        assert!(!out.is_empty());
    }

    /// `/sessions help` returns the help text.
    #[test]
    fn test_handle_sessions_help() {
        let mut state = AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::shared::Config::default(),
        )));
        let out = handle_sessions_command("help", &mut state);
        assert!(out.contains("/sessions"));
        assert!(out.contains("prune"));
        assert!(out.contains("delete"));
    }

    /// `/sessions delete` without an id → usage hint.
    #[test]
    fn test_handle_sessions_delete_requires_id() {
        let mut state = AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::shared::Config::default(),
        )));
        let out = handle_sessions_command("delete", &mut state);
        assert!(out.contains("Usage"));
    }

    /// `/sessions foo` → "Unknown subcommand" message that still
    /// surfaces the help text.
    #[test]
    fn test_handle_sessions_unknown_subcommand() {
        let mut state = AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::shared::Config::default(),
        )));
        let out = handle_sessions_command("foo", &mut state);
        assert!(out.contains("Unknown"));
        assert!(out.contains("/sessions"));
    }
}
