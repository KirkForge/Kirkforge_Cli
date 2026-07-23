//! `/commit` slash command — stage, review, and commit changes.
//!
//! The command has two modes:
//!
//! - `/commit` without arguments runs the pre-commit sanitation pass,
//!   shows `git status`, and suggests a conventional-commit style message.
//!   It does **not** commit anything, so the user can review before
//!   providing a message.
//! - `/commit "message"` runs sanitation and, if no blockers are found,
//!   stages all changes (`git add -A`) and commits with the supplied
//!   message.
//! - `/commit --push "message"` does the same and then pushes the current
//!   branch to the configured upstream.
//!
//! Sanitation is fail-closed: blockers (large files, secret patterns, or
//! merge-conflict markers) abort the commit and are shown to the user.
//! Warnings (untracked/unstaged debris) are shown but do not block.

use super::memory::truncate_to_char_boundary;
use crate::session::git_sanitation::{check_worktree, suggest_message};
use crate::shared::Config;
use std::path::Path;
use tokio::process::Command;

/// Maximum bytes to capture from `git status --porcelain`.
const MAX_STATUS_BYTES: usize = 256 * 1024;

/// Parsed arguments for `/commit`.
#[derive(Debug, Clone, Default)]
struct CommitArgs {
    /// `--push` flag was present.
    push: bool,
    /// The commit message, if any.
    message: Option<String>,
}

/// Parse `/commit`, `/commit "message"`, or `/commit --push "message"`.
fn parse_commit_args(input: &str) -> CommitArgs {
    let mut rest = input.trim();
    let mut push = false;

    // Handle leading --push flag, with or without a message after it.
    if let Some(after_flag) = rest.strip_prefix("--push") {
        push = true;
        rest = after_flag.trim_start();
    }

    let message = if rest.is_empty() {
        None
    } else {
        // Strip surrounding quotes if present.
        let m = rest
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(rest);
        Some(m.to_string())
    };

    CommitArgs { push, message }
}

/// Handle `/commit [message]` or `/commit --push [message]`.
///
/// If no message is supplied, returns a status report and a suggested
/// message. If a message is supplied, runs sanitation, commits, and
/// optionally pushes.
pub async fn handle_commit_command(args: &str, cwd: &Path, config: &Config) -> String {
    let parsed = parse_commit_args(args);
    let has_message = parsed.message.is_some();

    // 1. Gather git status.
    let status = match git_status_porcelain(cwd).await {
        Ok(s) => s,
        Err(e) => return format!("❌ Cannot run git status: {e}"),
    };

    let status_lines: Vec<String> = status.lines().map(|l| l.to_string()).collect();

    // 2. Run sanitation.
    let report = match check_worktree(cwd, &status, Some(config.security.commit_max_file_size)) {
        Ok(r) => r,
        Err(e) => return format!("❌ Sanitation check failed: {e}"),
    };

    let report_text = report.format();

    // 3. If no commit message, show the report + suggestion and stop.
    if !has_message {
        let suggested = suggest_message(&status_lines);
        let status_preview = if status_lines.is_empty() {
            "Working tree is clean.".to_string()
        } else {
            format!("Changed files:\n{}", status_lines.join("\n"))
        };
        let push_hint = if parsed.push {
            "\nFlag `--push` will be applied once a message is supplied."
        } else {
            ""
        };
        return format!(
            "📝 Pre-commit review\n\n{status_preview}\n\n{report_text}\n\n\
             Suggested message: `{suggested}`\n\
             Run `/commit \"{suggested}\"` to commit, or supply your own message.{push_hint}"
        );
    }

    // 4. If blockers exist, refuse to commit.
    if !report.is_clean() {
        return format!(
            "❌ Commit aborted due to sanitation blockers.\n\n{report_text}\n\n\
             Fix the blockers above and try again."
        );
    }

    // 5. Stage all and commit.
    let message = parsed.message.unwrap_or_default();
    match stage_all(cwd).await {
        Ok(()) => {}
        Err(e) => return format!("❌ `git add -A` failed: {e}"),
    }
    let commit_output = match git_commit(cwd, &message).await {
        Ok(output) => output,
        Err(e) => return format!("❌ `git commit` failed: {e}"),
    };

    // 6. Optionally push.
    let mut push_output = String::new();
    if parsed.push {
        match git_push(cwd).await {
            Ok(out) => {
                push_output = format!("\n\n📤 Pushed:\n{}", out.trim());
            }
            Err(e) => {
                push_output = format!("\n\n⚠️ Push failed: {e}");
            }
        }
    }

    let output_trimmed = commit_output.trim();
    if output_trimmed.is_empty() {
        format!("✅ Committed: {message}\n\n{report_text}{push_output}")
    } else {
        format!("✅ Committed: {message}\n\n{output_trimmed}\n\n{report_text}{push_output}")
    }
}

/// Run `git status --porcelain` in `cwd` and return its stdout.
async fn git_status_porcelain(cwd: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("failed to spawn git status: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git status failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(truncate_to_char_boundary(&stdout, MAX_STATUS_BYTES).to_string())
}

/// Stage all changes in `cwd`.
async fn stage_all(cwd: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .args(["add", "-A"])
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("failed to spawn git add: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    Ok(())
}

/// Commit with `message` in `cwd`.
async fn git_commit(cwd: &Path, message: &str) -> Result<String, String> {
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("failed to spawn git commit: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.to_string())
}

/// Push the current branch in `cwd`.
async fn git_push(cwd: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .arg("push")
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("failed to spawn git push: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> Config {
        Config::default()
    }

    #[tokio::test]
    async fn empty_args_returns_report_and_suggestion() {
        let out = handle_commit_command("", Path::new("."), &default_config()).await;
        // Should mention pre-commit review in any git repo (or git status failure).
        assert!(
            out.contains("Pre-commit review") || out.contains("Cannot run git status"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn commit_without_git_repo_fails_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let out = handle_commit_command("test message", tmp.path(), &default_config()).await;
        assert!(
            out.contains("Cannot run git status") || out.contains("git status failed"),
            "got: {out}"
        );
    }

    #[test]
    fn parse_commit_args_no_message_no_push() {
        let p = parse_commit_args("");
        assert!(!p.push);
        assert!(p.message.is_none());
    }

    #[test]
    fn parse_commit_args_message_no_push() {
        let p = parse_commit_args("\"fix the thing\"");
        assert!(!p.push);
        assert_eq!(p.message.as_deref(), Some("fix the thing"));
    }

    #[test]
    fn parse_commit_args_push_flag_with_message() {
        let p = parse_commit_args("--push \"feat: add widget\"");
        assert!(p.push);
        assert_eq!(p.message.as_deref(), Some("feat: add widget"));
    }

    #[test]
    fn parse_commit_args_push_flag_without_message() {
        let p = parse_commit_args("--push");
        assert!(p.push);
        assert!(p.message.is_none());
    }
}
