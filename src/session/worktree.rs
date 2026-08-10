use std::path::PathBuf;
use std::process::Command;

/// Manages an isolated git worktree for a session.
/// Created at session start, removed at session end.
pub struct WorktreeSession {
    worktree_path: PathBuf,
    original_path: PathBuf,
}

impl WorktreeSession {
    /// Create a new git worktree at a temp path for the given session id.
    /// Returns the worktree path and a guard that removes it on drop.
    pub fn create(session_id: &str, repo_root: &std::path::Path) -> anyhow::Result<Self> {
        if session_id.is_empty()
            || session_id.contains('/')
            || session_id.contains('\\')
            || session_id.contains("..")
        {
            anyhow::bail!(
                "invalid session id `{session_id}`: must be non-empty with no path separators or `..`"
            );
        }
        let worktree_path = std::env::temp_dir().join(format!("kf-code-session-{session_id}"));

        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                &worktree_path.to_string_lossy(),
                "HEAD",
            ])
            .current_dir(repo_root)
            .output()
            .map_err(|e| anyhow::anyhow!("failed to spawn git worktree add: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // A stale worktree from a crashed session occupies this path and
            // makes `git worktree add` fail. Remove it so the next attempt can
            // proceed instead of leaving the user stuck on every resume.
            let remove = Command::new("git")
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    &worktree_path.to_string_lossy(),
                ])
                .current_dir(repo_root)
                .output();
            match remove {
                Ok(out) if out.status.success() => {
                    tracing::warn!(
                        path = %worktree_path.display(),
                        "removed stale worktree blocking session; retrying add"
                    );
                }
                Ok(out) => {
                    let remove_err = String::from_utf8_lossy(&out.stderr);
                    tracing::warn!(
                        path = %worktree_path.display(),
                        error = %remove_err,
                        "failed to remove stale worktree; original add error: {stderr}"
                    );
                    anyhow::bail!("git worktree add failed: {stderr}");
                }
                Err(e) => {
                    tracing::warn!(
                        path = %worktree_path.display(),
                        error = %e,
                        "failed to run git worktree remove; original add error: {stderr}"
                    );
                    anyhow::bail!("git worktree add failed: {stderr}");
                }
            }
            let retry = Command::new("git")
                .args([
                    "worktree",
                    "add",
                    "--detach",
                    &worktree_path.to_string_lossy(),
                    "HEAD",
                ])
                .current_dir(repo_root)
                .output();
            let output = retry
                .map_err(|e| anyhow::anyhow!("failed to spawn git worktree add retry: {e}"))?;
            if !output.status.success() {
                let retry_err = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git worktree add retry failed: {retry_err}");
            }
        }

        Ok(Self {
            worktree_path: worktree_path.clone(),
            original_path: repo_root.to_path_buf(),
        })
    }

    /// The path to the worktree directory.
    pub fn path(&self) -> &PathBuf {
        &self.worktree_path
    }
}

impl Drop for WorktreeSession {
    fn drop(&mut self) {
        let result = Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                &self.worktree_path.to_string_lossy(),
            ])
            .current_dir(&self.original_path)
            .output();
        if let Err(e) = result {
            tracing::warn!(
                path = %self.worktree_path.display(),
                error = %e,
                "failed to remove worktree"
            );
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn worktree_create_write_file_drop_cleanup() {
        // Create a temp git repo
        let tmp = std::env::temp_dir().join(format!("kf-code-wt-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Init git repo
        let output = Command::new("git")
            .args(["init"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        assert!(output.status.success(), "git init failed");

        // Configure minimal git user so commits work
        Command::new("git")
            .args(["config", "user.email", "test@test"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&tmp)
            .output()
            .unwrap();

        // Create an initial commit (worktree add requires a ref)
        fs::write(tmp.join("README.md"), "test").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&tmp)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&tmp)
            .output()
            .unwrap();

        // Create worktree
        let session_id = "test-session";
        let wt = WorktreeSession::create(session_id, &tmp).unwrap();
        let wt_path = wt.path().clone();

        // Verify worktree exists in git worktree list
        let list_output = Command::new("git")
            .args(["worktree", "list"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        let list = String::from_utf8_lossy(&list_output.stdout);
        let list_norm = list.replace('\\', "/");
        let wt_path_norm = wt_path.to_string_lossy().replace('\\', "/");
        assert!(
            list_norm.contains(&wt_path_norm),
            "worktree list should contain the new worktree path:\n{list}"
        );

        // Write a file inside the worktree
        let test_file = wt_path.join("test.txt");
        fs::write(&test_file, "hello from worktree").unwrap();
        assert!(test_file.exists(), "file should exist in worktree");
        assert_eq!(
            fs::read_to_string(&test_file).unwrap(),
            "hello from worktree"
        );

        // Drop the worktree session
        drop(wt);

        // Verify worktree is removed
        assert!(
            !wt_path.exists(),
            "worktree directory should be removed after drop"
        );

        // Verify git worktree list no longer contains it
        let list_output = Command::new("git")
            .args(["worktree", "list"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        let list = String::from_utf8_lossy(&list_output.stdout);
        let list_norm = list.replace('\\', "/");
        assert!(
            !list_norm.contains(&wt_path_norm),
            "worktree list should not contain removed worktree:\n{list}"
        );

        // Cleanup temp repo
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn worktree_create_recovers_from_stale_worktree() {
        // Simulate a crashed session: a worktree is registered in git's
        // worktree registry at the session path, but its directory was lost.
        // A naive `git worktree add` fails because the path is already
        // registered; create() must clean the stale entry and retry.
        let tmp =
            std::env::temp_dir().join(format!("kf-code-wt-stale-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        Command::new("git")
            .args(["init"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&tmp)
            .output()
            .unwrap();
        fs::write(tmp.join("README.md"), "test").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&tmp)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&tmp)
            .output()
            .unwrap();

        let session_id = "stale-session";
        let wt_path = std::env::temp_dir().join(format!("kf-code-session-{session_id}"));
        let _ = fs::remove_dir_all(&wt_path);

        // First register a worktree at the session path (as if created by a
        // crashed session that never unregistered it)...
        let add = Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                &wt_path.to_string_lossy(),
                "HEAD",
            ])
            .current_dir(&tmp)
            .output()
            .unwrap();
        assert!(add.status.success(), "initial worktree add should succeed");

        // ...then destroy its directory without unregistering (crash).
        fs::remove_dir_all(&wt_path).unwrap();
        assert!(!wt_path.exists());

        // A raw `git worktree add` at the same path must now fail.
        let retry = Command::new("git")
            .args([
                "worktree",
                "add",
                "--detach",
                &wt_path.to_string_lossy(),
                "HEAD",
            ])
            .current_dir(&tmp)
            .output()
            .unwrap();
        assert!(
            !retry.status.success(),
            "raw git worktree add should fail on a stale entry"
        );

        // create() should clean up the stale entry and succeed.
        let wt = WorktreeSession::create(session_id, &tmp);
        assert!(wt.is_ok(), "create should recover from stale worktree");
        let wt = wt.unwrap();
        assert!(wt_path.exists(), "worktree should exist after recovery");

        drop(wt);
        assert!(!wt_path.exists(), "worktree should be removed after drop");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn worktree_create_rejects_path_traversal_session_id() {
        // Validation runs before any git spawn, so a dummy repo root is fine.
        let dummy = std::path::Path::new("/nonexistent-repo");
        for bad in ["", "..", "../escape", "a/b", "a\\b"] {
            let err = WorktreeSession::create(bad, dummy);
            assert!(
                err.is_err(),
                "session id `{bad}` should be rejected as a path-traversal risk"
            );
        }
    }
}
