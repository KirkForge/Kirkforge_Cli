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
                // deny_paths come from cross-platform config (forward-slash
                // convention); `diff_paths` emits OS-native separators, so
                // normalize both sides before comparing.
                let rel = diff_paths(&full, Path::new(cwd))
                    .map(|p| p.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
                    .unwrap_or_default();
                let hit = deny.deny_paths.iter().any(|d| {
                    let d = d.replace('\\', "/");
                    rel == d || rel.starts_with(&format!("{d}/"))
                });
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

    // ── zero-coverage branch tests (example-style) ───────────────────────
    // Five branches in `write_artifacts` had no tests (WO 43.4). These are
    // deterministic vectors; the property suite below adds the for-all layer.

    #[test]
    fn write_artifacts_blocks_size_limit() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let oversize = "x".repeat(MAX_ARTIFACT_BYTES + 1);
        let arts = vec![ArtifactRecord {
            file_path: "big.txt".into(),
            content: oversize,
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert!(!results[0].ok);
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("exceeds maximum size"));
        assert!(!dir.path().join("big.txt").exists());
    }

    #[test]
    fn write_artifacts_blocks_binary_content() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        // >30% non-printable control bytes in first 8K → binary-like.
        let mut bin = String::new();
        for _ in 0..2000 {
            bin.push('\x01');
        }
        bin.push('a');
        let arts = vec![ArtifactRecord {
            file_path: "blob.txt".into(),
            content: bin,
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert!(!results[0].ok);
        assert!(results[0].blocked.as_ref().unwrap().contains("binary-like"));
        assert!(!dir.path().join("blob.txt").exists());
    }

    #[test]
    fn write_artifacts_blocks_deny_paths_exact() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let p = TaskProfileLike {
            language: Some("text".into()),
            allowed_extensions: Some(vec![".txt".into()]),
            forbidden_extensions: None,
            write_policy: Some(WritePolicyLike {
                allow_overwrite: true,
                deny_paths: vec!["secrets/vault.txt".into()],
            }),
        };
        let arts = vec![ArtifactRecord {
            file_path: "secrets/vault.txt".into(),
            content: "x".into(),
        }];
        let results = write_artifacts(&arts, &cwd, Some(&p));
        assert!(!results[0].ok);
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("path denied by writePolicy"));
    }

    #[test]
    fn write_artifacts_blocks_deny_paths_prefix() {
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let p = TaskProfileLike {
            language: Some("text".into()),
            allowed_extensions: Some(vec![".txt".into()]),
            forbidden_extensions: None,
            write_policy: Some(WritePolicyLike {
                allow_overwrite: true,
                deny_paths: vec!["secrets".into()],
            }),
        };
        let arts = vec![ArtifactRecord {
            file_path: "secrets/vault.txt".into(),
            content: "x".into(),
        }];
        let results = write_artifacts(&arts, &cwd, Some(&p));
        assert!(!results[0].ok);
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("path denied by writePolicy"));
    }

    #[cfg(unix)]
    #[test]
    fn write_artifacts_blocks_terminal_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempdir();
        let cwd = dir.path().to_string_lossy().to_string();
        let target = dir.path().join("real.txt");
        std::fs::write(&target, "keep").unwrap();
        let link = dir.path().join("link.txt");
        symlink(&target, &link).unwrap();
        let arts = vec![ArtifactRecord {
            file_path: "link.txt".into(),
            content: "new".into(),
        }];
        let results = write_artifacts(&arts, &cwd, None);
        assert!(!results[0].ok);
        assert!(results[0]
            .blocked
            .as_ref()
            .unwrap()
            .contains("final path is symlink"));
        // The link target is untouched.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }

    // ── property-based tests (WO 43.4) ───────────────────────────────────
    // proptest! blocks live in a dedicated submodule because
    // `cargo clippy --all-targets` chokes on `#[test]` items generated by the
    // `proptest!` macro when they're directly inside `mod tests`
    // ("cannot test inner items"). Isolating them in their own module avoids
    // the clippy false-positive while keeping the tests. Same pattern as
    // WO 41.7 in `src/shared/permission.rs:1500`.
    mod proptest_suites {
        use super::*;
        use proptest::prelude::*;

        // A relative-path segment: lowercase letters, no separators, not
        // empty. Enough to build realistic relative paths without smuggling in
        // `..`/`.`/absolute prefixes — the injection strategies below add
        // those deliberately.
        fn seg_strategy() -> impl Strategy<Value = String> {
            "[a-z]{1,8}"
        }

        fn rel_path_strategy() -> impl Strategy<Value = String> {
            prop::collection::vec(seg_strategy(), 1..4).prop_map(|segs| segs.join("/"))
        }

        // ── P1: traversal ────────────────────────────────────────────────

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            // Leading `..` escapes cwd: `is_inside_cwd` false AND
            // `safe_relative_path` None. The cwd is a real tempdir so the
            // containment check has a concrete base to resolve against.
            #[test]
            fn p1_leading_parent_escape_rejected(
                tail in rel_path_strategy(),
            ) {
                let dir = tempdir();
                let cwd = dir.path().to_string_lossy().to_string();
                let full = format!("{cwd}/../{tail}");
                prop_assert!(!is_inside_cwd(&full, &cwd));
                let user = format!("../{tail}");
                prop_assert_eq!(safe_relative_path(&cwd, &user, false), None);
            }

            // Interior `..` that pops below the root still escapes. `head` is
            // a SINGLE segment so two `..` always pops past the cwd root.
            #[test]
            fn p1_interior_parent_escape_rejected(
                head in seg_strategy(),
                tail in rel_path_strategy(),
            ) {
                let dir = tempdir();
                let cwd = dir.path().to_string_lossy().to_string();
                let full = format!("{cwd}/{head}/../../{tail}");
                prop_assert!(!is_inside_cwd(&full, &cwd));
                let user = format!("{head}/../../{tail}");
                prop_assert_eq!(safe_relative_path(&cwd, &user, false), None);
            }

            // A clean relative path stays inside cwd and is accepted. The
            // `safe.is_some() == inside` invariant is the core contract: the
            // two predicates agree on the `[a-z]` segment space (no hidden
            // segs, no traversal).
            #[test]
            fn p1_clean_relative_is_inside_and_safe(
                segs in prop::collection::vec(seg_strategy(), 1..4),
            ) {
                let dir = tempdir();
                let cwd = dir.path().to_string_lossy().to_string();
                let rel = segs.join("/");
                let full = format!("{cwd}/{rel}");
                let inside = is_inside_cwd(&full, &cwd);
                let safe = safe_relative_path(&cwd, &rel, false);
                prop_assert!(inside, "expected inside for {}", full);
                prop_assert!(safe.is_some(), "expected Some for {}", rel);
                prop_assert_eq!(inside, safe.is_some());
            }
        }

        // ── P2: absolute injection ───────────────────────────────────────

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            // Unix-absolute and Windows drive-absolute forms are detected.
            // `rest_no_sep` never starts/ends with a separator so the
            // non-absolute cases (`sub/{rest}`, `C:{rest}`) stay non-absolute.
            #[test]
            fn p2_absolute_prefixes_detected(
                rest in "[a-z0-9_/.]{1,16}",
                rest_no_sep in "[a-z0-9_]{1,16}",
            ) {
                let unix_abs = format!("/{rest}");
                let win_fwd = format!("C:/x/{rest}");
                let win_bwd = format!(r"C:\x\{rest}");
                let d_fwd = format!("D:/stuff/{rest}");
                let rel = format!("sub/{rest_no_sep}");
                let drive_rel = format!("C:{rest_no_sep}");
                prop_assert!(is_absolute_path(&unix_abs));
                prop_assert!(is_absolute_path(&win_fwd));
                prop_assert!(is_absolute_path(&win_bwd));
                prop_assert!(is_absolute_path(&d_fwd));
                // Relative form is NOT absolute.
                prop_assert!(!is_absolute_path(&rel));
                // `C:foo` (drive-relative, no slash) is NOT absolute.
                prop_assert!(!is_absolute_path(&drive_rel));
            }
        }

        // Pinned finding (WO 43.4): UNC paths `\\srv\share` are NOT detected
        // by `is_absolute_path` — backslash lead misses the Unix `/` check
        // AND the `[A-Za-z]:[\\/]` Windows check. Containment step 1 in
        // `write_artifacts` still blocks them (they aren't inside cwd), so
        // this is a detection gap, not an escape. Fix deferred.
        #[test]
        fn p2_unc_path_not_detected_documented_gap() {
            assert!(!is_absolute_path(r"\\srv\share"));
            assert!(!is_absolute_path(r"\srv\share"));
        }

        // ── P3: no-panic on arbitrary input ───────────────────────────────

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            // Arbitrary chars (incl. `\0`, control bytes, emoji, mixed
            // separators) through every pure predicate: never panic, always
            // return a verdict.
            #[test]
            fn p3_predicates_total_on_arbitrary(
                s in ".*",
            ) {
                let _ = is_absolute_path(&s);
                let _ = has_hidden_segment(&s);
                let _ = extract_extension(&s);
                let _ = is_binary_like_content(&s);
                let dir = tempdir();
                let cwd = dir.path().to_string_lossy().to_string();
                let _ = is_inside_cwd(&s, &cwd);
                let _ = safe_relative_path(&cwd, &s, false);
                let _ = safe_relative_path(&cwd, &s, true);
            }

            // `disallowed_artifact` on arbitrary content + path: never panics.
            #[test]
            fn p3_disallowed_artifact_total(
                path in ".*",
                content in ".*",
            ) {
                let art = ArtifactRecord {
                    file_path: path.clone(),
                    content,
                };
                let _ = disallowed_artifact(&art, None);
                let p = TaskProfileLike {
                    language: Some("rust".into()),
                    allowed_extensions: Some(vec![".rs".into()]),
                    forbidden_extensions: Some(vec![".exe".into()]),
                    write_policy: None,
                };
                let _ = disallowed_artifact(&art, Some(&p));
            }
        }

        // ── P4: NFC/NFD consistency ───────────────────────────────────────

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            // Same logical path in NFC vs NFD gets identical containment
            // verdicts from `is_inside_cwd` and `safe_relative_path`.
            // `path::components()` splits on chars, so NFC `é` (1 char) and
            // NFD `e` + `\u{301}` (2 chars) produce the same component count
            // and the same containment decision.
            #[test]
            fn p4_nfc_nfd_containment_identical(
                seg in "[a-z]{1,6}",
            ) {
                let dir = tempdir();
                let cwd = dir.path().to_string_lossy().to_string();
                // NFC: é = U+00E9. NFD: e + U+0301.
                let nfc_seg = format!("caf\u{00E9}{seg}");
                let nfd_seg = format!("cafe\u{0301}{seg}");
                let nfc_full = format!("{cwd}/{nfc_seg}");
                let nfd_full = format!("{cwd}/{nfd_seg}");
                let nfc_inside = is_inside_cwd(&nfc_full, &cwd);
                let nfd_inside = is_inside_cwd(&nfd_full, &cwd);
                prop_assert_eq!(nfc_inside, nfd_inside);
                let nfc_safe = safe_relative_path(&cwd, &nfc_seg, false);
                let nfd_safe = safe_relative_path(&cwd, &nfd_seg, false);
                prop_assert_eq!(nfc_safe.is_some(), nfd_safe.is_some());
            }
        }

        // Pinned finding (WO 43.4): `write_artifacts` deny_paths matching
        // (`path_safety.rs:548`) is byte-compare (`rel == *d`), NOT
        // normalization-aware. A deny_paths entry written in NFC will NOT block
        // the same logical path emitted in NFD (different byte sequences).
        // Fix deferred — would require normalizing both sides before compare.
        #[test]
        fn p4_deny_paths_normalization_gap_documented() {
            let dir = tempdir();
            let cwd = dir.path().to_string_lossy().to_string();
            // NFC deny entry; NFD artifact.
            let nfc_deny = "café/secret.txt";
            let nfd_art = "cafe\u{0301}/secret.txt";
            let p = TaskProfileLike {
                language: Some("text".into()),
                allowed_extensions: Some(vec![".txt".into()]),
                forbidden_extensions: None,
                write_policy: Some(WritePolicyLike {
                    allow_overwrite: true,
                    deny_paths: vec![nfc_deny.into()],
                }),
            };
            let arts = vec![ArtifactRecord {
                file_path: nfd_art.into(),
                content: "x".into(),
            }];
            let results = write_artifacts(&arts, &cwd, Some(&p));
            // The NFD artifact is NOT blocked by the NFC deny entry.
            assert!(results[0].ok, "NFD artifact should bypass NFC deny entry");
        }

        // ── P5: symlink fixtures (Unix-only) ──────────────────────────────
        #[cfg(unix)]
        mod symlinks {
            use super::*;
            use std::os::unix::fs::symlink;
            use tempfile::TempDir;

            struct Fixture {
                _dir: TempDir,
                cwd: String,
            }

            impl Fixture {
                fn new() -> Self {
                    let dir = tempdir();
                    let cwd = dir.path().to_string_lossy().to_string();
                    // inside_target — link target is inside cwd.
                    let inside_target = dir.path().join("inside_target");
                    std::fs::create_dir_all(&inside_target).unwrap();
                    // outside_target — link target is cwd's parent (escapes).
                    let outside_target = dir.path().parent().unwrap().join("outside_target");
                    std::fs::create_dir_all(&outside_target).unwrap();
                    // inside_link → inside_target (safe).
                    symlink(&inside_target, dir.path().join("inside_link")).unwrap();
                    // outside_link → outside_target (escapes cwd).
                    symlink(&outside_target, dir.path().join("outside_link")).unwrap();
                    // term_link → inside_target/term.txt (terminal symlink).
                    let term_target = inside_target.join("term.txt");
                    std::fs::write(&term_target, "x").unwrap();
                    symlink(&term_target, dir.path().join("term_link")).unwrap();
                    Self { _dir: dir, cwd }
                }
            }

            proptest! {
                #![proptest_config(ProptestConfig::with_cases(64))]

                // `segments_have_escaping_symlink` fires ONLY when a segment
                // is a symlink whose target escapes cwd. inside_link is safe;
                // outside_link fires.
                #[test]
                fn p5_escaping_symlink_only_for_outside_link(
                    sub in rel_path_strategy(),
                ) {
                    let f = Fixture::new();
                    let inside = format!("inside_link/{sub}");
                    let outside = format!("outside_link/{sub}");
                    let inside_full = format!("{}/{}", f.cwd, inside);
                    let outside_full = format!("{}/{}", f.cwd, outside);
                    prop_assert!(
                        !segments_have_escaping_symlink(&inside_full, &f.cwd),
                        "inside_link should not escape: {}", inside_full,
                    );
                    prop_assert!(
                        segments_have_escaping_symlink(&outside_full, &f.cwd),
                        "outside_link should escape: {}", outside_full,
                    );
                }
            }

            #[test]
            fn p5_final_file_is_symlink_true_for_terminal() {
                let f = Fixture::new();
                let term = format!("{}/term_link", f.cwd);
                assert!(final_file_is_symlink(&term));
                // A regular path that doesn't exist → false.
                assert!(!final_file_is_symlink(&format!("{}/nope", f.cwd)));
            }
        }
    }
}
