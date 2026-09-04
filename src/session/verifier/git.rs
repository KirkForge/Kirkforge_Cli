use crate::session::verifier::bus::{
    BusVerifier, Severity, VerdictEntry, VerifierSource, VerifyContext,
};
use crate::session::verifier::types::{BashExecEvent, BusEvent};
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
        BusEvent::BashExec(BashExecEvent {
            command,
            exit_code,
            workdir,
            ..
        }) => {
            // Only react to bash commands that invoke git anywhere in the
            // chain (e.g. `cd /repo && git merge`, `sudo git …`).
            if command_invokes_git(command) {
                verify_git_bash(command, *exit_code, workdir.as_deref()).await
            } else {
                Verdict::Skipped("not a git command".into())
            }
        }
        _ => Verdict::Skipped("not a git event".into()),
    }
}

/// Best-effort scan for `git` as a command word anywhere in a shell chain.
///
/// Splits on `&&`/`||`/`;`/`|`/newlines, then for each segment skips leading
/// env assignments (`VAR=value`) and the common prefixes `sudo`/`env`/`nice`/
/// `nohup`/`time`/`exec` before the command word. Returns the lowercased git
/// subcommand (the token after `git`) for each git-invoking segment.
///
/// ponytail: ceiling — command substitution `$(git …)`, `git` inside quoted
/// strings, and `sudo -E git` (a flag between sudo and git) are not parsed.
/// This is a post-condition verifier, not a shell parser; under-detection
/// only skips a best-effort `git status`, and over-detection only runs an
/// extra one, so the trade-off is safe, upgrade to libgit2 if shell-out
/// latency or PATH fragility matters.
fn git_subcommands_in(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    for segment in command.split(['&', '|', ';', '\n']) {
        let mut tokens = segment.split_whitespace();
        let mut cmd = tokens.next();
        while let Some(t) = cmd {
            if t.contains('=') && !t.starts_with('=') {
                cmd = tokens.next();
                continue;
            }
            if matches!(t, "sudo" | "env" | "nice" | "nohup" | "time" | "exec") {
                cmd = tokens.next();
                continue;
            }
            break;
        }
        if cmd.map(|c| c.eq_ignore_ascii_case("git")).unwrap_or(false) {
            out.push(tokens.next().unwrap_or("").to_lowercase());
        }
    }
    out
}

/// True if the command chain invokes `git` somewhere — not just when it
/// starts literally with `git `. Catches `cd /repo && git merge`, `sudo
/// git …`, `GIT_DIR=… git …`, `a && b || git …`.
fn command_invokes_git(command: &str) -> bool {
    !git_subcommands_in(command).is_empty()
}

/// Commands that, on success, may leave the worktree dirty.
#[inline]
fn is_git_modifying_command(command: &str) -> bool {
    const MODS: &[&str] = &[
        "add",
        "rm",
        "mv",
        "commit",
        "merge",
        "rebase",
        "cherry-pick",
        "pull",
        "checkout",
        "reset",
        "restore",
        "revert",
    ];
    git_subcommands_in(command)
        .iter()
        .any(|sub| MODS.iter().any(|m| sub.starts_with(m)))
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
///
/// `git status --porcelain` emits one line per non-clean entry as `XY <path>`
/// where `X` is the index (staged) status and `Y` is the worktree status. A
/// staged-only file (`A  file.txt` — X non-space, Y space) is NOT a worktree
/// violation — the model can commit it. Only entries with a non-space `Y`
/// (unstaged modifications) or untracked files (`??`) count as "dirty".
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
    let mut dirty: Vec<&str> = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        // Porcelain format: two status chars, a space, then the path.
        // `??` = untracked (dirty). Otherwise Y (worktree status, byte 1)
        // non-space means unstaged changes — dirty. X non-space with Y space
        // means staged-only — not a violation, so we skip it.
        let bytes = line.as_bytes();
        let is_untracked = bytes.len() >= 2 && bytes[0] == b'?' && bytes[1] == b'?';
        let has_worktree_changes = bytes.len() >= 2 && bytes[1] != b' ' && bytes[1] != b'\0';
        if is_untracked || has_worktree_changes {
            dirty.push(line);
        }
    }

    if dirty.is_empty() {
        // Staged-only (or fully clean) is not a worktree violation.
        Verdict::Clean
    } else {
        let dirty_count = dirty.len();
        Verdict::Unfixable(VerificationError {
            description: format!("Dirty worktree: {dirty_count} uncommitted changes"),
            file: None,
            details: format!(
                "There are {} uncommitted files. Consider committing or stashing before proceeding.\n{}",
                dirty_count,
                dirty.iter().take(10).copied().collect::<Vec<_>>().join("\n")
            ),
            line: None,
        })
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
        line: None,
    })
}

// ── BusVerifier impl (WO 47.14) ─────────────────────────────────────────
//
// The sync `BusVerifier` path. Reads bash command/exit_code/workdir from
// `VerifyContext` and runs `git status`/`git diff` via `std::process::Command`
// (blocking — the bus runs inside `spawn_blocking`).

/// Git verifier registered on the `VerifierBus`. WO 47.14.
pub struct GitVerifier;

impl BusVerifier for GitVerifier {
    fn name(&self) -> &str {
        "git"
    }

    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        // Only react to bash events
        let Some(ref command) = ctx.bash_command else {
            return vec![];
        };
        let exit_code = ctx.bash_exit_code.unwrap_or(0);
        let workdir = ctx.bash_workdir.as_deref();

        // Only react to bash commands that invoke git
        if !command_invokes_git(command) {
            return vec![];
        }

        if exit_code == 0 {
            if is_git_modifying_command(command) {
                return check_dirty_worktree_sync(workdir);
            }
            return vec![];
        }

        if is_conflict_prone_command(command) {
            return check_merge_conflicts_sync(workdir);
        }

        vec![]
    }
}

/// Sync version of check_dirty_worktree using std::process::Command.
fn check_dirty_worktree_sync(workdir: Option<&Path>) -> Vec<VerdictEntry> {
    let mut cmd = std::process::Command::new("git");
    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    }
    let output = match cmd.args(["status", "--porcelain"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut dirty: Vec<&str> = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let bytes = line.as_bytes();
        let is_untracked = bytes.len() >= 2 && bytes[0] == b'?' && bytes[1] == b'?';
        let has_worktree_changes = bytes.len() >= 2 && bytes[1] != b' ' && bytes[1] != b'\0';
        if is_untracked || has_worktree_changes {
            dirty.push(line);
        }
    }
    if dirty.is_empty() {
        return vec![];
    }
    let dirty_count = dirty.len();
    vec![VerdictEntry {
        source: VerifierSource::Git,
        severity: Severity::Error,
        message: format!(
            "Dirty worktree: {dirty_count} uncommitted changes\nThere are {} uncommitted files. Consider committing or stashing before proceeding.\n{}",
            dirty_count,
            dirty.iter().take(10).copied().collect::<Vec<_>>().join("\n")
        ),
        file: None,
        line: None,
        fix: None,
    }]
}

/// Sync version of check_merge_conflicts using std::process::Command.
fn check_merge_conflicts_sync(workdir: Option<&Path>) -> Vec<VerdictEntry> {
    let mut cmd = std::process::Command::new("git");
    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    }
    let output = match cmd
        .args(["diff", "--name-only", "--diff-filter=U"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let conflicted: Vec<&str> = stdout.lines().collect();
    if conflicted.is_empty() {
        return vec![];
    }
    vec![VerdictEntry {
        source: VerifierSource::Git,
        severity: Severity::Error,
        message: format!(
            "{} merge conflicts detected\nFiles with conflicts:\n{}",
            conflicted.len(),
            conflicted
                .iter()
                .map(|f| format!("  - {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        file: conflicted.first().map(PathBuf::from),
        line: None,
        fix: None,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_skips_non_git_events() {
        let event = BusEvent::Edit(crate::session::verifier::types::EditEvent {
            path: std::path::PathBuf::from("x.rs"),
            diff: "".into(),
        });
        let v = verify_git(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_non_git_bash_skipped() {
        let event = BusEvent::BashExec(crate::session::verifier::types::BashExecEvent {
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
    async fn test_non_modifying_git_command_is_clean() {
        // `git status` succeeds without modifying anything, so we should not
        // complain about a dirty worktree even if one exists elsewhere.
        let tmp = std::env::temp_dir().join("kf_code_git_nonmod");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let event = BusEvent::BashExec(crate::session::verifier::types::BashExecEvent {
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
    async fn test_staged_file_is_not_a_worktree_violation() {
        // WO 15.9 (bucketlist 2.10): a staged file (`A  file.txt` in
        // porcelain) is not a worktree violation — the model can commit it.
        let tmp = std::env::temp_dir().join("kf_code_git_staged");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Initialise a repo and stage a new file so status --porcelain
        // reports it as `A  file.txt` (staged, no worktree changes).
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

        let event = BusEvent::BashExec(crate::session::verifier::types::BashExecEvent {
            command: "git add file.txt".into(),
            exit_code: 0,
            stdout_len: 0,
            stderr_len: 0,
            workdir: Some(tmp.clone()),
        });
        let v = verify_git(&event).await;
        assert!(
            matches!(v, Verdict::Clean),
            "staged-only file should NOT be a worktree violation: {v:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_dirty_worktree_after_unstaged_modification() {
        // A genuine unstaged modification (` M file.txt` in porcelain) IS a
        // dirty worktree and must stay Unfixable.
        let tmp = std::env::temp_dir().join("kf_code_git_dirty");
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
        git(&tmp, &["config", "user.email", "test@example.com"]).await;
        git(&tmp, &["config", "user.name", "Test User"]).await;

        std::fs::write(tmp.join("file.txt"), "hello").unwrap();
        git(&tmp, &["add", "file.txt"]).await;
        git(&tmp, &["commit", "-m", "initial"]).await;

        // Modify the tracked file WITHOUT staging — porcelain shows ` M`.
        std::fs::write(tmp.join("file.txt"), "hello modified").unwrap();

        // A genuine unstaged modification (` M file.txt` in porcelain) IS a
        // dirty worktree and must stay Unfixable. Call the checker directly
        // so we exercise the parsing without depending on a modifying-command
        // classifier.
        let v = check_dirty_worktree(Some(&tmp)).await;
        assert!(
            matches!(v, Verdict::Unfixable(_)),
            "unstaged modification should be a dirty worktree: {v:?}"
        );
        if let Verdict::Unfixable(err) = v {
            assert!(err.description.contains("Dirty worktree"));
            assert!(err.details.contains("file.txt"));
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_failed_merge_conflict_detection() {
        let tmp = std::env::temp_dir().join("kf_code_git_conflict");
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

        let event = BusEvent::BashExec(crate::session::verifier::types::BashExecEvent {
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

    #[test]
    fn test_command_invokes_git_detects_chained_and_prefixed() {
        // S6: the old `starts_with("git ")` gate skipped all of these.
        assert!(command_invokes_git("git merge"));
        assert!(command_invokes_git("  git status"));
        assert!(command_invokes_git("cd /repo && git merge main"));
        assert!(command_invokes_git("sudo git pull"));
        assert!(command_invokes_git("GIT_DIR=/x git status"));
        assert!(command_invokes_git("env GIT_DIR=/x git merge"));
        assert!(command_invokes_git("a && b || git rebase"));
        assert!(command_invokes_git("git log | grep HEAD"));
        // Non-git commands are not flagged.
        assert!(!command_invokes_git("ls -la"));
        assert!(!command_invokes_git("echo git merge"));
        assert!(!command_invokes_git("cat git-status.txt"));
        assert!(!command_invokes_git("gitter status"));
        assert!(!command_invokes_git(""));
    }

    #[test]
    fn test_is_git_modifying_command_chained() {
        assert!(is_git_modifying_command("git add ."));
        assert!(is_git_modifying_command("cd /repo && git add ."));
        assert!(is_git_modifying_command("sudo git commit -m x"));
        assert!(is_git_modifying_command("GIT_DIR=/x git merge main"));
        // Non-modifying git subcommands stay clean.
        assert!(!is_git_modifying_command("git status"));
        assert!(!is_git_modifying_command("cd /repo && git log"));
        // Non-git commands and git-as-an-argument are not modifying.
        assert!(!is_git_modifying_command("rm -rf /tmp/x"));
        assert!(!is_git_modifying_command("echo git commit"));
    }

    #[test]
    fn test_git_subcommands_in_extracts_each_segment_subcommand() {
        let subs = git_subcommands_in("git add . && git commit -m msg");
        assert_eq!(subs, vec!["add", "commit"]);
    }

    #[test]
    fn test_git_subcommands_in_handles_prefixes() {
        let subs = git_subcommands_in("sudo env FOO=bar git pull");
        assert_eq!(subs, vec!["pull"]);
    }

    #[test]
    fn test_git_subcommands_in_empty_for_non_git() {
        let subs = git_subcommands_in("ls -la && echo hi");
        assert!(subs.is_empty());
    }

    #[test]
    fn test_git_subcommands_in_returns_empty_subcommand_for_bare_git() {
        let subs = git_subcommands_in("git");
        assert_eq!(subs, vec![""]);
    }

    #[test]
    fn test_git_subcommands_in_lowercases_subcommand() {
        let subs = git_subcommands_in("git STATUS");
        assert_eq!(subs, vec!["status"]);
    }

    #[test]
    fn test_git_subcommands_in_skips_env_assignments_before_git() {
        let subs = git_subcommands_in("FOO=bar BAZ=qux git checkout -b new");
        assert_eq!(subs, vec!["checkout"]);
    }

    #[test]
    fn test_git_subcommands_in_splits_on_pipes_and_newlines() {
        let subs = git_subcommands_in("git status | grep modified\ngit diff");
        assert_eq!(subs, vec!["status", "diff"]);
    }

    #[test]
    fn test_is_conflict_prone_command_matches_merge_rebase_cherry_pick_pull() {
        assert!(is_conflict_prone_command("git merge foo"));
        assert!(is_conflict_prone_command("git rebase main"));
        assert!(is_conflict_prone_command("git cherry-pick abc"));
        assert!(is_conflict_prone_command("git pull origin main"));
        assert!(is_conflict_prone_command("GIT merge"));
        assert!(is_conflict_prone_command("MERGE branch"));
    }

    #[test]
    fn test_is_conflict_prone_command_rejects_non_conflict_prone() {
        assert!(!is_conflict_prone_command("git status"));
        assert!(!is_conflict_prone_command("git log --oneline"));
        assert!(!is_conflict_prone_command("cargo build"));
        assert!(!is_conflict_prone_command(""));
    }
}
