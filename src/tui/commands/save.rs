//! `/save` slash-command handler.
//!
//! Writes the current TUI conversation as a GitHub-flavored Markdown
//! transcript to a file. The user can then open or share the file like a
//! Claude Code transcript.

use crate::session::access::{access_from_config, GuardVerdict};
use crate::tui::app::AppState;
use std::path::PathBuf;

/// Handle `/save [path]` command.
///
/// - `args` is empty → write to a default path next to the session log
///   (`~/.local/share/kirkforge/sessions/YYYY-MM-DD-session-NN.md`).
/// - `args` is a path → write to that path.
///
/// Returns a user-visible status string.
pub fn handle_save_command(args: &str, state: &AppState) -> String {
    let path = resolve_save_path(args, state);

    // Apply the same PathGuard write check that write_file/edit_file go
    // through. `/save` writes user data to disk, so it must respect the
    // sandbox and deny list.
    let cfg = crate::shared::read_shared_config(&state.config);
    let (_deny_list, path_guard, _read_gate) = access_from_config(&cfg);
    if let GuardVerdict::Denied(msg) = path_guard.check_write(&path) {
        return format!("🔒 Access denied: {msg}");
    }

    let transcript = crate::tui::transcript::format_transcript(&state.session_id, &state.messages);

    if let Err(e) = ensure_parent_dir(&path) {
        return format!(
            "❌ Failed to create directory for {}: {}",
            path.display(),
            e
        );
    }

    match std::fs::write(&path, &transcript) {
        Ok(()) => format!(
            "💾 Saved transcript to {} ({} bytes)",
            path.display(),
            transcript.len()
        ),
        Err(e) => format!("❌ Failed to save transcript to {}: {}", path.display(), e),
    }
}

/// Create the parent directory for `path` if it exists and is non-empty.
/// Relative filenames like "chat.md" have a parent of "" and are skipped.
fn ensure_parent_dir(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            return std::fs::create_dir_all(parent);
        }
    }
    Ok(())
}

fn resolve_save_path(args: &str, state: &AppState) -> PathBuf {
    let trimmed = args.trim();
    if !trimmed.is_empty() {
        return PathBuf::from(trimmed);
    }

    if let Some(log) = &state.log_path {
        let stem = log
            .file_stem()
            .and_then(|f| f.to_str())
            .map(|s| s.trim_end_matches(".conv"))
            .unwrap_or("transcript");
        log.with_file_name(format!("{stem}.md"))
    } else {
        let now = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(format!("kirkforge-transcript-{now}.md"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;
    use crate::tui::app::AppState;
    use std::sync::Arc;

    fn test_state_with_log(log_path: PathBuf) -> AppState {
        let mut state = AppState::new(Arc::new(std::sync::RwLock::new(Config::default())));
        state.log_path = Some(log_path);
        state.session_id = "2026-06-22-session-01".to_string();
        state
    }

    #[test]
    fn save_writes_file_next_to_log() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("2026-06-22-session-01.conv.ndjson");
        let state = test_state_with_log(log);
        let msg = handle_save_command("", &state);
        assert!(msg.starts_with("💾 Saved transcript"));
        let expected = tmp.path().join("2026-06-22-session-01.md");
        assert!(
            expected.exists(),
            "expected {} to exist",
            expected.display()
        );
        let content = std::fs::read_to_string(&expected).unwrap();
        assert!(content.contains("# KirkForge transcript"));
    }

    #[test]
    fn save_writes_file_to_explicit_path() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("my-chat.md");
        let state = AppState::new(Arc::new(std::sync::RwLock::new(Config::default())));
        let msg = handle_save_command(target.to_str().unwrap(), &state);
        assert!(msg.starts_with("💾 Saved transcript"));
        assert!(target.exists());
    }

    #[test]
    fn save_includes_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("s.conv.ndjson");
        let mut state = test_state_with_log(log);
        state
            .messages
            .push(crate::tui::app::ConversationEntry::new("user", "hi"));
        state.messages.push(crate::tui::app::ConversationEntry::new(
            "assistant",
            "hello",
        ));
        let _msg = handle_save_command("", &state);
        let expected = tmp.path().join("s.md");
        let content = std::fs::read_to_string(&expected).unwrap();
        assert!(content.contains("hi"));
        assert!(content.contains("hello"));
    }

    #[test]
    fn ensure_parent_dir_creates_missing_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b").join("c.md");
        assert!(ensure_parent_dir(&nested).is_ok());
        assert!(nested.parent().unwrap().exists());
    }

    #[test]
    fn ensure_parent_dir_skips_relative_file_without_parent() {
        let path = PathBuf::from("chat.md");
        assert!(ensure_parent_dir(&path).is_ok());
    }
}
