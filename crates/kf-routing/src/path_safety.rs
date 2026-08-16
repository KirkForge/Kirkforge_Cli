//! R5 — port of `orchestrator/src/path-safety.ts`.
//!
//! Pure path + content safety checks for artifact writes, plus an atomic
//! `write_artifacts` that applies all guards before touching the filesystem.
//!
//! Overlap with `src/shared/access/mod.rs` (`PathGuard`): `PathGuard` is the
//! async, config/DenyList-coupled guard used by the live agent loop. This
//! module is the pure, synchronous artifact-emission policy that the
//! orchestrator (WO 29.7) calls.
//!
//! Unifying them is DEFERRED to WO 32.11. The two impls have genuinely
//! different contracts: `PathGuard` is per-file, `Path`/`OsStr`-based,
//! `canonicalize`-resolved (fail-closed on metadata error), config-coupled,
//! async (gitignore probe); `path_safety` is batch-write, `&str`-based,
//! lexical `is_inside_cwd` (no fs canonicalize, fail-open on metadata error),
//! profile-coupled, sync. Every overlapping check (extension, symlink,
//! sandbox, dotfile) has different types or error semantics. Delegating
//! without behavior change needs an adapter (OsStr→str, fail-closed→fail-open
//! wrapping) that is more code than the duplication it removes.
//! Remaining work: design a `PathPolicy` trait here with sync + async-compatible
//! signatures; adapt `PathGuard` to implement it; reconcile error semantics.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum artifact size (1 MiB; matches TS `MAX_ARTIFACT_BYTES`).
pub const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

const ALLOWED_HIDDEN_SEGMENTS: &[&str] = &[".vscode", ".idea"];

// ── Hash helpers ────────────────────────────────────────────────────────────

pub fn sha256_of(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex(&hasher.finalize())
}

pub fn sha256_of_raw(buf: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(buf);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// ── Absolute-path detection ─────────────────────────────────────────────────

/// True if `p` starts with `/` (Unix absolute) or matches `[A-Za-z]:\\`
/// (Windows drive absolute). Matches TS `isAbsolutePath`.
pub fn is_absolute_path(p: &str) -> bool {
    if p.starts_with('/') {
        return true;
    }
    let bytes = p.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

// ── Path containment ────────────────────────────────────────────────────────

/// Lexically normalize `.` and `..` segments in a path without touching the
/// filesystem. Matches Node's `path.resolve` behavior for the cases
/// `is_inside_cwd`/`safe_relative_path` care about: a leading `..` on an
/// absolute path stays (it escapes the root), interior `..` pops the
/// preceding `Normal` segment, and `.` is dropped.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {
                    // `..` at the root is a no-op.
                }
                _ => {
                    out.push(c);
                }
            },
            other => out.push(other),
        }
    }
    let mut pb = PathBuf::new();
    for c in &out {
        pb.push(c.as_os_str());
    }
    pb
}

/// Compute the relative path from `base` to `target`, resolving common
/// prefixes into nothing and leftover `base` components into `..`. Returns
/// `None` when one path is absolute and the other is not (matches
/// `pathdiff::diff_paths`). Inlined here to avoid a dependency for ~25 lines.
fn diff_paths<'a>(target: &'a Path, base: &'a Path) -> Option<PathBuf> {
    let mut ita = target.components();
    let mut itb = base.components();
    let mut comps: Vec<Component<'a>> = Vec::new();
    loop {
        match (ita.next(), itb.next()) {
            (None, None) => break,
            (Some(a), None) => {
                comps.push(a);
                comps.extend(ita.by_ref());
                break;
            }
            (None, Some(_)) => {
                comps.push(Component::ParentDir);
                for _ in itb.by_ref() {
                    comps.push(Component::ParentDir);
                }
                break;
            }
            (Some(a), Some(b)) if a == b => {
                // still on a common prefix
            }
            (Some(a), Some(b)) => {
                // First mismatch. If exactly one of the mismatched starts is
                // absolute (RootDir / Prefix), the paths live in different
                // roots → no relative path exists. When both are non-root
                // (e.g. Normal dirs), proceed with the standard diff.
                let a_abs = matches!(a, Component::Prefix(_) | Component::RootDir);
                let b_abs = matches!(b, Component::Prefix(_) | Component::RootDir);
                if a_abs != b_abs {
                    return None;
                }
                comps.push(Component::ParentDir);
                for _ in itb.by_ref() {
                    comps.push(Component::ParentDir);
                }
                comps.push(a);
                comps.extend(ita.by_ref());
                break;
            }
        }
    }
    if comps.is_empty() {
        return Some(PathBuf::new());
    }
    let mut out = PathBuf::new();
    for c in &comps {
        out.push(c.as_os_str());
    }
    Some(out)
}

/// True if `full_path` is strictly inside `cwd`. Rejects absolute `rel`,
/// exact-equal (cwd itself), and `..` escapes (prefix-collision safe).
pub fn is_inside_cwd(full_path: &str, cwd: &str) -> bool {
    let target = normalize_lexical(Path::new(full_path));
    let rel = match diff_paths(&target, Path::new(cwd)) {
        Some(r) => r,
        None => return false,
    };
    let s = rel.to_string_lossy();
    if s.is_empty() || s == ".." || s.starts_with("..") || Path::new(s.as_ref()).is_absolute() {
        // ponytail: `starts_with("..")` covers both "../foo" and the lone ".."
        // case, matching the TS `rel === ".." || rel.startsWith(".." + sep) ||
        // rel.startsWith("../")` triple-test in one check.
        return false;
    }
    true
}

/// Centralised safe-relative-path from user/CLI input. Rejects absolute
/// paths, `..` segments, empty strings, and hidden segments (unless
/// `allow_hidden`).
pub fn safe_relative_path(cwd: &str, user_path: &str, allow_hidden: bool) -> Option<String> {
    if user_path.trim().is_empty() {
        return None;
    }
    let joined = Path::new(cwd).join(user_path);
    let resolved = normalize_lexical(&joined);
    let rel = diff_paths(&resolved, Path::new(cwd))?;
    let s = rel.to_string_lossy();
    if s.is_empty() || s == ".." || s.starts_with("..") || Path::new(s.as_ref()).is_absolute() {
        return None;
    }
    if !allow_hidden {
        for seg in rel.components() {
            if let Component::Normal(os) = seg {
                let part = os.to_string_lossy();
                if part.starts_with('.') && part != "." && part != ".." {
                    return None;
                }
            }
        }
    }
    Some(s.into_owned())
}

// ── Content safety ──────────────────────────────────────────────────────────

/// True if the first 8192 chars are > 30% non-printable control characters.
pub fn is_binary_like_content(content: &str) -> bool {
    let mut non_printable = 0usize;
    let mut total = 0usize;
    for c in content.chars().take(8192) {
        total += 1;
        let cc = c as u32;
        if cc < 32 && cc != 9 && cc != 10 && cc != 13 {
            non_printable += 1;
        }
    }
    if total == 0 {
        return false;
    }
    (non_printable as f64) / (total as f64) > 0.3
}

// ── Extension / hidden-segment helpers ──────────────────────────────────────

/// Last-dot extension of the basename, lowercased. Leading-dot files (e.g.
/// `.env`) return empty. Matches TS `extractExtension` (`dotIndex > 0`).
pub fn extract_extension(file_path: &str) -> String {
    let base = Path::new(file_path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match base.rfind('.') {
        Some(0) | None => String::new(),
        Some(i) => base[i..].to_lowercase(),
    }
}

/// True if any non-root segment starts with `.` and is not in the
/// allow-list (`.vscode`, `.idea`).
pub fn has_hidden_segment(file_path: &str) -> bool {
    for part in file_path.split(['/', '\\']) {
        if part.starts_with('.')
            && part != "."
            && part != ".."
            && !ALLOWED_HIDDEN_SEGMENTS.contains(&part)
        {
            return true;
        }
    }
    false
}

// ── Symlink guards ──────────────────────────────────────────────────────────

/// Walk each absolute-segment prefix of `file_path` and fail if any
/// intermediate dir/file is a symlink whose target escapes `cwd`. Missing
/// intermediate segments are normal (target may not exist yet).
pub fn segments_have_escaping_symlink(file_path: &str, cwd: &str) -> bool {
    let path = Path::new(file_path);
    let mut acc = match path.components().next() {
        Some(Component::RootDir) => PathBuf::from("/"),
        Some(Component::Prefix(p)) => {
            let mut pb = PathBuf::new();
            pb.push(p.as_os_str());
            pb.push("\\");
            pb
        }
        _ => PathBuf::new(),
    };
    for comp in path.components() {
        match comp {
            Component::RootDir | Component::Prefix(_) => continue,
            Component::CurDir => continue,
            Component::ParentDir => {
                acc.pop();
                continue;
            }
            Component::Normal(seg) => {
                acc.push(seg);
            }
        }
        if let Ok(meta) = std::fs::symlink_metadata(&acc) {
            if meta.file_type().is_symlink() {
                if let Ok(target) = std::fs::read_link(&acc) {
                    let resolved = if target.is_absolute() {
                        target
                    } else {
                        acc.parent().map(|p| p.join(&target)).unwrap_or(target)
                    };
                    if !is_inside_cwd(&resolved.to_string_lossy(), cwd) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// True when the final path exists and is a symlink (never safe to write
/// through).
pub fn final_file_is_symlink(file_path: &str) -> bool {
    std::fs::symlink_metadata(file_path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

// ── Artifact allow/deny ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub file_path: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskProfileLike {
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub allowed_extensions: Option<Vec<String>>,
    #[serde(default)]
    pub forbidden_extensions: Option<Vec<String>>,
    #[serde(default)]
    pub write_policy: Option<WritePolicyLike>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WritePolicyLike {
    #[serde(default)]
    pub allow_overwrite: bool,
    #[serde(default)]
    pub deny_paths: Vec<String>,
}

/// Returns `Some(reason)` if the artifact is blocked, or `None` if allowed.
pub fn disallowed_artifact(
    art: &ArtifactRecord,
    profile: Option<&TaskProfileLike>,
) -> Option<String> {
    if is_absolute_path(&art.file_path) {
        return Some(format!(
            "absolute path \"{}\" is not allowed",
            art.file_path
        ));
    }
    let base = Path::new(&art.file_path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if base.starts_with('.') {
        return Some(format!(
            "hidden dotfile \"{base}\" is not allowed (dotfiles are default-deny)"
        ));
    }
    if has_hidden_segment(&art.file_path) {
        return Some(format!(
            "hidden directory segment in path \"{}\" is not allowed",
            art.file_path
        ));
    }
    let ext = extract_extension(&art.file_path);
    if let Some(p) = profile {
        if !ext.is_empty() {
            if let Some(forbidden) = &p.forbidden_extensions {
                if forbidden.iter().any(|e| e == &ext) {
                    let lang = p.language.as_deref().unwrap_or("task");
                    return Some(format!(
                        "{} task cannot emit \"{}\" (forbidden extension {})",
                        lang, art.file_path, ext
                    ));
                }
            }
        }
        if let Some(allowed) = &p.allowed_extensions {
            if !allowed.is_empty() {
                if ext.is_empty() {
                    return Some(format!(
                        "{} task emitted \"{}\" — no-extension files not allowed",
                        p.language.as_deref().unwrap_or("task"),
                        art.file_path
                    ));
                }
                if !allowed.iter().any(|e| e == &ext) {
                    return Some(format!(
                        "{} task emitted \"{}\" with unexpected extension {}",
                        p.language.as_deref().unwrap_or("task"),
                        art.file_path,
                        ext
                    ));
                }
            }
        }
    }
    None
}

// ── Atomic write ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteResult {
    pub file_path: String,
    pub bytes: usize,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existed: Option<bool>,
}

impl WriteResult {
    fn blocked(file_path: impl Into<String>, reason: impl Into<String>) -> Self {
        WriteResult {
            file_path: file_path.into(),
            bytes: 0,
            ok: false,
            blocked: Some(reason.into()),
            warning: None,
            sha256: None,
            before_hash: None,
            existed: None,
        }
    }
}

fn unique_tmp_path(full: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = full
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(&format!(".tmp.{nanos:x}.{n:x}"));
    full.with_file_name(name)
}

/// Write `artifacts` into `cwd` with all safety checks applied. Uses a
/// sibling tmp file + rename for atomicity; fsync best-effort.
pub fn write_artifacts(
    artifacts: &[ArtifactRecord],
    cwd: &str,
    profile: Option<&TaskProfileLike>,
) -> Vec<WriteResult> {
    let mut results = Vec::with_capacity(artifacts.len());
    for art in artifacts {
        let full = Path::new(cwd).join(&art.file_path);
        let full_str = full.to_string_lossy();

        // 1. Sandbox containment.
        if !is_inside_cwd(&full_str, cwd) {
            results.push(WriteResult::blocked(
                &art.file_path,
                format!("resolved path escapes sandbox: {}", art.file_path),
            ));
            continue;
        }

        // 2. Extension / dotfile policy.
        if let Some(reason) = disallowed_artifact(art, profile) {
            results.push(WriteResult::blocked(&art.file_path, reason));
            continue;
        }

        // 3. Symlink traversal guard.
        if segments_have_escaping_symlink(&full_str, cwd) {
            results.push(WriteResult::blocked(
                &art.file_path,
                format!("symlink escape detected in path: {}", art.file_path),
            ));
            continue;
        }

        // 4. Final symlink guard.
        if final_file_is_symlink(&full_str) {
            results.push(WriteResult::blocked(
                &art.file_path,
                format!(
                    "final path is symlink — writes would follow link outside sandbox: {}",
                    art.file_path
                ),
            ));
            continue;
        }

        // 5. Size limit.
        let byte_len = art.content.len();
        if byte_len > MAX_ARTIFACT_BYTES {
            results.push(WriteResult::blocked(
                &art.file_path,
                format!(
                    "artifact exceeds maximum size ({byte_len} bytes > {MAX_ARTIFACT_BYTES} bytes)"
                ),
            ));
            continue;
        }

        // 6. Binary check.
        if is_binary_like_content(&art.content) {
            results.push(WriteResult::blocked(
                &art.file_path,
                "artifact rejected: content appears binary-like (>30% non-printable characters in first 8 KB)",
            ));
            continue;
        }

        // 7. Hash + existence.
        let hash = sha256_of(&art.content);
        let existed = full.exists();

        // 8. Write policy — overwrite deny.
        let allow_overwrite = profile
            .and_then(|p| p.write_policy.as_ref())
            .map(|w| w.allow_overwrite)
            .unwrap_or(false);
        if existed && !allow_overwrite {
            results.push(WriteResult::blocked(
                &art.file_path,
                format!(
                    "overwrite denied (allowOverwrite not enabled in writePolicy): {}",
                    art.file_path
                ),
            ));
            continue;
        }

        // 9. Write policy — deny_paths.
        if let Some(deny) = profile.and_then(|p| p.write_policy.as_ref()) {
            if !deny.deny_paths.is_empty() {
                let rel = diff_paths(&full, Path::new(cwd))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let hit = deny
                    .deny_paths
                    .iter()
                    .any(|d| rel == *d || rel.starts_with(&format!("{d}/")));
                if hit {
                    results.push(WriteResult::blocked(
                        &art.file_path,
                        format!("path denied by writePolicy: {}", art.file_path),
                    ));
                    continue;
                }
            }
        }

        // 10. Before-hash (only if file existed).
        let mut before_hash: Option<Option<String>> = None;
        if existed {
            match std::fs::read(&full) {
                Ok(bytes) => {
                    before_hash = Some(Some(sha256_of_raw(&bytes)));
                }
                Err(_) => {
                    results.push(WriteResult::blocked(
                        &art.file_path,
                        format!(
                            "cannot read existing file for beforeHash: {}",
                            art.file_path
                        ),
                    ));
                    continue;
                }
            }
        }

        // 11. Atomic write.
        if let Some(parent) = full.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                results.push(WriteResult::blocked(
                    &art.file_path,
                    format!("write error: {e}"),
                ));
                continue;
            }
        }
        let tmp = unique_tmp_path(&full);
        if let Err(e) = std::fs::write(&tmp, art.content.as_bytes()) {
            results.push(WriteResult::blocked(
                &art.file_path,
                format!("write error: {e}"),
            ));
            let _ = std::fs::remove_file(&tmp);
            continue;
        }
        // fsync best-effort.
        if let Ok(file) = std::fs::File::open(&tmp) {
            let _ = file.sync_all();
            drop(file);
        }
        match std::fs::rename(&tmp, &full) {
            Ok(()) => {
                let rel = diff_paths(&full, Path::new(cwd))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| art.file_path.clone());
                results.push(WriteResult {
                    file_path: rel,
                    bytes: byte_len,
                    ok: true,
                    blocked: None,
                    warning: None,
                    sha256: Some(hash),
                    before_hash,
                    existed: Some(existed),
                });
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                results.push(WriteResult::blocked(
                    &art.file_path,
                    format!("write error: {e}"),
                ));
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_absolute_path ──

    #[test]
    fn detects_unix_absolute() {
        assert!(is_absolute_path("/etc/passwd"));
        assert!(!is_absolute_path("etc/passwd"));
    }

    #[test]
    fn detects_windows_drive_absolute() {
        assert!(is_absolute_path(r"C:\windows"));
        assert!(is_absolute_path("D:/stuff"));
        assert!(!is_absolute_path("C:foo"));
    }

    // ── sha256_of ──

    #[test]
    fn sha256_known_vector() {
        // SHA-256 of empty string.
        assert_eq!(
            sha256_of(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_deterministic() {
        assert_eq!(sha256_of("hello"), sha256_of("hello"));
        assert_ne!(sha256_of("hello"), sha256_of("world"));
    }

    #[test]
    fn sha256_of_raw_matches_string_form() {
        assert_eq!(sha256_of_raw(b"abc"), sha256_of("abc"));
    }

    // ── is_inside_cwd ──

    #[test]
    fn inside_cwd_prefix_collision_rejected() {
        assert!(!is_inside_cwd(
            "/home/user/project-other/file.txt",
            "/home/user/project"
        ));
    }

    #[test]
    fn inside_cwd_self_is_false() {
        assert!(!is_inside_cwd("/tmp/test", "/tmp/test"));
    }

    #[test]
    fn inside_cwd_empty_path_false() {
        assert!(!is_inside_cwd("", "/tmp/test"));
    }

    #[test]
    fn inside_cwd_subdir_true() {
        assert!(is_inside_cwd("/tmp/test/src/main.rs", "/tmp/test"));
    }

    #[test]
    fn inside_cwd_parent_escape_false() {
        assert!(!is_inside_cwd("/tmp/test/../etc/passwd", "/tmp/test"));
    }

    // ── safe_relative_path ──

    #[test]
    fn safe_relative_rejects_empty() {
        assert_eq!(safe_relative_path("/tmp", "", false), None);
        assert_eq!(safe_relative_path("/tmp", "   ", false), None);
    }

    #[test]
    fn safe_relative_rejects_absolute() {
        assert_eq!(safe_relative_path("/tmp", "/etc/passwd", false), None);
    }

    #[test]
    fn safe_relative_rejects_hidden() {
        assert_eq!(safe_relative_path("/tmp", ".env", false), None);
        assert_eq!(
            safe_relative_path("/tmp", "subdir/.git/config", false),
            None
        );
    }

    // The `Some(string)` assertions below pin the exact returned path string.
    // `PathBuf::to_string_lossy()` uses the platform separator (`/` on Unix,
    // `\` on Windows), so the expected literal is platform-specific. The Unix
    // tests assert the forward-slash form; the Windows equivalents assert the
    // backslash form produced by joining a drive-root-relative cwd.
    #[cfg(unix)]
    #[test]
    fn safe_relative_allows_dotfiles_when_enabled() {
        assert_eq!(
            safe_relative_path("/tmp", ".vscode/settings.json", true),
            Some(".vscode/settings.json".to_string())
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn safe_relative_allows_dotfiles_when_enabled() {
        // On Windows `/tmp` is drive-root-relative (`\tmp`); the joined path
        // uses the platform separator, so the returned string uses `\`.
        assert_eq!(
            safe_relative_path("/tmp", ".vscode/settings.json", true),
            Some(".vscode\\settings.json".to_string())
        );
    }

    #[test]
    fn safe_relative_rejects_traversal() {
        assert_eq!(safe_relative_path("/tmp", "../etc/passwd", false), None);
        assert_eq!(safe_relative_path("/tmp", "foo/../../bar", false), None);
    }

    #[cfg(unix)]
    #[test]
    fn safe_relative_allows_simple_relative_path() {
        assert_eq!(
            safe_relative_path("/tmp", "src/main.rs", false),
            Some("src/main.rs".to_string())
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn safe_relative_allows_simple_relative_path() {
        // See `safe_relative_allows_dotfiles_when_enabled` for the separator
        // rationale. The joined relative path uses `\` on Windows.
        assert_eq!(
            safe_relative_path("/tmp", "src/main.rs", false),
            Some("src\\main.rs".to_string())
        );
    }

    // ── is_binary_like_content ──

    #[test]
    fn empty_is_not_binary() {
        assert!(!is_binary_like_content(""));
    }

    #[test]
    fn text_is_not_binary() {
        assert!(!is_binary_like_content("hello world\nprint('ok')"));
        assert!(!is_binary_like_content("SELECT * FROM users;"));
    }

    #[test]
    fn many_controls_is_binary() {
        let binary: String = std::iter::repeat_n('\x01', 1000).collect();
        assert!(is_binary_like_content(&binary));
    }

    #[test]
    fn unicode_text_not_binary() {
        assert!(!is_binary_like_content("こんにちは世界"));
        assert!(!is_binary_like_content("🚀✨ test"));
    }

    // ── extract_extension ──

    #[test]
    fn extension_basic() {
        assert_eq!(extract_extension("foo.py"), ".py");
        assert_eq!(extract_extension("a/b/solution.py"), ".py");
    }

    #[test]
    fn extension_uppercased_to_lower() {
        assert_eq!(extract_extension("README.MD"), ".md");
    }

    #[test]
    fn extension_none_for_leading_dotfile() {
        assert_eq!(extract_extension(".env"), "");
        assert_eq!(extract_extension(".gitignore"), "");
    }

    #[test]
    fn extension_last_dot_wins() {
        // `types.d.ts` → ".ts" (matches TS lastIndexOf, which extracts the
        // forbidden `.ts` from a `.d.ts` artifact).
        assert_eq!(extract_extension("types.d.ts"), ".ts");
    }

    #[test]
    fn extension_none_for_no_dot() {
        assert_eq!(extract_extension("Dockerfile"), "");
        assert_eq!(extract_extension("Makefile"), "");
    }

    // ── has_hidden_segment ──

    #[test]
    fn hidden_segment_detects_dot_dir() {
        assert!(has_hidden_segment("src/.git/config"));
        assert!(!has_hidden_segment("src/main.rs"));
    }

    #[test]
    fn hidden_segment_allows_vscode() {
        assert!(!has_hidden_segment(".vscode/settings.json"));
        assert!(!has_hidden_segment(".idea/workspace.xml"));
    }

    // ── disallowed_artifact ──

    #[test]
    fn blocks_absolute_path() {
        let art = ArtifactRecord {
            file_path: "/etc/passwd".into(),
            content: "".into(),
        };
        assert!(disallowed_artifact(&art, None)
            .unwrap()
            .contains("absolute path"));
    }

    #[test]
    fn blocks_dotfile() {
        let art = ArtifactRecord {
            file_path: ".env".into(),
            content: "".into(),
        };
        assert!(disallowed_artifact(&art, None)
            .unwrap()
            .contains("hidden dotfile"));
    }

    #[test]
    fn blocks_forbidden_extension() {
        let art = ArtifactRecord {
            file_path: "out.ts".into(),
            content: "".into(),
        };
        let p = TaskProfileLike {
            language: Some("python".into()),
            allowed_extensions: None,
            forbidden_extensions: Some(vec![".ts".into()]),
            write_policy: None,
        };
        let r = disallowed_artifact(&art, Some(&p)).unwrap();
        assert!(r.contains("forbidden extension .ts"));
    }

    #[test]
    fn blocks_no_extension_when_allowed_list_defined() {
        let art = ArtifactRecord {
            file_path: "Dockerfile".into(),
            content: "".into(),
        };
        let p = TaskProfileLike {
            language: Some("python".into()),
            allowed_extensions: Some(vec![".py".into()]),
            forbidden_extensions: None,
            write_policy: None,
        };
        let r = disallowed_artifact(&art, Some(&p)).unwrap();
        assert!(r.contains("no-extension files not allowed"));
    }

    #[test]
    fn allows_when_profile_absent() {
        let art = ArtifactRecord {
            file_path: "Dockerfile".into(),
            content: "".into(),
        };
        assert!(disallowed_artifact(&art, None).is_none());
    }

    // ── write_artifacts (fs-integrated) ──

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn write_artifacts_emits_valid_file() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let arts = vec![ArtifactRecord {
            file_path: "solution.py".into(),
            content: "print('hello')\n".into(),
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert_eq!(results.len(), 1);
        assert!(results[0].ok, "blocked: {:?}", results[0].blocked);
        assert_eq!(results[0].bytes, "print('hello')\n".len());
        let written = std::fs::read_to_string(dir.path().join("solution.py")).unwrap();
        assert_eq!(written, "print('hello')\n");
    }

    #[test]
    fn write_artifacts_blocks_dotfile_without_profile() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let arts = vec![ArtifactRecord {
            file_path: ".env".into(),
            content: "SECRET=1".into(),
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert!(!results[0].ok);
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("hidden dotfile"));
        assert!(!dir.path().join(".env").exists());
    }

    #[test]
    fn write_artifacts_blocks_escape() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let arts = vec![ArtifactRecord {
            file_path: "../escape.py".into(),
            content: "print('nope')".into(),
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert!(!results[0].ok);
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("escapes sandbox"));
        assert!(!dir.path().parent().unwrap().join("escape.py").exists());
    }

    #[test]
    fn write_artifacts_blocks_absolute() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let arts = vec![ArtifactRecord {
            file_path: "/etc/passwd".into(),
            content: "root".into(),
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert!(!results[0].ok);
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("escapes sandbox"));
    }

    #[test]
    fn write_artifacts_blocks_sibling_prefix_escape() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let sibling = format!("{cwd}-evil/file.py");
        let arts = vec![ArtifactRecord {
            file_path: sibling.clone(),
            content: "x".into(),
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert!(!results[0].ok);
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("escapes sandbox"));
    }

    #[test]
    fn write_artifacts_blocks_forbidden_extension_under_profile() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let p = TaskProfileLike {
            language: Some("python".into()),
            allowed_extensions: Some(vec![".py".into()]),
            forbidden_extensions: Some(vec![".ts".into()]),
            write_policy: None,
        };
        let arts = vec![ArtifactRecord {
            file_path: "out.ts".into(),
            content: "console.log('hi')".into(),
        }];
        let results = write_artifacts(&arts, &cwd, Some(&p));
        assert!(!results[0].ok);
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("forbidden extension .ts"));
    }

    #[test]
    fn write_artifacts_allows_makefile_no_profile() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let arts = vec![ArtifactRecord {
            file_path: "Makefile".into(),
            content: "all:\n\techo hi\n".into(),
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert!(results[0].ok, "blocked: {:?}", results[0].blocked);
        let written = std::fs::read_to_string(dir.path().join("Makefile")).unwrap();
        assert_eq!(written, "all:\n\techo hi\n");
    }

    #[test]
    fn write_artifacts_blocks_overwrite_when_not_allowed() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        std::fs::write(dir.path().join("existing.txt"), "old").unwrap();
        let arts = vec![ArtifactRecord {
            file_path: "existing.txt".into(),
            content: "new".into(),
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert!(!results[0].ok, "expected overwrite block");
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("overwrite denied"));
    }

    #[test]
    fn write_artifacts_allows_overwrite_when_enabled() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        std::fs::write(dir.path().join("existing.txt"), "old").unwrap();
        let p = TaskProfileLike {
            language: Some("text".into()),
            allowed_extensions: Some(vec![".txt".into()]),
            forbidden_extensions: None,
            write_policy: Some(WritePolicyLike {
                allow_overwrite: true,
                deny_paths: vec![],
            }),
        };
        let arts = vec![ArtifactRecord {
            file_path: "existing.txt".into(),
            content: "new".into(),
        }];
        let results = write_artifacts(&arts, &cwd, Some(&p));
        assert!(results[0].ok, "blocked: {:?}", results[0].blocked);
        assert_eq!(results[0].existed, Some(true));
        assert_eq!(results[0].before_hash, Some(Some(sha256_of("old"))));
        let written = std::fs::read_to_string(dir.path().join("existing.txt")).unwrap();
        assert_eq!(written, "new");
    }
}
