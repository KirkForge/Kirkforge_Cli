/// Access control — deny lists, path guards, symlink protection, read-before-edit.
///
/// Two main types:
/// - [`DenyList`]: patterns always blocked (paths, URLs). Built from config.
/// - [`PathGuard`]: multi-layer safety checks for file read/write operations.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing;

mod deny_list;
pub use deny_list::{url_is_denied, DenyList};

// ── Path Guard ───────────────────────────────────────────────────────

/// Outcome of a path guard check.
///
/// A verdict must be inspected before proceeding; ignoring it would
/// bypass the safety checks the guard exists to enforce.
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub enum GuardVerdict {
    /// The operation is allowed. Contains the resolved path.
    Allowed(PathBuf),
    /// The operation is denied with a reason.
    Denied(String),
}

/// Multi-layer path safety guard.
///
/// Checks are applied in a fixed order so the error message is
/// deterministic: deny list → symlink → sandbox → size → binary
/// (reads) / extension → dotfile → size → symlink → allowed
/// directories (writes).
#[derive(Debug, Clone)]
pub struct PathGuard {
    /// If set, all file ops are restricted to this directory tree.
    pub sandbox_dir: Option<PathBuf>,
    /// File extensions blocked for write (e.g. `.pem`, `.key`).
    pub deny_extensions: Vec<String>,
    /// Block writes to dotfiles (files starting with `.`).
    pub block_dotfiles: bool,
    /// Block writes to dotfiles that are ignored by git.
    pub block_gitignored_dotfiles: bool,
    /// Maximum file size in bytes for reads (0 = unlimited).
    pub max_read_size: usize,
    /// Maximum size (in bytes) of an existing file that may be overwritten
    /// by `edit_file` or `write_file` (0 = unlimited).
    pub max_overwrite_size: usize,
    /// Deny list reference.
    pub deny_list: DenyList,
    /// If false, symlinks are rejected entirely.
    pub follow_symlinks: bool,
    /// If non-empty, only paths under these directories may be written.
    pub allowed_write_dirs: Vec<PathBuf>,
    /// If true, detect and reject binary file reads.
    pub block_binary_reads: bool,
}

impl PathGuard {
    /// Returns true if this guard has either a `sandbox_dir` or a non-empty
    /// `allowed_write_dirs`. The operator-facing `warn_if_unsandboxed`
    /// function (and the TUI startup banner) use this to decide whether
    /// to surface a config warning.
    pub fn is_sandboxed(&self) -> bool {
        self.sandbox_dir.is_some() || !self.allowed_write_dirs.is_empty()
    }

    /// Returns true if `path` is ignored by git in the working directory.
    ///
    /// Uses `git check-ignore --quiet <path>`. If git is unavailable, the
    /// directory is not a repo, or the path is not ignored, returns `false`.
    /// This is intentionally fail-open: the block happens only when we can
    /// positively confirm the dotfile is git-ignored.
    ///
    /// This is an async method so the subprocess does not block the Tokio
    /// runtime while `git` starts up or while the working directory is on a
    /// slow filesystem.
    async fn is_gitignored(&self, path: &Path) -> bool {
        let workdir = self
            .sandbox_dir
            .as_deref()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let path = path.to_path_buf();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async move {
            match tokio::process::Command::new("git")
                .arg("check-ignore")
                .arg("--quiet")
                .arg(&path)
                .current_dir(workdir)
                .status()
                .await
            {
                Ok(status) => status.success(),
                Err(_) => false,
            }
        })
        .await;
        result.unwrap_or(false)
    }

    /// Check whether `path` may be traversed (listed or read).
    ///
    /// This is the lightweight subset of `check_read`: deny list, existence,
    /// symlink guard, and sandbox containment. It does **not** enforce size
    /// limits or binary detection, so it is suitable for directory-listing
    /// tools (`glob`) that only need to know a path is visible.
    ///
    /// On success returns the canonicalized path.
    pub fn check_traversal(&self, path: &Path) -> GuardVerdict {
        // 1. Deny list
        if self.deny_list.is_path_denied(path) {
            return GuardVerdict::Denied(format!("Path denied by deny list: {}", path.display()));
        }

        if !path.exists() {
            return GuardVerdict::Denied(format!("Path does not exist: {}", path.display()));
        }

        // 2. Symlink guard — check before resolving the target so a
        // symlink outside the sandbox cannot influence the sandbox check
        // and so the denial reason is accurate.
        if !self.follow_symlinks {
            let metadata = match std::fs::symlink_metadata(path) {
                Ok(m) => m,
                Err(e) => {
                    return GuardVerdict::Denied(format!(
                        "Cannot read metadata for '{}': {e}",
                        path.display()
                    ));
                }
            };
            if metadata.file_type().is_symlink() {
                return GuardVerdict::Denied(format!("Symlinks not allowed: {}", path.display()));
            }
        }

        // 3. Sandbox containment
        let canonical = match path.canonicalize() {
            Ok(c) => c,
            Err(e) => {
                return GuardVerdict::Denied(format!(
                    "Cannot resolve path '{}': {e}",
                    path.display()
                ));
            }
        };

        // 3b. When we follow symlinks, the original deny-list check may not
        //     have caught the canonical target. Re-check the resolved path so
        //     a symlink inside the sandbox cannot point to a denied file.
        if self.follow_symlinks && self.deny_list.is_path_denied(&canonical) {
            return GuardVerdict::Denied(format!(
                "Resolved path denied by deny list: {}",
                canonical.display()
            ));
        }

        if let Some(ref sandbox) = self.sandbox_dir {
            let sb = match sandbox.canonicalize() {
                Ok(s) => s,
                Err(e) => {
                    return GuardVerdict::Denied(format!(
                        "Cannot resolve sandbox dir '{}': {e}",
                        sandbox.display()
                    ));
                }
            };
            if !canonical.starts_with(&sb) {
                return GuardVerdict::Denied(format!("Path outside sandbox: {}", path.display()));
            }
        }

        GuardVerdict::Allowed(canonical)
    }

    /// Check whether `path` may be read.
    ///
    /// On success returns the resolved path (canonicalized if it exists).
    pub fn check_read(&self, path: &Path) -> GuardVerdict {
        let canonical = match self.check_traversal(path) {
            GuardVerdict::Allowed(c) => c,
            GuardVerdict::Denied(msg) => return GuardVerdict::Denied(msg),
        };

        // 4. Size limit
        // Use the canonical path's metadata, not the original path's. When
        // follow_symlinks is true the original may be a tiny symlink pointing
        // to a huge file outside the size limit; measuring the symlink itself
        // would let the oversized target through.
        if self.max_read_size > 0 {
            let metadata = match std::fs::metadata(&canonical) {
                Ok(m) => m,
                Err(e) => {
                    return GuardVerdict::Denied(format!(
                        "Cannot read metadata for '{}': {e}",
                        canonical.display()
                    ));
                }
            };
            if metadata.len() > self.max_read_size as u64 {
                return GuardVerdict::Denied(format!(
                    "File too large ({} > {} bytes): {}",
                    metadata.len(),
                    self.max_read_size,
                    canonical.display()
                ));
            }
        }

        // 5. Binary file detection
        if self.block_binary_reads {
            let sample = match std::fs::read(&canonical) {
                Ok(data) => data,
                Err(e) => {
                    return GuardVerdict::Denied(format!(
                        "Cannot read '{}' for binary detection: {e}",
                        canonical.display()
                    ));
                }
            };
            if Self::is_binary(&sample) {
                return GuardVerdict::Denied(format!(
                    "Binary file blocked: {}",
                    canonical.display()
                ));
            }
        }

        GuardVerdict::Allowed(canonical)
    }

    /// Check whether `path` may be written (created or overwritten).
    ///
    /// Handles non-existent paths by checking the parent directory
    /// for sandbox containment.
    ///
    /// This is async because the git-ignore probe needs to spawn a subprocess
    /// without blocking the Tokio runtime.
    pub async fn check_write(&self, path: &Path) -> GuardVerdict {
        // 1. Deny list
        if self.deny_list.is_path_denied(path) {
            return GuardVerdict::Denied(format!("Path denied by deny list: {}", path.display()));
        }

        // 2. Sandbox containment
        if let Some(ref sandbox) = self.sandbox_dir {
            let sb = match sandbox.canonicalize() {
                Ok(s) => s,
                Err(e) => {
                    return GuardVerdict::Denied(format!(
                        "Cannot resolve sandbox dir '{}': {e}",
                        sandbox.display()
                    ));
                }
            };
            // For new files the path doesn't exist yet — check parent directory.
            // If we cannot resolve the path (or its parent), deny rather than
            // falling back to the unresolved literal — a symlink/mount race
            // could otherwise make `starts_with` pass on a path that escapes
            // the sandbox.
            let check = if path.exists() {
                match path.canonicalize() {
                    Ok(c) => c,
                    Err(e) => {
                        return GuardVerdict::Denied(format!(
                            "Cannot resolve path '{}': {e} (refusing write outside sandbox)",
                            path.display()
                        ));
                    }
                }
            } else {
                let parent = path.parent().unwrap_or(Path::new("."));
                match parent.canonicalize() {
                    Ok(c) => c,
                    Err(e) => {
                        return GuardVerdict::Denied(format!(
                            "Cannot resolve parent directory '{}': {e} (refusing write outside sandbox)",
                            parent.display()
                        ));
                    }
                }
            };
            if !check.starts_with(&sb) {
                return GuardVerdict::Denied(format!("Path outside sandbox: {}", path.display()));
            }
        }

        // 3. Extension deny list
        if let Some(ext) = path.extension() {
            let ext_str = format!(".{}", ext.to_string_lossy().to_lowercase());
            if self.deny_extensions.iter().any(|d| d == &ext_str) {
                return GuardVerdict::Denied(format!(
                    "Extension '{}' denied for write: {}",
                    ext_str,
                    path.display()
                ));
            }
        }

        // 4. Dotfile block
        if self.block_dotfiles {
            if let Some(name) = path.file_name() {
                if name.to_string_lossy().starts_with('.') {
                    return GuardVerdict::Denied(format!(
                        "Dotfiles not allowed: {}",
                        path.display()
                    ));
                }
            }
        }

        // 4b. Git-ignored dotfile block (default on). Fail-open when git is
        //     unavailable so we do not accidentally block writes in a non-repo.
        if self.block_gitignored_dotfiles {
            if let Some(name) = path.file_name() {
                if name.to_string_lossy().starts_with('.') && self.is_gitignored(path).await {
                    return GuardVerdict::Denied(format!(
                        "Git-ignored dotfile write blocked: {}. \
                         Add an explicit permission rule if you really need this file.",
                        path.display()
                    ));
                }
            }
        }

        // 5. Large-file overwrite guard. We check the existing file size so
        //    the model cannot silently clobber a large asset.
        if self.max_overwrite_size > 0 && path.exists() {
            let metadata = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(e) => {
                    return GuardVerdict::Denied(format!(
                        "Cannot read metadata for '{}': {e}",
                        path.display()
                    ));
                }
            };
            if metadata.len() > self.max_overwrite_size as u64 {
                return GuardVerdict::Denied(format!(
                    "Refusing to overwrite {} ({} bytes) because it exceeds the {}-byte limit",
                    path.display(),
                    metadata.len(),
                    self.max_overwrite_size
                ));
            }
        }

        // 6. Symlink guard for existing files (don't follow symlinks on write)
        if path.exists() {
            let metadata = match std::fs::symlink_metadata(path) {
                Ok(m) => m,
                Err(e) => {
                    return GuardVerdict::Denied(format!(
                        "Cannot read metadata for '{}': {e}",
                        path.display()
                    ));
                }
            };
            if metadata.file_type().is_symlink() {
                return GuardVerdict::Denied(format!(
                    "Cannot write through symlink: {}",
                    path.display()
                ));
            }
        }

        // 6. Allowed write directories
        if !self.allowed_write_dirs.is_empty() {
            let check = if path.exists() {
                match path.canonicalize() {
                    Ok(c) => c,
                    Err(e) => {
                        return GuardVerdict::Denied(format!(
                            "Cannot resolve path '{}': {e} (refusing write outside allowed dirs)",
                            path.display()
                        ));
                    }
                }
            } else {
                let parent = path.parent().unwrap_or(Path::new("."));
                match parent.canonicalize() {
                    Ok(c) => c,
                    Err(e) => {
                        return GuardVerdict::Denied(format!(
                            "Cannot resolve parent directory '{}': {e} (refusing write outside allowed dirs)",
                            parent.display()
                        ));
                    }
                }
            };
            let ok = self.allowed_write_dirs.iter().any(|d| {
                d.canonicalize()
                    .ok()
                    .is_some_and(|cd| check.starts_with(&cd))
            });
            if !ok {
                return GuardVerdict::Denied(format!(
                    "Path not in allowed write directories: {}",
                    path.display()
                ));
            }
        }

        GuardVerdict::Allowed(path.to_path_buf())
    }

    /// Quick binary detection by scanning the first 512 bytes.
    pub fn is_binary(content: &[u8]) -> bool {
        if content.is_empty() {
            return false;
        }
        let sample = if content.len() > 512 {
            &content[..512]
        } else {
            content
        };
        sample.contains(&0x00)
    }
}

impl Default for PathGuard {
    fn default() -> Self {
        Self {
            sandbox_dir: None,
            max_overwrite_size: 1024 * 1024,
            deny_extensions: vec![
                ".pem".into(),
                ".key".into(),
                ".crt".into(),
                ".cert".into(),
                ".o".into(),
                ".so".into(),
                ".dll".into(),
                ".dylib".into(),
                ".exe".into(),
            ],
            block_dotfiles: false,
            block_gitignored_dotfiles: false,
            max_read_size: 1024 * 1024, // 1 MB
            deny_list: DenyList::default(),
            follow_symlinks: false,
            allowed_write_dirs: vec![],
            block_binary_reads: false,
        }
    }
}

// ── Read-Before-Edit Gate ────────────────────────────────────────────

/// Tracks which files have been read in the current session.
///
/// The read-before-edit gate requires that a file be explicitly read
/// (via the read_file tool) before it can be edited. This prevents
/// the model from blindly patching files it hasn't inspected.
#[derive(Debug, Clone, Default)]
pub struct ReadGate {
    read_files: HashSet<PathBuf>,
}

impl ReadGate {
    pub fn new() -> Self {
        Self {
            read_files: HashSet::new(),
        }
    }

    /// Record that `path` was read (after path-guard approval).
    ///
    /// Prefer passing the canonical path returned by [`PathGuard::check_read`]
    /// so the gate stores the resolved real path rather than a user-supplied
    /// literal that may contain relative components or symlinks.
    pub fn mark_read(&mut self, path: &Path) {
        // Canonicalize if possible for consistent matching
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.read_files.insert(key);
    }

    /// Returns true if `path` was previously read this session.
    pub fn was_read(&self, path: &Path) -> bool {
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.read_files.contains(&key)
    }

    /// Check that `path` was read before edit. Returns a denial reason
    /// if not.
    ///
    /// `resolved` is the canonical path produced by the path guard (e.g. the
    /// return value of [`PathGuard::check_read`] or [`PathGuard::check_write`]).
    /// Using the resolved path avoids a second canonicalization round and
    /// prevents a malicious/ambiguous literal path from bypassing the gate.
    pub fn check_edit(&self, path: &Path, resolved: &Path) -> GuardVerdict {
        let key = resolved
            .canonicalize()
            .unwrap_or_else(|_| resolved.to_path_buf());
        if self.read_files.contains(&key) {
            GuardVerdict::Allowed(path.to_path_buf())
        } else {
            GuardVerdict::Denied(format!(
                "Read-before-edit: '{}' has not been read this session. \
                 Use read_file first.",
                path.display()
            ))
        }
    }

    /// Clear all read marks (e.g. for a new session).
    pub fn clear(&mut self) {
        self.read_files.clear();
    }
}

// ── Helper: Construct from Config ────────────────────────────────────

/// Emit a startup warning when the path guard is unsandboxed.
///
/// If neither `sandbox_dir` nor `allowed_write_dirs` is configured, every
/// write falls through to `GuardVerdict::Allowed`. This is a fail-open
/// design choice that the operator should be aware of — model-driven
/// writes can then touch anything the user can touch (subject to the
/// deny list and the deny-extension list).
///
/// Emits a single `tracing::warn!` with the remediation. Safe to call
/// repeatedly in tests; the warning is gated so it only fires when the
/// guard is actually unsandboxed.
pub fn warn_if_unsandboxed(path_guard: &PathGuard) {
    if !path_guard.is_sandboxed() {
        tracing::warn!(
            "PathGuard is unsandboxed: no `sandbox_dir` and no `allowed_write_dirs` configured. \
             Model-driven writes are not restricted to any directory tree (only the deny list and \
             deny extensions apply). Set `sandbox_dir` in config.toml or via KIRKFORGE_SANDBOX_DIR, \
             or list `allowed_write_dirs` to scope writes."
        );
    }
}

/// Build a [`DenyList`] and [`PathGuard`] from the user's config.
///
/// Merges configured deny patterns with safe defaults so that .ssh, .git,
/// and credential files are always blocked unless explicitly overridden.
pub fn access_from_config(config: &crate::shared::Config) -> (DenyList, PathGuard, ReadGate) {
    // Start with safe defaults, then merge configured patterns on top
    let mut base = DenyList::default();
    base.path_patterns.extend(config.deny_paths.clone());
    base.url_patterns.extend(config.deny_urls.clone());
    let deny_list = DenyList::new(base.path_patterns, base.url_patterns);

    let sandbox_dir = config
        .sandbox_dir
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    let allowed_dirs: Vec<PathBuf> = config
        .allowed_write_dirs
        .iter()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();

    // Merge deny extensions with defaults
    let mut deny_extensions: Vec<String> = PathGuard::default().deny_extensions;
    deny_extensions.extend(config.deny_extensions.clone());

    let path_guard = PathGuard {
        sandbox_dir,
        deny_extensions,
        block_dotfiles: config.block_dotfiles,
        block_gitignored_dotfiles: config.block_gitignored_dotfiles,
        max_read_size: config.max_file_read_size,
        max_overwrite_size: config.max_overwrite_size,
        deny_list: deny_list.clone(),
        follow_symlinks: config.follow_symlinks,
        allowed_write_dirs: allowed_dirs,
        block_binary_reads: config.block_binary_reads,
    };

    let read_gate = ReadGate::new();

    (deny_list, path_guard, read_gate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::remove_test_file;

    // ── DenyList ────────────────────────────────────────────────────

    #[test]
    fn test_deny_list_default_blocks_ssh() {
        let dl = DenyList::default();
        assert!(dl.is_path_denied(Path::new("/home/user/.ssh/id_rsa")));
        assert!(dl.is_path_denied(Path::new(".ssh/config")));
    }

    /// Invalid glob patterns are logged and skipped; valid patterns still work.
    #[test]
    fn test_deny_list_skips_invalid_globs() {
        let dl = DenyList::new(
            vec!["**/.ssh/**".into(), "[invalid".into(), "*.key".into()],
            vec![],
        );
        // Valid patterns should still match.
        assert!(dl.is_path_denied(Path::new("/home/user/.ssh/id_rsa")));
        assert!(dl.is_path_denied(Path::new("secret.key")));
        // The invalid pattern does not cause a panic and matches nothing.
        assert!(!dl.is_path_denied(Path::new("[invalid")));
        assert!(!dl.is_path_denied(Path::new("some.file")));
    }

    #[test]
    fn test_deny_list_default_blocks_git() {
        let dl = DenyList::default();
        assert!(dl.is_path_denied(Path::new(".git/config")));
        assert!(dl.is_path_denied(Path::new("/repo/.git/objects/pack/abc")));
    }

    #[test]
    fn test_deny_list_blocks_pem_key_crt() {
        let dl = DenyList::default();
        assert!(dl.is_path_denied(Path::new("server.key")));
        assert!(dl.is_path_denied(Path::new("certs/chain.pem")));
        assert!(dl.is_path_denied(Path::new("/etc/ssl/cert.crt")));
    }

    #[test]
    fn test_deny_list_allows_normal_source() {
        let dl = DenyList::default();
        assert!(!dl.is_path_denied(Path::new("src/main.rs")));
        assert!(!dl.is_path_denied(Path::new("/home/user/project/Cargo.toml")));
        assert!(!dl.is_path_denied(Path::new("/tmp/test.txt")));
    }

    #[test]
    fn test_deny_list_blocks_urls() {
        let dl = DenyList::default();
        assert!(dl.is_url_denied("http://169.254.169.254/latest/meta-data/"));
        assert!(dl.is_url_denied("http://metadata.google.internal/computeMetadata/v1/"));
        assert!(!dl.is_url_denied("https://api.github.com/repos/user/repo"));
    }

    #[test]
    fn test_deny_list_custom_patterns() {
        let dl = DenyList::new(vec!["**/secret/**".into()], vec![]);
        assert!(dl.is_path_denied(Path::new("project/secret/config.json")));
        assert!(!dl.is_path_denied(Path::new("project/src/main.rs")));
    }

    // ── PathGuard ───────────────────────────────────────────────────

    #[test]
    fn test_path_guard_deny_list_checked() {
        let guard = PathGuard {
            deny_list: DenyList::new(vec!["**/.git/**".into()], vec![]),
            ..Default::default()
        };
        let result = guard.check_read(Path::new(".git/config"));
        assert!(matches!(result, GuardVerdict::Denied(_)));
    }

    #[tokio::test]
    async fn test_path_guard_blocks_denied_extensions() {
        let guard = PathGuard {
            deny_extensions: vec![".bad".into(), ".evil".into()],
            deny_list: DenyList::new(vec![], vec![]), // no default deny list (which includes .pem/.key)
            ..Default::default()
        };
        let result = guard.check_write(Path::new("script.bad")).await;
        assert!(matches!(result, GuardVerdict::Denied(msg) if msg.contains("Extension")));
        let result2 = guard.check_write(Path::new("file.evil")).await;
        assert!(matches!(result2, GuardVerdict::Denied(msg) if msg.contains("Extension")));
        // Normal extension should pass
        let result3 = guard.check_write(Path::new("normal.txt")).await;
        assert!(!matches!(result3, GuardVerdict::Denied(_)));
    }

    #[tokio::test]
    async fn test_path_guard_blocks_dotfiles() {
        let guard = PathGuard {
            block_dotfiles: true,
            ..Default::default()
        };
        let result = guard.check_write(Path::new(".hidden")).await;
        assert!(matches!(result, GuardVerdict::Denied(msg) if msg.contains("Dotfiles")));
    }

    #[test]
    fn test_path_guard_allows_dotfiles_when_not_blocked() {
        let guard = PathGuard {
            block_dotfiles: false,
            ..Default::default()
        };
        // This writes to tmp so the path exists for the read check
        let tmp = std::env::temp_dir().join(".kirkforge_test_dotfile");
        std::fs::write(&tmp, "test").unwrap();
        let result = guard.check_read(&tmp);
        remove_test_file(&tmp);
        assert!(matches!(result, GuardVerdict::Allowed(_)));
    }

    #[test]
    fn test_path_guard_allows_normal_files() {
        let guard = PathGuard::default();
        let tmp = std::env::temp_dir().join("kirkforge_test_normal.txt");
        std::fs::write(&tmp, "hello").unwrap();
        let result = guard.check_read(&tmp);
        remove_test_file(&tmp);
        assert!(matches!(result, GuardVerdict::Allowed(_)));
    }

    #[cfg(unix)]
    #[test]
    fn test_check_read_size_limit_follows_symlink_target() {
        let dir = std::env::temp_dir().join("kirkforge_guard_symlink_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();

        let target = dir.join("large_target.txt");
        let link = dir.join("tiny_link");

        // Target is larger than the default max_read_size (1 MiB).
        let oversized = vec![b'x'; 1024 * 1024 + 1];
        std::fs::write(&target, &oversized).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let guard = PathGuard {
            follow_symlinks: true,
            ..Default::default()
        };

        // The symlink itself is tiny, but the canonical target is oversized.
        let result = guard.check_read(&link);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(result, GuardVerdict::Denied(ref msg) if msg.contains("File too large")),
            "expected denial based on target size, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_path_guard_write_for_nonexistent_path() {
        let guard = PathGuard::default();
        let tmp = std::env::temp_dir().join("kirkforge_new_file_test.txt");
        // Should not exist for this test
        remove_test_file(&tmp);
        let result = guard.check_write(&tmp).await;
        // Allowed since it's in tmp (no sandbox, no allow list restrictions)
        assert!(matches!(result, GuardVerdict::Allowed(_)));
        // Cleanup
        remove_test_file(&tmp);
    }

    #[test]
    fn test_is_binary_detects_null_bytes() {
        assert!(PathGuard::is_binary(&[0x00, 0x01, 0x02]));
        assert!(!PathGuard::is_binary(&[0x01, 0x02, 0x03]));
        assert!(PathGuard::is_binary(&[0x68, 0x00, 0x6c])); // "h\0l"
    }

    #[test]
    fn test_is_binary_empty_is_not_binary() {
        assert!(!PathGuard::is_binary(&[]));
    }

    // ── ReadGate ────────────────────────────────────────────────────

    #[test]
    fn test_read_gate_tracks_reads() {
        let mut gate = ReadGate::new();
        let p = Path::new("/tmp/kirkforge_read_gate_test.txt");
        // Not read yet
        assert!(matches!(gate.check_edit(p, p), GuardVerdict::Denied(_)));
        // Mark as read
        gate.mark_read(p);
        assert!(matches!(gate.check_edit(p, p), GuardVerdict::Allowed(_)));
    }

    #[test]
    fn test_read_gate_clear() {
        let mut gate = ReadGate::new();
        let p = Path::new("/tmp/kirkforge_clear_test.txt");
        gate.mark_read(p);
        gate.clear();
        assert!(matches!(gate.check_edit(p, p), GuardVerdict::Denied(_)));
    }

    #[test]
    fn test_read_gate_uses_resolved_path_for_lookup() {
        // The gate should match the resolved canonical key even when the
        // display path and the resolved path differ in form.
        let mut gate = ReadGate::new();
        let display = Path::new("/tmp/../tmp/kirkforge_resolved_test.txt");
        let resolved = Path::new("/tmp/kirkforge_resolved_test.txt");
        gate.mark_read(resolved);
        assert!(matches!(
            gate.check_edit(display, resolved),
            GuardVerdict::Allowed(_)
        ));

        // A different resolved key that was never read should still be denied.
        let other = Path::new("/tmp/kirkforge_other_test.txt");
        assert!(matches!(
            gate.check_edit(display, other),
            GuardVerdict::Denied(_)
        ));
    }

    // ── warn_if_unsandboxed ─────────────────────────────────────────

    #[test]
    fn test_warn_if_unsandboxed_default_guard_is_quiet_in_test() {
        // We can't easily assert a tracing::warn! was emitted without
        // installing a custom subscriber, but we can at least verify
        // the function is callable on the default guard (no panic,
        // no side effect beyond logging).
        let guard = PathGuard::default();
        warn_if_unsandboxed(&guard);
    }

    #[test]
    fn test_warn_if_unsandboxed_with_sandbox_is_quiet() {
        // A guard with only sandbox_dir set: should not warn.
        let guard = PathGuard {
            sandbox_dir: Some(PathBuf::from("/tmp")),
            ..Default::default()
        };
        warn_if_unsandboxed(&guard);
    }

    #[test]
    fn test_warn_if_unsandboxed_with_allowlist_is_quiet() {
        // A guard with only allowed_write_dirs set: should not warn.
        let guard = PathGuard {
            allowed_write_dirs: vec![PathBuf::from("/tmp")],
            ..Default::default()
        };
        warn_if_unsandboxed(&guard);
    }

    // ── Default contract (pinned by tests; changing these is a breaking change)

    /// **Contract:** `PathGuard::default()` is **fail-open** — it does NOT
    /// restrict writes. This is a deliberate operator choice: the deny list
    /// and deny extensions still block the obviously-dangerous paths, but
    /// without a sandbox or an `allowed_write_dirs` list, model-driven
    /// writes fall through to `GuardVerdict::Allowed`. Operators who want
    /// strict containment must call `access_from_config` (which honours
    /// the config file) or set `sandbox_dir` / `allowed_write_dirs` on the
    /// guard explicitly. The startup `warn_if_unsandboxed` banner is the
    /// the operator's signal that they're running unsandboxed.
    ///
    /// **Do not change `Default` to fail-closed** without coordinating with
    /// the existing call sites — see `test_path_guard_default_is_fail_open`
    /// below for the cases that depend on this.
    #[tokio::test]
    async fn test_path_guard_default_is_fail_open() {
        let guard = PathGuard::default();
        assert!(
            !guard.is_sandboxed(),
            "default PathGuard is unsandboxed by design"
        );
        // Writes to /tmp are allowed (no sandbox, no allowlist blocking it).
        let tmp = std::env::temp_dir().join("kirkforge_default_failopen.txt");
        remove_test_file(&tmp);
        let result = guard.check_write(&tmp).await;
        remove_test_file(&tmp);
        assert!(
            matches!(result, GuardVerdict::Allowed(_)),
            "default PathGuard allows writes to /tmp; if this changes, the \
             test name `is_fail_open` no longer matches reality and several \
             call sites in tests/ will need to be updated"
        );
        // But the deny list and deny extensions still apply — even
        // unsandboxed, .ssh and .pem are blocked.
        let ssh_like = std::env::temp_dir().join("kirkforge.pem");
        let result = guard.check_write(&ssh_like).await;
        assert!(
            matches!(result, GuardVerdict::Denied(_)),
            "default PathGuard still applies the deny-extension list: .pem must be blocked"
        );
    }

    /// **Contract:** `is_sandboxed()` is true iff `sandbox_dir` is `Some`
    /// OR `allowed_write_dirs` is non-empty. Both count; either alone is
    /// sufficient.
    #[test]
    fn test_path_guard_is_sandboxed_predicate() {
        let bare = PathGuard::default();
        assert!(!bare.is_sandboxed());

        let with_sandbox = PathGuard {
            sandbox_dir: Some(PathBuf::from("/tmp")),
            ..Default::default()
        };
        assert!(with_sandbox.is_sandboxed());

        let with_allowlist = PathGuard {
            allowed_write_dirs: vec![PathBuf::from("/tmp")],
            ..Default::default()
        };
        assert!(with_allowlist.is_sandboxed());

        // An empty allowed_write_dirs does NOT count as sandboxed.
        let empty_allowlist = PathGuard {
            allowed_write_dirs: vec![],
            ..Default::default()
        };
        assert!(!empty_allowlist.is_sandboxed());
    }

    /// **Contract:** `Config::default()` now sandboxes to the current
    /// working directory. Operators who want unsandboxed operation must
    /// explicitly opt out via `sandbox_dir = ""` in the config file (or
    /// `KIRKFORGE_SANDBOX_DIR=""` env var); `access_from_config` treats
    /// the empty string as `None`. This replaces the prior fail-open
    /// default, which was the source of GPT 5.5's review finding #5
    /// ("Default mode is fail-open for writes").
    /// Guard the user-visible invariant: when a KirkForge session
    /// starts in a typical directory, the resulting `PathGuard` is
    /// sandboxed to that directory.
    ///
    /// Review.md arch concern #3 moved the cwd resolution out of
    /// `Config::default()` into a new `freeze_launch_sandbox` helper
    /// in `session::config`. The test now exercises that helper
    /// explicitly — it's the actual launch path, where the policy
    /// takes effect. The old test (which asserted the same property
    /// on `Config::default()` directly) was guarding a now-defunct
    /// code path.
    ///
    /// History: the previous fail-open default (sandbox_dir = None
    /// when cwd was unreachable) was the source of GPT 5.5's review
    /// finding #5 ("Default mode is fail-open for writes"). The new
    /// `freeze_launch_sandbox` surfaces a `current_dir()` failure
    /// as `None` (still) but does so from a single, testable call
    /// site, with a tracking comment that explains the policy.
    #[test]
    fn test_launch_path_sandboxes_to_cwd_by_default() {
        use crate::session::config::freeze_launch_sandbox;

        // Default Config has no sandbox_dir — operator didn't set
        // it. After `freeze_launch_sandbox`, it should be filled
        // in with the resolved cwd. This is the typical launch
        // case the user sees.
        let mut config = crate::shared::Config::default();
        assert!(
            config.sandbox_dir.is_none(),
            "Config::default() must NOT pre-resolve cwd — that's the \
             review.md arch concern #3 fix. Resolution happens at \
             launch time in `freeze_launch_sandbox`."
        );

        // Launch path. In the unit-test runtime, cwd is always
        // present, so the helper fills in some path.
        freeze_launch_sandbox(&mut config);
        let (_deny, guard, _gate) = access_from_config(&config);
        assert!(
            guard.is_sandboxed(),
            "After freeze_launch_sandbox, the launch path must produce a \
             sandboxed guard. The user-visible invariant — sandboxed by \
             default — is what we care about; the helper is the new \
             single resolution site."
        );

        // Explicit escape hatch: empty string in config = unsandboxed.
        // The helper must not overwrite it.
        let mut config_unsandboxed = crate::shared::Config {
            sandbox_dir: Some(String::new()),
            seed: None,
            ..crate::shared::Config::default()
        };
        freeze_launch_sandbox(&mut config_unsandboxed);
        let (_deny, guard_unsandboxed, _gate) = access_from_config(&config_unsandboxed);
        assert!(
            !guard_unsandboxed.is_sandboxed(),
            "Setting sandbox_dir = Some(\"\") in config is the explicit \
             escape hatch; freeze_launch_sandbox must not overwrite it"
        );
        assert_eq!(
            config_unsandboxed.sandbox_dir.as_deref(),
            Some(""),
            "freeze_launch_sandbox must leave an explicit-empty sandbox_dir alone"
        );

        // `None` is also the escape hatch — same path, different
        // spelling (e.g. resolved from `KIRKFORGE_SANDBOX_DIR=""`).
        let mut config_none = crate::shared::Config::default();
        // For the test, simulate the "KIRKFORGE_SANDBOX_DIR=\"\""
        // path by clearing the field *after* the helper would
        // normally have filled it. The user-facing property is
        // that `None` produces an unsandboxed guard.
        freeze_launch_sandbox(&mut config_none);
        config_none.sandbox_dir = None;
        let (_deny, guard_none, _gate) = access_from_config(&config_none);
        assert!(
            !guard_none.is_sandboxed(),
            "sandbox_dir = None must produce an unsandboxed guard"
        );
    }

    // ── Git-ignored dotfile blocking ────────────────────────────────

    fn setup_gitignored_dotfile_repo(suffix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kirkforge_guard_gitignored_test_{suffix}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Minimal git repo.
        std::process::Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(&dir)
            .output()
            .expect("git init should succeed");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&dir)
            .output()
            .unwrap();

        // Ignore a dotfile name that is NOT in the default deny list so the
        // git-ignored check is the one that fires.
        std::fs::write(dir.join(".gitignore"), ".ignored_local\n").unwrap();

        dir
    }

    #[tokio::test]
    async fn test_path_guard_blocks_gitignored_dotfiles() {
        let dir = setup_gitignored_dotfile_repo("blocked");
        let guard = PathGuard {
            sandbox_dir: Some(dir.clone()),
            block_gitignored_dotfiles: true,
            ..Default::default()
        };

        let result = guard.check_write(&dir.join(".ignored_local")).await;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(result, GuardVerdict::Denied(ref msg) if msg.contains("Git-ignored dotfile")),
            "expected git-ignored dotfile denial, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_path_guard_allows_non_gitignored_dotfiles() {
        let dir = setup_gitignored_dotfile_repo("nonignored");
        let guard = PathGuard {
            sandbox_dir: Some(dir.clone()),
            block_gitignored_dotfiles: true,
            ..Default::default()
        };

        // `.tracked` is a dotfile but is not git-ignored.
        let result = guard.check_write(&dir.join(".tracked")).await;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(result, GuardVerdict::Allowed(_)),
            "expected non-gitignored dotfile to be allowed, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_path_guard_allows_gitignored_dotfiles_when_disabled() {
        let dir = setup_gitignored_dotfile_repo("disabled");
        let guard = PathGuard {
            sandbox_dir: Some(dir.clone()),
            block_gitignored_dotfiles: false,
            ..Default::default()
        };

        let result = guard.check_write(&dir.join(".ignored_local")).await;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(result, GuardVerdict::Allowed(_)),
            "expected ignored dotfile allowed when disabled, got {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_check_read_rechecks_deny_list_on_canonical_target() {
        let dir = std::env::temp_dir().join("kirkforge_deny_symlink_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();

        let target = dir.join("secret.pem");
        let link = dir.join("safe_link");
        std::fs::write(&target, "secret").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let guard = PathGuard {
            sandbox_dir: Some(dir.clone()),
            follow_symlinks: true,
            ..Default::default()
        };

        // The symlink name is not denied, but its canonical target is *.pem.
        let result = guard.check_read(&link);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(result, GuardVerdict::Denied(ref msg) if msg.contains("secret.pem")),
            "expected denial of symlink target, got {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_check_traversal_rechecks_deny_list_on_canonical_target() {
        let dir = std::env::temp_dir().join("kirkforge_traversal_deny_symlink_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();

        let target = dir.join("secret.pem");
        let link = dir.join("safe_link");
        std::fs::write(&target, "secret").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let guard = PathGuard {
            sandbox_dir: Some(dir.clone()),
            follow_symlinks: true,
            ..Default::default()
        };

        let result = guard.check_traversal(&link);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(result, GuardVerdict::Denied(ref msg) if msg.contains("secret.pem")),
            "expected traversal denial of symlink target, got {result:?}"
        );
    }
}
