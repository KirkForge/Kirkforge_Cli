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

use crate::tui::app::AppState;
use tokio::sync::mpsc;

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
pub fn handle_undo_command(
    args: &str,
    undo_tx: &mpsc::UnboundedSender<()>,
    state: &mut AppState,
) -> String {
    let args = args.trim();
    match args {
        "" => {
            // Pop is performed by the executor so the file I/O happens on
            // the executor task, not the TUI task. The result comes back as
            // a TurnEvent::Token in the chat.
            match undo_tx.send(()) {
                Ok(()) => "Undoing most recent edit…".to_string(),
                Err(_) => "Executor is not running; cannot undo.".to_string(),
            }
        }
        "list" => {
            let Some(ref stack) = state.undo_stack else {
                return "Undo unavailable: no undo stack for this session.".to_string();
            };
            let entries = match stack.lock() {
                Ok(s) => s.list(),
                Err(e) => return format!("Undo stack mutex poisoned: {}", e),
            };
            format_undo_list(&entries)
        }
        "count" => {
            let Some(ref stack) = state.undo_stack else {
                return "Undo unavailable: no undo stack for this session.".to_string();
            };
            let count = match stack.lock() {
                Ok(s) => s.len(),
                Err(e) => return format!("Undo stack mutex poisoned: {}", e),
            };
            format!("Undo stack contains {} entr{}", count, if count == 1 { "y" } else { "ies" })
        }
        _ => format!(
            "Usage: /undo [list|count]\n\nUnknown argument '{}'. /undo pops the most recent edit; /undo list shows the stack; /undo count prints the depth.",
            args
        ),
    }
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
/// Currently only exercised by unit tests; kept around so the
/// executor's inline formatting and the TUI command stay in sync.
#[cfg(test)]
pub fn format_undo_popped(restored: &crate::session::undo::RestoredOp) -> String {
    let action = if restored.prev_existed {
        format!("restored {}", restored.path.display())
    } else {
        format!("removed newly-created {}", restored.path.display())
    };
    format!("↶ Undo: {} ({})", action, restored.kind.as_str())
}

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

    /// Helper: fresh AppState with an empty UndoStack in a temp dir.
    fn state_with_stack() -> (AppState, crate::tools::UndoStackRef, std::path::PathBuf) {
        let id = format!(
            "cmd-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let stack = crate::session::undo::UndoStack::for_session(&id).expect("for_session");
        let target = std::env::temp_dir().join(format!("kf_undo_cmd_target_{}.txt", id));
        let state = AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::shared::Config::default(),
        )));
        let stack_ref = std::sync::Arc::new(std::sync::Mutex::new(stack));
        (state, stack_ref, target)
    }

    /// `/undo count` on an empty stack reports zero with singular wording.
    #[test]
    fn test_undo_count_empty() {
        let (mut state, stack_ref, _) = state_with_stack();
        state.undo_stack = Some(stack_ref);
        let (tx, _rx) = mpsc::unbounded_channel();
        let out = handle_undo_command("count", &tx, &mut state);
        assert!(out.contains("0 entries"), "got: {}", out);
    }

    /// `/undo list` on an empty stack reports the empty message.
    #[test]
    fn test_undo_list_empty() {
        let (mut state, stack_ref, _) = state_with_stack();
        state.undo_stack = Some(stack_ref);
        let (tx, _rx) = mpsc::unbounded_channel();
        let out = handle_undo_command("list", &tx, &mut state);
        assert!(out.contains("empty"), "got: {}", out);
    }

    /// `/undo count` reflects pushed entries.
    #[test]
    fn test_undo_count_reflects_stack() {
        let (mut state, stack_ref, target) = state_with_stack();
        std::fs::write(&target, b"v1").unwrap();
        {
            let mut s = stack_ref.lock().unwrap();
            let prev = std::fs::read(&target).unwrap();
            s.push(crate::session::undo::UndoKind::Edit, &target, true, &prev)
                .unwrap();
            std::fs::write(&target, b"v2").unwrap();
        }
        state.undo_stack = Some(stack_ref);
        let (tx, _rx) = mpsc::unbounded_channel();
        let out = handle_undo_command("count", &tx, &mut state);
        assert!(out.contains("1 entry"), "got: {}", out);
    }

    /// `/undo list` reflects pushed entries with paths and kinds.
    #[test]
    fn test_undo_list_reflects_stack() {
        let (mut state, stack_ref, target) = state_with_stack();
        std::fs::write(&target, b"v1").unwrap();
        {
            let mut s = stack_ref.lock().unwrap();
            let prev = std::fs::read(&target).unwrap();
            s.push(crate::session::undo::UndoKind::Edit, &target, true, &prev)
                .unwrap();
            std::fs::write(&target, b"v2").unwrap();
        }
        state.undo_stack = Some(stack_ref);
        let (tx, _rx) = mpsc::unbounded_channel();
        let out = handle_undo_command("list", &tx, &mut state);
        assert!(out.contains("1 entries"), "got: {}", out);
        assert!(out.contains("edit"), "got: {}", out);
        assert!(
            out.contains(target.file_name().unwrap().to_str().unwrap()),
            "got: {}",
            out
        );
    }

    /// Unknown `/undo` argument returns usage and does not pop.
    #[test]
    fn test_undo_unknown_argument_returns_usage() {
        let (mut state, stack_ref, _) = state_with_stack();
        state.undo_stack = Some(stack_ref);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let out = handle_undo_command("foo", &tx, &mut state);
        assert!(out.contains("Usage"), "got: {}", out);
        assert!(out.contains("foo"), "got: {}", out);
        assert!(
            rx.try_recv().is_err(),
            "pop signal should not have been sent"
        );
    }
}
