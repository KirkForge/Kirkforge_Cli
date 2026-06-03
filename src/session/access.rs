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
        self.path_matchers.iter().any(|m| m.is_match(as_str.as_ref()))
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
    /// Check whether `path` may be read.
    ///
    /// On success returns the resolved path (canonicalized if it exists).
    pub fn check_read(&self, path: &Path) -> GuardVerdict {
        // 1. Deny list
        if self.deny_list.is_path_denied(path) {
            return GuardVerdict::Denied(format!(
                "Path denied by deny list: {}",
                path.display()
            ));
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
                return GuardVerdict::Denied(format!(
                    "Path outside sandbox: {}",
                    path.display()
                ));
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
                return GuardVerdict::Denied(format!(
                    "Symlinks not allowed: {}",
                    path.display()
                ));
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

        GuardVerdict::Allowed(canonical)
    }

    /// Check whether `path` may be written (created or overwritten).
    ///
    /// Handles non-existent paths by checking the parent directory
    /// for sandbox containment.
    pub fn check_write(&self, path: &Path) -> GuardVerdict {
        // 1. Deny list
        if self.deny_list.is_path_denied(path) {
            return GuardVerdict::Denied(format!(
                "Path denied by deny list: {}",
                path.display()
            ));
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
                return GuardVerdict::Denied(format!(
                    "Path outside sandbox: {}",
                    path.display()
                ));
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
        let sample = if content.len() > 512 { &content[..512] } else { content };
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
                 Use read_file first, or explicitly confirm with the \
                 force_edit argument.",
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

/// Build a [`DenyList`] and [`PathGuard`] from the user's config.
pub fn access_from_config(config: &crate::shared::Config) -> (DenyList, PathGuard, ReadGate) {
    let deny_list = DenyList::new(config.deny_paths.clone(), config.deny_urls.clone());

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

    let path_guard = PathGuard {
        sandbox_dir,
        deny_extensions: config.deny_extensions.clone(),
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
        let dl = DenyList::new(
            vec!["**/secret/**".into()],
            vec![],
        );
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
}