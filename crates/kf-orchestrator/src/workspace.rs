//! Workspace management (R5). Port of `orchestrator/src/orchestrator-workspace.ts`
//! + the `WorkspaceManager` class from `workspace.ts`.
//!
//! Provides isolated workspace creation (copy baseline cwd into a tempdir,
//! optionally overlay emitted files), baseline snapshotting for the
//! correction loop, and idempotent cleanup.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tempfile::TempDir;

/// Top-level dirs that get stripped when copying into a turn/baseline
/// workspace. Matches TS `TURN_COPY_EXCLUDED`.
const TURN_COPY_EXCLUDED: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    ".tsbuildinfo",
    "tsconfig.tsbuildinfo",
];

/// True if `src` (relative-or-absolute) should be excluded from a turn copy.
pub fn should_exclude_from_turn_copy(src: &str, base_len: usize) -> bool {
    if src.len() <= base_len {
        return false;
    }
    let rel = &src[base_len + 1..];
    if rel.is_empty() {
        return false;
    }
    for seg in rel.split('/') {
        if TURN_COPY_EXCLUDED.contains(&seg) {
            return true;
        }
    }
    let last = rel.split('/').next_back().unwrap_or("");
    TURN_COPY_EXCLUDED.contains(&last)
}

/// Copy `src` into `dst` recursively, skipping the dirs in
/// `TURN_COPY_EXCLUDED`. Does not dereference symlinks (matches TS
/// `dereference: false`). Returns the number of entries copied.
fn copy_dir_filtered(src: &Path, dst: &Path) -> Result<usize> {
    let mut count = 0;
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if TURN_COPY_EXCLUDED.contains(&name_str.as_ref()) {
            continue;
        }
        let ft = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if ft.is_dir() {
            count += copy_dir_filtered(&src_path, &dst_path)?;
        } else if ft.is_file() {
            fs::copy(&src_path, &dst_path)?;
            count += 1;
        } else if ft.is_symlink() {
            // ponytail: preserve symlinks as-is; dereference:false mirrors TS.
            let target = fs::read_link(&src_path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                let _ = symlink(&target, &dst_path);
            }
            #[cfg(not(unix))]
            {
                // No symlink primitive on non-Unix; fall back to copying the
                // target's contents (best-effort).
                if target.is_file() {
                    let _ = fs::copy(&target, &dst_path);
                }
            }
            count += 1;
        }
    }
    Ok(count)
}

/// Owns a workspace directory for the duration of a delegation / validation
/// run. `TempDir` ensures cleanup on drop.
pub struct IsolatedWorkspace {
    pub path: PathBuf,
    _dir: TempDir,
}

impl IsolatedWorkspace {
    pub fn new() -> Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("kirkforge-validator-")
            .tempdir()?;
        Ok(Self {
            path: dir.path().to_path_buf(),
            _dir: dir,
        })
    }

    /// Path as a `String` for `cd`-style consumers.
    pub fn path_str(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

/// File overlay spec. If `content` is set, write it directly; otherwise copy
/// from `baseline` if present.
#[derive(Debug, Clone)]
pub struct OverlaySpec {
    pub path: String,
    pub content: Option<String>,
}

/// Manages isolated workspace directories for validator execution. Port of
/// TS `WorkspaceManager`. Owns the baseline snapshot for the lifetime of
/// the manager (cleaned up on drop via `TempDir`).
pub struct WorkspaceManager {
    cwd: PathBuf,
    baseline: Option<IsolatedWorkspace>,
}

impl WorkspaceManager {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            baseline: None,
        }
    }

    /// Create a fresh isolated workspace. If `overlay` is non-empty, copy
    /// the baseline cwd first, then overlay each file. Otherwise just copy
    /// the baseline cwd.
    pub fn create_isolated(
        &self,
        overlay: &[OverlaySpec],
        baseline_dir: Option<&Path>,
    ) -> Result<IsolatedWorkspace> {
        let ws = IsolatedWorkspace::new()?;
        let baseline = baseline_dir.unwrap_or(self.cwd.as_path());
        copy_dir_filtered(baseline, &ws.path)?;
        for f in overlay {
            let dst = ws.path.join(&f.path);
            if let Some(content) = &f.content {
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&dst, content)?;
            } else {
                let src = baseline.join(&f.path);
                if src.exists() {
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(&src, &dst)?;
                }
            }
        }
        Ok(ws)
    }

    /// Snapshot the cwd once per manager instance. Subsequent calls return
    /// the cached snapshot.
    pub fn ensure_baseline(&mut self) -> Result<&IsolatedWorkspace> {
        if self.baseline.is_some() {
            return Ok(self.baseline.as_ref().unwrap());
        }
        let snap = IsolatedWorkspace::new()?;
        // ponytail: use a separate tempdir prefix for baseline dirs to mirror
        // TS's "kirkforge-baseline-" marker; both are tempdirs either way.
        let dst = snap.path.clone();
        copy_dir_filtered(&self.cwd, &dst)?;
        self.baseline = Some(snap);
        Ok(self.baseline.as_ref().unwrap())
    }

    /// Drop the cached baseline snapshot (next `ensure_baseline` recreates it).
    pub fn drop_baseline(&mut self) {
        self.baseline = None;
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn exclude_filters_known_dirs() {
        assert!(should_exclude_from_turn_copy(
            "/tmp/proj/node_modules/foo",
            "/tmp/proj".len()
        ));
        assert!(should_exclude_from_turn_copy(
            "/tmp/proj/.git",
            "/tmp/proj".len()
        ));
        assert!(!should_exclude_from_turn_copy(
            "/tmp/proj/src/main.rs",
            "/tmp/proj".len()
        ));
        assert!(!should_exclude_from_turn_copy(
            "/tmp/proj",
            "/tmp/proj".len()
        ));
    }

    #[test]
    fn isolated_workspace_copy_skips_excluded_dirs() {
        let base = tempfile::tempdir().unwrap();
        let base_path = base.path();
        write(&base_path.join("src/main.rs"), "fn main() {}");
        write(
            &base_path.join("node_modules/x/index.js"),
            "module.exports = 1",
        );
        write(&base_path.join(".git/config"), "[core]");

        let mgr = WorkspaceManager::new(base_path);
        let ws = mgr.create_isolated(&[], None).unwrap();
        assert!(ws.path.join("src/main.rs").exists());
        assert!(!ws.path.join("node_modules").exists());
        assert!(!ws.path.join(".git").exists());
    }

    #[test]
    fn overlay_writes_provided_content() {
        let base = tempfile::tempdir().unwrap();
        let base_path = base.path();
        write(&base_path.join("src/orig.rs"), "fn old() {}");

        let mgr = WorkspaceManager::new(base_path);
        let overlay = vec![OverlaySpec {
            path: "src/orig.rs".into(),
            content: Some("fn new() {}".into()),
        }];
        let ws = mgr.create_isolated(&overlay, None).unwrap();
        let body = fs::read_to_string(ws.path.join("src/orig.rs")).unwrap();
        assert_eq!(body, "fn new() {}");
    }

    #[test]
    fn overlay_copies_from_baseline_when_no_content() {
        let base = tempfile::tempdir().unwrap();
        let base_path = base.path();
        write(&base_path.join("a.txt"), "from-base");

        let mgr = WorkspaceManager::new(base_path);
        let overlay = vec![OverlaySpec {
            path: "a.txt".into(),
            content: None,
        }];
        let ws = mgr.create_isolated(&overlay, None).unwrap();
        let body = fs::read_to_string(ws.path.join("a.txt")).unwrap();
        assert_eq!(body, "from-base");
    }

    #[test]
    fn ensure_baseline_caches_first_call() {
        let base = tempfile::tempdir().unwrap();
        write(&base.path().join("a.txt"), "x");
        let mut mgr = WorkspaceManager::new(base.path());
        let p1 = mgr.ensure_baseline().unwrap().path_str();
        let p2 = mgr.ensure_baseline().unwrap().path_str();
        assert_eq!(p1, p2, "second ensure_baseline must return cached dir");
    }

    #[test]
    fn drop_baseline_forces_recreate() {
        let base = tempfile::tempdir().unwrap();
        write(&base.path().join("a.txt"), "x");
        let mut mgr = WorkspaceManager::new(base.path());
        let p1 = mgr.ensure_baseline().unwrap().path_str();
        mgr.drop_baseline();
        let p2 = mgr.ensure_baseline().unwrap().path_str();
        assert_ne!(p1, p2, "drop_baseline must force recreate");
    }
}
