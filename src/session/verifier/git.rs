use crate::session::event_bus::{BashExecEvent, BusEvent, GitOperationEvent};
/// Git verifier — validates git state after operations.
///
/// Checks for:
/// - Uncommitted changes after git-modifying bash commands
/// - Merge conflicts after failed merge-like operations
/// - Dirty worktree state
/// - Branch status
use crate::session::verifier::{Verdict, VerificationError};
use std::path::{Path, PathBuf};

/// Run the git verifier against an event.
pub async fn verify_git(event: &BusEvent) -> Verdict {
    match event {
        BusEvent::GitOperation(GitOperationEvent {
            args,
            output,
            success,
        }) => verify_git_operation(args, output, *success).await,
        BusEvent::BashExec(BashExecEvent {
            command,
            exit_code,
            workdir,
            ..
        }) => {
            // Only react to bash commands that look like git commands
            if command.trim_start().starts_with("git ") {
                verify_git_bash(command, *exit_code, workdir.as_deref()).await
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
            check_merge_conflicts(None).await
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

/// Commands that, on success, may leave the worktree dirty.
#[inline]
fn is_git_modifying_command(command: &str) -> bool {
    let lowered = command.to_lowercase();
    [
        "git add",
        "git rm",
        "git mv",
        "git commit",
        "git merge",
        "git rebase",
        "git cherry-pick",
        "git pull",
        "git checkout",
        "git reset",
        "git restore",
        "git revert",
    ]
    .iter()
    .any(|prefix| lowered.starts_with(prefix))
}

/// Commands whose failure may leave merge conflicts behind.
#[inline]
fn is_conflict_prone_command(command: &str) -> bool {
    let lowered = command.to_lowercase();
    lowered.contains("merge")
        || lowered.contains("rebase")
        || lowered.contains("cherry-pick")
        || lowered.contains("pull")
}

async fn verify_git_bash(command: &str, exit_code: i32, workdir: Option<&Path>) -> Verdict {
    if exit_code == 0 {
        // Even successful operations may leave dirty state, but only check
        // after commands that are known to modify the worktree.
        if is_git_modifying_command(command) {
            return check_dirty_worktree(workdir).await;
        }
        return Verdict::Clean;
    }

    // Check for merge conflict messages only on conflict-prone commands.
    if is_conflict_prone_command(command) {
        return check_merge_conflicts(workdir).await;
    }

    Verdict::Clean
}

/// Build a `git` command that optionally runs inside `workdir`.
fn git_cmd(workdir: Option<&Path>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    }
    cmd
}

/// Check for dirty worktree after an operation.
async fn check_dirty_worktree(workdir: Option<&Path>) -> Verdict {
    let output = git_cmd(workdir)
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
            description: format!("Dirty worktree: {dirty_count} uncommitted changes"),
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
async fn check_merge_conflicts(workdir: Option<&Path>) -> Verdict {
    let output = git_cmd(workdir)
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
            workdir: None,
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

    #[tokio::test]
    async fn test_non_modifying_git_command_is_clean() {
        // `git status` succeeds without modifying anything, so we should not
        // complain about a dirty worktree even if one exists elsewhere.
        let tmp = std::env::temp_dir().join("kirkforge_git_nonmod");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let event = BusEvent::BashExec(crate::session::event_bus::BashExecEvent {
            command: "git status".into(),
            exit_code: 0,
            stdout_len: 0,
            stderr_len: 0,
            workdir: Some(tmp.clone()),
        });
        let v = verify_git(&event).await;
        // Either Clean (git status on a non-repo is not an error here because
        // the command succeeded but `git status` exits 128; since we pass
        // exit_code=0 explicitly, the verifier sees success and treats it as
        // a non-modifying command, returning Clean without running git again).
        assert!(matches!(v, Verdict::Clean | Verdict::Skipped(_)));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_dirty_worktree_after_git_add() {
        let tmp = std::env::temp_dir().join("kirkforge_git_dirty");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Initialise a repo and stage a new file so status --porcelain reports it.
        let init = tokio::process::Command::new("git")
            .current_dir(&tmp)
            .args(["init"])
            .output()
            .await
            .expect("git init failed");
        assert!(init.status.success());

        std::fs::write(tmp.join("file.txt"), "hello").unwrap();

        let stage = tokio::process::Command::new("git")
            .current_dir(&tmp)
            .args(["add", "file.txt"])
            .output()
            .await
            .expect("git add failed");
        assert!(stage.status.success());

        let event = BusEvent::BashExec(crate::session::event_bus::BashExecEvent {
            command: "git add file.txt".into(),
            exit_code: 0,
            stdout_len: 0,
            stderr_len: 0,
            workdir: Some(tmp.clone()),
        });
        let v = verify_git(&event).await;
        assert!(
            matches!(v, Verdict::Unfixable(_)),
            "staged file should leave a dirty worktree: {v:?}"
        );
        if let Verdict::Unfixable(err) = v {
            assert!(err.description.contains("Dirty worktree"));
            assert!(err.details.contains("file.txt"));
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_failed_merge_conflict_detection() {
        let tmp = std::env::temp_dir().join("kirkforge_git_conflict");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        async fn git(tmp: &std::path::Path, args: &[&str]) {
            let out = tokio::process::Command::new("git")
                .current_dir(tmp)
                .args(args)
                .output()
                .await
                .expect("git command failed");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        git(&tmp, &["init"]).await;
        // Ensure the default branch is named "main" regardless of git version/config.
        git(&tmp, &["branch", "-m", "main"]).await;
        git(&tmp, &["config", "user.email", "test@example.com"]).await;
        git(&tmp, &["config", "user.name", "Test User"]).await;

        std::fs::write(tmp.join("file.txt"), "base\n").unwrap();
        git(&tmp, &["add", "file.txt"]).await;
        git(&tmp, &["commit", "-m", "initial"]).await;

        git(&tmp, &["checkout", "-b", "branch"]).await;
        std::fs::write(tmp.join("file.txt"), "branch-line\n").unwrap();
        git(&tmp, &["commit", "-am", "branch change"]).await;

        git(&tmp, &["checkout", "main"]).await;
        std::fs::write(tmp.join("file.txt"), "main-line\n").unwrap();
        git(&tmp, &["commit", "-am", "main change"]).await;

        // This merge will fail and leave `file.txt` as an unmerged path.
        let merge = tokio::process::Command::new("git")
            .current_dir(&tmp)
            .args(["merge", "branch"])
            .output()
            .await
            .expect("git merge failed");
        assert!(!merge.status.success(), "merge should have conflicted");

        let event = BusEvent::BashExec(crate::session::event_bus::BashExecEvent {
            command: "git merge branch".into(),
            exit_code: 1,
            stdout_len: 0,
            stderr_len: 10,
            workdir: Some(tmp.clone()),
        });
        let v = verify_git(&event).await;
        assert!(
            matches!(v, Verdict::Unfixable(_)),
            "conflict file should be detected: {v:?}"
        );
        if let Verdict::Unfixable(err) = v {
            assert!(err.description.contains("merge conflicts"));
            assert!(err.details.contains("file.txt"));
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
