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
        let worktree_path = std::env::temp_dir().join(format!("kirkforge-session-{session_id}"));

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
            anyhow::bail!("git worktree add failed: {stderr}");
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
            eprintln!(
                "warning: failed to remove worktree at {}: {e}",
                self.worktree_path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn worktree_create_write_file_drop_cleanup() {
        // Create a temp git repo
        let tmp = std::env::temp_dir().join(format!("kirkforge-wt-test-{}", std::process::id()));
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
        assert!(
            list.contains(wt_path.to_str().unwrap()),
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
        assert!(
            !list.contains(wt_path.to_str().unwrap()),
            "worktree list should not contain removed worktree:\n{list}"
        );

        // Cleanup temp repo
        let _ = fs::remove_dir_all(&tmp);
    }
}
