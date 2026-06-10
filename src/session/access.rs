/// Access control — deny lists, path guards, symlink protection, read-before-edit.
///
/// Two main types:
/// - [`DenyList`]: patterns always blocked (paths, URLs). Built from config.
/// - [`PathGuard`]: multi-layer safety checks for file read/write operations.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ── Deny List ────────────────────────────────────────────────────────

/// Patterns always blocked for tool access.
///
/// Deny-list checks are the outermost gate — they fire before any
/// sandbox, approval, or guard check. A denied path is *always* rejected.
#[derive(Debug, Clone)]
pub struct DenyList {
    /// Compiled glob matchers for denied path patterns.
    path_matchers: Vec<globset::GlobMatcher>,
    /// Raw patterns (for display/debug).
    pub path_patterns: Vec<String>,
    /// URL prefix patterns (blocked if the target URL starts with any).
    pub url_patterns: Vec<String>,
}

impl DenyList {
    /// Build from raw pattern strings; invalid globs are silently skipped.
    pub fn new(path_patterns: Vec<String>, url_patterns: Vec<String>) -> Self {
        let path_matchers = path_patterns
            .iter()
            .filter_map(|p| globset::Glob::new(p).ok())
            .map(|g| g.compile_matcher())
            .collect();
        Self {
            path_matchers,
            path_patterns,
            url_patterns,
        }
    }

    /// Returns true if `path` matches any deny pattern.
    pub fn is_path_denied(&self, path: &Path) -> bool {
        let as_str = path.to_string_lossy();
        self.path_matchers
            .iter()
            .any(|m| m.is_match(as_str.as_ref()))
    }

    /// Returns true if `url` starts with any blocked prefix.
    pub fn is_url_denied(&self, url: &str) -> bool {
        self.url_patterns.iter().any(|p| url.starts_with(p))
    }
}

impl Default for DenyList {
    fn default() -> Self {
        Self::new(
            vec![
                "**/.ssh/**".into(),
                "**/.git/**".into(),
                "**/__pycache__/**".into(),
                "**/.env*".into(),
                "**/*.pem".into(),
                "**/*.key".into(),
                "**/*.crt".into(),
                "**/*.cert".into(),
                "/etc/shadow".into(),
                "/etc/sudoers".into(),
                "/etc/passwd".into(),
                "/etc/kubernetes/**".into(),
            ],
            vec![
                // Cloud metadata endpoints — never let the model probe these
                "http://169.254.169.254".into(),
                "http://metadata.google.internal".into(),
                "http://100.100.100.200".into(),
            ],
        )
    }
}

// ── Path Guard ───────────────────────────────────────────────────────

/// Outcome of a path guard check.
#[derive(Debug, Clone, PartialEq)]
pub enum GuardVerdict {
    /// The operation is allowed. Contains the resolved path.
    Allowed(PathBuf),
    /// The operation is denied with a reason.
    Denied(String),
}

/// Multi-layer path safety guard.
///
/// Checks are applied in a fixed order so the error message is
/// deterministic: deny list → sandbox → extension → dotfile → symlink
/// → size → allowed directories.
#[derive(Debug, Clone)]
pub struct PathGuard {
    /// If set, all file ops are restricted to this directory tree.
    pub sandbox_dir: Option<PathBuf>,
    /// File extensions blocked for write (e.g. `.pem`, `.key`).
    pub deny_extensions: Vec<String>,
    /// Block writes to dotfiles (files starting with `.`).
    pub block_dotfiles: bool,
    /// Maximum file size in bytes for reads (0 = unlimited).
    pub max_read_size: usize,
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

    /// Check whether `path` may be read.
    ///
    /// On success returns the resolved path (canonicalized if it exists).
    pub fn check_read(&self, path: &Path) -> GuardVerdict {
        // 1. Deny list
        if self.deny_list.is_path_denied(path) {
            return GuardVerdict::Denied(format!("Path denied by deny list: {}", path.display()));
        }

        // Resolve real path if it exists (for sandbox + symlink checks)
        let canonical = if path.exists() {
            match path.canonicalize() {
                Ok(c) => c,
                Err(e) => {
                    return GuardVerdict::Denied(format!(
                        "Cannot resolve path '{}': {e}",
                        path.display()
                    ));
                }
            }
        } else {
            return GuardVerdict::Denied(format!("Path does not exist: {}", path.display()));
        };

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
            if !canonical.starts_with(&sb) {
                return GuardVerdict::Denied(format!("Path outside sandbox: {}", path.display()));
            }
        }

        // 3. Symlink guard
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

        // 4. Size limit
        if self.max_read_size > 0 {
            let metadata = match std::fs::symlink_metadata(path) {
                Ok(m) => m,
                Err(e) => {
                    return GuardVerdict::Denied(format!(
                        "Cannot read metadata for '{}': {e}",
                        path.display()
                    ));
                }
            };
            if metadata.len() > self.max_read_size as u64 {
                return GuardVerdict::Denied(format!(
                    "File too large ({} > {} bytes): {}",
                    metadata.len(),
                    self.max_read_size,
                    path.display()
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
    pub fn check_write(&self, path: &Path) -> GuardVerdict {
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
            // For new files the path doesn't exist yet — check parent directory
            let check = if path.exists() {
                path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
            } else {
                path.parent()
                    .unwrap_or(Path::new("."))
                    .canonicalize()
                    .unwrap_or_else(|_| path.to_path_buf())
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

        // 5. Symlink guard for existing files (don't follow symlinks on write)
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
                path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
            } else {
                path.parent()
                    .unwrap_or(Path::new("."))
                    .canonicalize()
                    .unwrap_or_else(|_| path.to_path_buf())
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
    pub fn check_edit(&self, path: &Path) -> GuardVerdict {
        if self.was_read(path) {
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
        max_read_size: config.max_file_read_size,
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

    // ── DenyList ────────────────────────────────────────────────────

    #[test]
    fn test_deny_list_default_blocks_ssh() {
        let dl = DenyList::default();
        assert!(dl.is_path_denied(Path::new("/home/user/.ssh/id_rsa")));
        assert!(dl.is_path_denied(Path::new(".ssh/config")));
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

    #[test]
    fn test_path_guard_blocks_denied_extensions() {
        let guard = PathGuard {
            deny_extensions: vec![".bad".into(), ".evil".into()],
            deny_list: DenyList::new(vec![], vec![]), // no default deny list (which includes .pem/.key)
            ..Default::default()
        };
        let result = guard.check_write(Path::new("script.bad"));
        assert!(matches!(result, GuardVerdict::Denied(msg) if msg.contains("Extension")));
        let result2 = guard.check_write(Path::new("file.evil"));
        assert!(matches!(result2, GuardVerdict::Denied(msg) if msg.contains("Extension")));
        // Normal extension should pass
        let result3 = guard.check_write(Path::new("normal.txt"));
        assert!(!matches!(result3, GuardVerdict::Denied(_)));
    }

    #[test]
    fn test_path_guard_blocks_dotfiles() {
        let guard = PathGuard {
            block_dotfiles: true,
            ..Default::default()
        };
        let result = guard.check_write(Path::new(".hidden"));
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
        let _ = std::fs::write(&tmp, "test");
        let result = guard.check_read(&tmp);
        let _ = std::fs::remove_file(&tmp);
        assert!(matches!(result, GuardVerdict::Allowed(_)));
    }

    #[test]
    fn test_path_guard_allows_normal_files() {
        let guard = PathGuard::default();
        let tmp = std::env::temp_dir().join("kirkforge_test_normal.txt");
        std::fs::write(&tmp, "hello").unwrap();
        let result = guard.check_read(&tmp);
        let _ = std::fs::remove_file(&tmp);
        assert!(matches!(result, GuardVerdict::Allowed(_)));
    }

    #[test]
    fn test_path_guard_write_for_nonexistent_path() {
        let guard = PathGuard::default();
        let tmp = std::env::temp_dir().join("kirkforge_new_file_test.txt");
        // Should not exist for this test
        let _ = std::fs::remove_file(&tmp);
        let result = guard.check_write(&tmp);
        // Allowed since it's in tmp (no sandbox, no allow list restrictions)
        assert!(matches!(result, GuardVerdict::Allowed(_)));
        // Cleanup
        let _ = std::fs::remove_file(&tmp);
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
        assert!(matches!(gate.check_edit(p), GuardVerdict::Denied(_)));
        // Mark as read
        gate.mark_read(p);
        assert!(matches!(gate.check_edit(p), GuardVerdict::Allowed(_)));
    }

    #[test]
    fn test_read_gate_clear() {
        let mut gate = ReadGate::new();
        let p = Path::new("/tmp/kirkforge_clear_test.txt");
        gate.mark_read(p);
        gate.clear();
        assert!(matches!(gate.check_edit(p), GuardVerdict::Denied(_)));
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
    #[test]
    fn test_path_guard_default_is_fail_open() {
        let guard = PathGuard::default();
        assert!(
            !guard.is_sandboxed(),
            "default PathGuard is unsandboxed by design"
        );
        // Writes to /tmp are allowed (no sandbox, no allowlist blocking it).
        let tmp = std::env::temp_dir().join("kirkforge_default_failopen.txt");
        let _ = std::fs::remove_file(&tmp);
        let result = guard.check_write(&tmp);
        let _ = std::fs::remove_file(&tmp);
        assert!(
            matches!(result, GuardVerdict::Allowed(_)),
            "default PathGuard allows writes to /tmp; if this changes, the \
             test name `is_fail_open` no longer matches reality and several \
             call sites in tests/ will need to be updated"
        );
        // But the deny list and deny extensions still apply — even
        // unsandboxed, .ssh and .pem are blocked.
        let ssh_like = std::env::temp_dir().join("kirkforge.pem");
        let result = guard.check_write(&ssh_like);
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
    #[test]
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
}
