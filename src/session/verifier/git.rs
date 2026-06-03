use crate::session::event_bus::{BashExecEvent, BusEvent, GitOperationEvent};
/// Git verifier — validates git state after operations.
///
/// Checks for:
/// - Uncommitted changes after git operations
/// - Merge conflicts
/// - Dirty worktree state
/// - Branch status
use crate::session::verifier::{Verdict, VerificationError};
use std::path::PathBuf;

/// Run the git verifier against an event.
pub async fn verify_git(event: &BusEvent) -> Verdict {
    match event {
        BusEvent::GitOperation(GitOperationEvent {
            args,
            output,
            success,
        }) => verify_git_operation(args, output, *success).await,
        BusEvent::BashExec(BashExecEvent {
            command, exit_code, ..
        }) => {
            // Only react to bash commands that look like git commands
            if command.trim_start().starts_with("git ") {
                verify_git_bash(command, *exit_code).await
            } else {
                Verdict::Skipped("not a git command".into())
            }
        }
        _ => Verdict::Skipped("not a git event".into()),
    }
}

async fn verify_git_operation(args: &[String], _output: &str, success: bool) -> Verdict {
    if success {
        return Verdict::Clean;
    }

    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");

    match cmd {
        "merge" | "rebase" | "cherry-pick" => {
            // Check for merge conflicts
            check_merge_conflicts().await
        }
        "commit" => Verdict::Unfixable(VerificationError {
            description: "git commit failed".into(),
            file: None,
            details: "The commit operation failed. Check git status and try again.".into(),
        }),
        "push" => Verdict::Unfixable(VerificationError {
            description: "git push failed".into(),
            file: None,
            details: "Push was rejected. You may need to pull first or check remote status.".into(),
        }),
        _ => Verdict::Clean,
    }
}

async fn verify_git_bash(command: &str, exit_code: i32) -> Verdict {
    if exit_code == 0 {
        // Even successful operations may leave dirty state
        return check_dirty_worktree().await;
    }

    // Check for merge conflict messages
    if command.contains("merge") || command.contains("rebase") || command.contains("pull") {
        return check_merge_conflicts().await;
    }

    Verdict::Clean
}

/// Check for dirty worktree after an operation.
async fn check_dirty_worktree() -> Verdict {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .await;

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Verdict::Skipped("git not available".into()),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let dirty_count = stdout.lines().count();

    if dirty_count > 0 {
        Verdict::Unfixable(VerificationError {
            description: format!("Dirty worktree: {} uncommitted changes", dirty_count),
            file: None,
            details: format!(
                "There are {} uncommitted files. Consider committing or stashing before proceeding.\n{}",
                dirty_count,
                stdout.lines().take(10).collect::<Vec<_>>().join("\n")
            ),
        })
    } else {
        Verdict::Clean
    }
}

/// Check for merge conflicts after a failed merge-like operation.
async fn check_merge_conflicts() -> Verdict {
    let output = tokio::process::Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .output()
        .await;

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Verdict::Skipped("git not available".into()),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let conflicted: Vec<&str> = stdout.lines().collect();

    if conflicted.is_empty() {
        return Verdict::Clean;
    }

    Verdict::Unfixable(VerificationError {
        description: format!("{} merge conflicts detected", conflicted.len()),
        file: Some(PathBuf::from(conflicted.first().unwrap_or(&""))),
        details: format!(
            "Files with conflicts:\n{}",
            conflicted
                .iter()
                .map(|f| format!("  - {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_skips_non_git_events() {
        let event = BusEvent::Edit(crate::session::event_bus::EditEvent {
            path: std::path::PathBuf::from("x.rs"),
            diff: "".into(),
        });
        let v = verify_git(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_non_git_bash_skipped() {
        let event = BusEvent::BashExec(crate::session::event_bus::BashExecEvent {
            command: "ls -la".into(),
            exit_code: 0,
            stdout_len: 100,
            stderr_len: 0,
        });
        let v = verify_git(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_successful_git_op_returns_clean() {
        let event = BusEvent::GitOperation(crate::session::event_bus::GitOperationEvent {
            args: vec!["status".into()],
            output: "On branch main".into(),
            success: true,
        });
        let v = verify_git(&event).await;
        assert!(matches!(v, Verdict::Clean));
    }
}
