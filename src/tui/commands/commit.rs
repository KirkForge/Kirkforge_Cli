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
//!
//! Sanitation is fail-closed: blockers (large files, secret patterns, or
//! merge-conflict markers) abort the commit and are shown to the user.
//! Warnings (untracked/unstaged debris) are shown but do not block.

use crate::session::git_sanitation::{check_worktree, suggest_message};
use std::path::Path;
use tokio::process::Command;

/// Maximum bytes to capture from `git status --porcelain`.
const MAX_STATUS_BYTES: usize = 256 * 1024;

/// Handle `/commit [message]`.
///
/// If no message is supplied, returns a status report and a suggested
/// message. If a message is supplied, runs sanitation and commits.
pub async fn handle_commit_command(args: &str, cwd: &Path) -> String {
    let args = args.trim();
    let has_message = !args.is_empty();

    // 1. Gather git status.
    let status = match git_status_porcelain(cwd).await {
        Ok(s) => s,
        Err(e) => return format!("❌ Cannot run git status: {e}"),
    };

    let status_lines: Vec<String> = status.lines().map(|l| l.to_string()).collect();

    // 2. Run sanitation.
    let report = match check_worktree(cwd, &status, None) {
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
        return format!(
            "📝 Pre-commit review\n\n{status_preview}\n\n{report_text}\n\n\
             Suggested message: `{suggested}`\n\
             Run `/commit \"{suggested}\"` to commit, or supply your own message."
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
    let message = args;
    match stage_all(cwd).await {
        Ok(()) => {}
        Err(e) => return format!("❌ `git add -A` failed: {e}"),
    }
    match git_commit(cwd, message).await {
        Ok(output) => {
            let output_trimmed = output.trim();
            if output_trimmed.is_empty() {
                format!("✅ Committed: {message}\n\n{report_text}")
            } else {
                format!("✅ Committed: {message}\n\n{output_trimmed}\n\n{report_text}")
            }
        }
        Err(e) => format!("❌ `git commit` failed: {e}"),
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
    Ok(stdout.chars().take(MAX_STATUS_BYTES).collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_args_returns_report_and_suggestion() {
        let out = handle_commit_command("", Path::new(".")).await;
        // Should mention pre-commit review in any git repo (or git status failure).
        assert!(
            out.contains("Pre-commit review") || out.contains("Cannot run git status"),
            "got: {}",
            out
        );
    }

    #[tokio::test]
    async fn commit_without_git_repo_fails_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let out = handle_commit_command("test message", tmp.path()).await;
        assert!(
            out.contains("Cannot run git status") || out.contains("git status failed"),
            "got: {}",
            out
        );
    }
}
