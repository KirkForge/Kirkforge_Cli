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
        let worktree_path = std::env::temp_dir()
            .join(format!("kirkforge-session-{session_id}"));

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
            .args(["worktree", "remove", "--force", &self.worktree_path.to_string_lossy()])
            .current_dir(&self.original_path)
            .output();
        if let Err(e) = result {
            eprintln!("warning: failed to remove worktree at {}: {e}", self.worktree_path.display());
        }
    }
}
