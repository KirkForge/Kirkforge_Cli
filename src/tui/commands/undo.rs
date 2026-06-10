//! `/undo` slash command — pop the most recent edit off the undo
//! stack and restore the file.
//!
//! Review.md gap #7: the safety net that makes users trust an AI
//! agent with their code. Without it, the only recourse on a bad
//! edit was `git checkout` — fine for tracked files, useless
//! otherwise.
//!
//! The undo stack lives on the executor (constructed at session
//! start, written to by the `edit_file` and `write_file` tools).
//! The TUI's `/undo` handler needs to reach it, so this command
//! takes an `UndoStackRef` and dispatches via that.

use crate::tui::app::{AppState, ConversationEntry};

/// Handle `/undo` and `/undo list`.
///
/// `/undo` (no args) pops the most recent edit and restores the
/// file. The user sees a system message with what was undone.
///
/// `/undo list` shows the stack contents (read-only).
///
/// `/undo count` is a small convenience: prints the current depth
/// of the stack. Useful for scripts that want to check before
/// running a destructive operation.
pub fn handle_undo_command(args: &str, _state: &mut AppState) -> String {
    // The undo stack is held by the executor, not by AppState.
    // The TUI dispatches slash commands that touch executor state
    // via a side channel; for the v1 implementation of `/undo`,
    // we surface the limitation in the help text and accept the
    // /undo invocation as a no-op with a system message.
    //
    // Wiring this fully requires plumbing the UndoStackRef from
    // the executor through to the keys handler. The M2 commit
    // lays the data and tool plumbing; the TUI-side command is
    // here as a placeholder so the dispatch is wired and
    // discoverable via /help. The actual TUI command is part of
    // a follow-up that also adds /sessions (M3).
    //
    // For now, return a clear message so users aren't confused.
    let _ = args;
    "⚠ /undo is registered, but the TUI command is not yet wired to the executor's undo stack. \
     Use the underlying EditFile/WriteFile tool plumbing (which IS wired) and the snapshot files \
     under `~/.local/share/kirkforge/undo/<session-id>/` for manual recovery. \
     The full TUI command ships in M3 alongside /sessions.".to_string()
}

/// Format a stack of undo entries as a multi-line display string.
/// Used by `/undo list`. Pure function — takes the entries as
/// `Vec<UndoSummary>` (no I/O) so it's unit-testable.
pub fn format_undo_list(entries: &[crate::session::undo::UndoSummary]) -> String {
    if entries.is_empty() {
        return "Undo stack is empty.".to_string();
    }
    let mut out = format!("Undo stack ({} entries, newest last):\n", entries.len());
    for e in entries {
        out.push_str(&format!(
            "  #{:>3}  {:>5}  {:>7} bytes  {}  {}\n",
            e.seq,
            e.kind.as_str(),
            e.snapshot_size,
            e.timestamp.format("%H:%M:%S"),
            e.path.display(),
        ));
    }
    out
}

/// Format the result of an `UndoStack::pop` as a user-facing string.
pub fn format_undo_popped(restored: &crate::session::undo::RestoredOp) -> String {
    let action = if restored.prev_existed {
        format!("restored {}", restored.path.display())
    } else {
        format!("removed newly-created {}", restored.path.display())
    };
    format!("↶ Undo: {} ({})", action, restored.kind.as_str())
}

// We need a stub for `ConversationEntry::new` usage in case future
// versions push system messages from here. The current
// implementation returns a string and lets `keys.rs` push it; that
// keeps the command side effect-free and testable.
#[allow(dead_code)]
fn _entry_anchor(_e: ConversationEntry) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::undo::{UndoKind, UndoSummary};
    use chrono::TimeZone;

    fn entry(seq: u64, kind: UndoKind, path: &str, size: u64) -> UndoSummary {
        UndoSummary {
            seq,
            kind,
            path: std::path::PathBuf::from(path),
            snapshot_size: size,
            timestamp: chrono::Local
                .with_ymd_and_hms(2026, 6, 10, 12, 0, 0)
                .unwrap(),
        }
    }

    /// Empty stack renders a clean "empty" message rather than
    /// a confusing header.
    #[test]
    fn test_format_undo_list_empty() {
        let s = format_undo_list(&[]);
        assert!(s.contains("empty"));
    }

    /// Non-empty stack lists each entry with seq, kind, size,
    /// timestamp, and path. The header shows the count.
    #[test]
    fn test_format_undo_list_with_entries() {
        let s = format_undo_list(&[
            entry(0, UndoKind::Edit, "src/main.rs", 1024),
            entry(1, UndoKind::Write, "src/new.rs", 0),
        ]);
        assert!(s.contains("2 entries"));
        assert!(s.contains("#  0"));
        assert!(s.contains("#  1"));
        assert!(s.contains("src/main.rs"));
        assert!(s.contains("src/new.rs"));
        assert!(s.contains("edit"));
        assert!(s.contains("write"));
    }

    /// `format_undo_popped` distinguishes between "restored" (file
    /// existed before the edit) and "removed" (the edit created
    /// the file). Both are valid undo outcomes; the message
    /// should make the difference clear.
    #[test]
    fn test_format_undo_popped_restored_existing() {
        let r = crate::session::undo::RestoredOp {
            path: std::path::PathBuf::from("src/foo.rs"),
            kind: UndoKind::Edit,
            prev_existed: true,
        };
        let s = format_undo_popped(&r);
        assert!(s.contains("restored"));
        assert!(s.contains("src/foo.rs"));
    }

    #[test]
    fn test_format_undo_popped_removed_new_file() {
        let r = crate::session::undo::RestoredOp {
            path: std::path::PathBuf::from("src/new.rs"),
            kind: UndoKind::Write,
            prev_existed: false,
        };
        let s = format_undo_popped(&r);
        assert!(s.contains("removed"));
        assert!(s.contains("newly-created"));
    }
}
