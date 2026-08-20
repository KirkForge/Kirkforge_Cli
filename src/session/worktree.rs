use std::path::PathBuf;

/// Run one git command to completion, blocking. Shared by the async
/// wrapper and `Drop` (which cannot await).
fn git_output_sync(cwd: &std::path::Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
}

/// Run one git command on a blocking thread (WO 38.3). The calls are
/// short one-shots, but a hung hook or a slow NFS mount must not stall
/// an async worker — session startup awaits this.
async fn git_output(cwd: &std::path::Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    let cwd = cwd.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        git_output_sync(&cwd, &arg_refs)
            .map_err(|e| anyhow::anyhow!("failed to spawn git {args:?}: {e}"))
    })
    .await
    .map_err(|e| anyhow::anyhow!("git task join failed: {e}"))?
}

/// Manages an isolated git worktree for a session.
/// Created at session start, removed at session end.
pub struct WorktreeSession {
    worktree_path: PathBuf,
    original_path: PathBuf,
}

impl WorktreeSession {
    /// Create a new git worktree at a temp path for the given session id.
    /// Returns the worktree path and a guard that removes it on drop.
    pub async fn create(session_id: &str, repo_root: &std::path::Path) -> anyhow::Result<Self> {
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
        let worktree_str = worktree_path.to_string_lossy().to_string();

        let output = git_output(
            repo_root,
            &["worktree", "add", "--detach", &worktree_str, "HEAD"],
        )
        .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // A stale worktree from a crashed session occupies this path and
            // makes `git worktree add` fail. Remove it so the next attempt can
            // proceed instead of leaving the user stuck on every resume.
            let remove =
                git_output(repo_root, &["worktree", "remove", "--force", &worktree_str]).await;
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
            let retry = git_output(
                repo_root,
                &["worktree", "add", "--detach", &worktree_str, "HEAD"],
            )
            .await;
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

    /// Capture the subagent's uncommitted work as an appliable patch
    /// (WO 35.2). Must be called before Drop removes the worktree.
    ///
    /// Untracked files are folded in via `git add --intent-to-add` so they
    /// appear as new-file diffs; that also covers the `git status
    /// --porcelain` case from the workorder (no separate listing needed).
    /// Returns an empty string when the worktree is clean.
    pub async fn diff_patch(&self) -> String {
        let _ = git_output(&self.worktree_path, &["add", "--all", "--intent-to-add"]).await;
        match git_output(&self.worktree_path, &["diff", "HEAD", "--"]).await {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => String::new(),
        }
    }
}

impl Drop for WorktreeSession {
    fn drop(&mut self) {
        // ceiling: Drop cannot await, so the removal stays a blocking
        // call on the dropping thread (tests also assert synchronous
        // removal). A crash mid-remove is already recovered by create()'s
        // stale-worktree path. Upgrade path: fire-and-forget blocking
        // spawn if a dropped-from-async context ever measurably stalls.
        let result = git_output_sync(
            &self.original_path,
            &[
                "worktree",
                "remove",
                "--force",
                &self.worktree_path.to_string_lossy(),
            ],
        );
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
    use std::sync::Arc;

    // Shared setup for the WO 35.2 patch tests: a temp git repo with one
    // commit, plus a unique tag so concurrent tests never collide on
    // worktree paths (WorktreeSession derives its path from the id).
    // Worktree ids are globally-namespaced under $TMPDIR, so they must be
    // unique per process too — a crashed run's leftover would otherwise
    // poison the next run's `git worktree add` at the same path.
    fn wt_id(name: &str) -> String {
        format!("{name}-{}", std::process::id())
    }

    fn init_test_repo(tag: &str) -> std::path::PathBuf {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-wt-patch-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@test"],
            vec!["config", "user.name", "test"],
        ] {
            let out = Command::new("git")
                .args(&args)
                .current_dir(&tmp)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        }
        fs::write(tmp.join("tracked.txt"), "base\n").unwrap();
        fs::write(tmp.join("other.txt"), "other base\n").unwrap();
        for args in [vec!["add", "."], vec!["commit", "-m", "init"]] {
            let out = Command::new("git")
                .args(&args)
                .current_dir(&tmp)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        }
        tmp
    }

    fn git_run(repo: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    #[tokio::test]
    async fn worktree_diff_patch_covers_tracked_and_untracked_changes() {
        let repo = init_test_repo("single");
        let wt = WorktreeSession::create(&wt_id("patch-single"), &repo)
            .await
            .unwrap();
        // Tracked edit + untracked new file.
        fs::write(wt.path().join("tracked.txt"), "base\nedited by coder\n").unwrap();
        fs::write(wt.path().join("new_file.txt"), "brand new\n").unwrap();

        let patch = wt.diff_patch().await;
        assert!(patch.contains("--- a/tracked.txt"), "patch:\n{patch}");
        assert!(patch.contains("+++ b/tracked.txt"), "patch:\n{patch}");
        assert!(patch.contains("+++ b/new_file.txt"), "patch:\n{patch}");
        assert!(patch.contains("brand new"), "patch:\n{patch}");

        // The patch applies cleanly to a fresh checkout at the same HEAD.
        let clean = WorktreeSession::create(&wt_id("patch-clean"), &repo)
            .await
            .unwrap();
        let patch_file = repo.join("single.patch");
        fs::write(&patch_file, &patch).unwrap();
        git_run(
            clean.path(),
            &["apply", patch_file.to_string_lossy().as_ref()],
        );
        assert_eq!(
            fs::read_to_string(clean.path().join("tracked.txt")).unwrap(),
            "base\nedited by coder\n"
        );
        assert_eq!(
            fs::read_to_string(clean.path().join("new_file.txt")).unwrap(),
            "brand new\n"
        );

        // A clean worktree produces an empty patch.
        assert!(WorktreeSession::create(&wt_id("patch-empty"), &repo)
            .await
            .unwrap()
            .diff_patch()
            .await
            .trim()
            .is_empty());
        // Drop the worktree guards BEFORE deleting the repo: Drop runs
        // `git worktree remove` with the repo as CWD, which fails on a
        // deleted repo and would leave the temp worktree dir behind.
        drop(wt);
        let _ = fs::remove_dir_all(&repo);
    }

    // WO 35.2 gate: two concurrent coder worktrees produce disjoint,
    // sequentially-appliable patches, and Drop leaves no stale entries.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_concurrent_coder_worktrees_produce_disjoint_applicable_patches() {
        let repo = init_test_repo("concurrent");
        // Barrier guarantees both worktrees are alive simultaneously
        // (isolation is the point) before either records its edit.
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let edit_a = {
            let repo = repo.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let wt = WorktreeSession::create(&wt_id("coder-a"), &repo)
                    .await
                    .unwrap();
                barrier.wait().await;
                fs::write(wt.path().join("tracked.txt"), "base\ncoder A edit\n").unwrap();
                (wt.diff_patch().await, wt.path().clone())
            })
        };
        let edit_b = {
            let repo = repo.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let wt = WorktreeSession::create(&wt_id("coder-b"), &repo)
                    .await
                    .unwrap();
                barrier.wait().await;
                fs::write(wt.path().join("other.txt"), "other base\ncoder B edit\n").unwrap();
                (wt.diff_patch().await, wt.path().clone())
            })
        };
        // Both worktrees exist concurrently (isolation is the point).
        let (patch_a, path_a) = edit_a.await.unwrap();
        let (patch_b, path_b) = edit_b.await.unwrap();
        assert!(path_a != path_b);
        assert!(patch_a.contains("coder A edit"), "patch A:\n{patch_a}");
        assert!(
            !patch_a.contains("coder B edit"),
            "patch A leaked B:\n{patch_a}"
        );
        assert!(patch_b.contains("coder B edit"), "patch B:\n{patch_b}");
        assert!(
            !patch_b.contains("coder A edit"),
            "patch B leaked A:\n{patch_b}"
        );

        // Sequentially applied to a fresh checkout at the same HEAD: both
        // apply cleanly (disjoint files → no conflict).
        let clean = WorktreeSession::create(&wt_id("coder-clean"), &repo)
            .await
            .unwrap();
        let file_a = repo.join("a.patch");
        let file_b = repo.join("b.patch");
        fs::write(&file_a, &patch_a).unwrap();
        fs::write(&file_b, &patch_b).unwrap();
        git_run(clean.path(), &["apply", file_a.to_string_lossy().as_ref()]);
        git_run(clean.path(), &["apply", file_b.to_string_lossy().as_ref()]);
        assert!(fs::read_to_string(clean.path().join("tracked.txt"))
            .unwrap()
            .contains("coder A edit"));
        assert!(fs::read_to_string(clean.path().join("other.txt"))
            .unwrap()
            .contains("coder B edit"));
        drop(clean);

        // After all Drops, `git worktree list` shows no test worktrees.
        let list = git_run(&repo, &["worktree", "list"]);
        for stale in [&path_a, &path_b] {
            let norm = stale.to_string_lossy().replace('\\', "/");
            assert!(
                !list.replace('\\', "/").contains(&norm),
                "stale worktree entry: {norm}"
            );
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worktree_create_write_file_drop_cleanup() {
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
        let wt = WorktreeSession::create(session_id, &tmp).await.unwrap();
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

    #[tokio::test]
    async fn worktree_create_recovers_from_stale_worktree() {
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
        let wt = WorktreeSession::create(session_id, &tmp).await;
        assert!(wt.is_ok(), "create should recover from stale worktree");
        let wt = wt.unwrap();
        assert!(wt_path.exists(), "worktree should exist after recovery");

        drop(wt);
        assert!(!wt_path.exists(), "worktree should be removed after drop");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn worktree_create_rejects_path_traversal_session_id() {
        // Validation runs before any git spawn, so a dummy repo root is fine.
        let dummy = std::path::Path::new("/nonexistent-repo");
        for bad in ["", "..", "../escape", "a/b", "a\\b"] {
            let err = WorktreeSession::create(bad, dummy).await;
            assert!(
                err.is_err(),
                "session id `{bad}` should be rejected as a path-traversal risk"
            );
        }
    }
}
