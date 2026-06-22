//! Pre-commit repository sanitation.
//!
//! Scans the working tree before a `git commit` and reports blockers
//! (things that should abort the commit) and warnings (things the user
//! should know about). The checks are intentionally cheap and deterministic:
//! no LLM round-trip, no heavy regex crate, no network.
//!
//! Checks:
//! - files larger than a configurable size (default 5 MB)
//! - common secret/credential patterns (`ghp_`, `sk-`, private keys, ...)
//! - merge-conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`)
//! - untracked / unstaged debris left over from the session

use std::path::{Path, PathBuf};

/// Default maximum file size allowed in a commit (bytes).
pub const DEFAULT_MAX_FILE_SIZE: u64 = 5 * 1024 * 1024;

/// Cap for how much of a file we scan for secret/conflict patterns.
/// Reading more than this is not useful for a quick sanitation pass and
/// keeps I/O bounded.
const SCAN_CAP_BYTES: u64 = 1024 * 1024;

/// Patterns that look like secrets or credentials.
///
/// Keep these specific enough to avoid flagging prose but broad enough to
/// catch common mistakes. All scanning is case-insensitive.
const SECRET_PATTERNS: &[&str] = &[
    "ghp_",        // GitHub personal access token
    "github_pat_", // GitHub fine-grained PAT
    "sk-",         // OpenAI / Stripe / generic secret key prefix
    "glpat-",      // GitLab personal access token
    "id_rsa",      // SSH private key file name
    "id_ed25519",  // SSH private key file name
    ".env",        // environment file (often contains secrets)
    "BEGIN OPENSSH PRIVATE KEY",
    "BEGIN RSA PRIVATE KEY",
    "BEGIN PRIVATE KEY",
    "BEGIN DSA PRIVATE KEY",
    "BEGIN EC PRIVATE KEY",
    "AKIA", // AWS access key id prefix
];

/// Merge-conflict marker prefixes.
const CONFLICT_MARKERS: &[&str] = &["<<<<<<< ", "=======", ">>>>>>> "];

/// Result of a sanitation pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SanitationReport {
    /// Hard blockers: the commit must not proceed until these are fixed.
    pub blockers: Vec<String>,
    /// Warnings: shown to the user but do not abort the commit.
    pub warnings: Vec<String>,
}

impl SanitationReport {
    /// True if the commit can proceed (no blockers).
    pub fn is_clean(&self) -> bool {
        self.blockers.is_empty()
    }

    /// Format the report for display in the TUI.
    pub fn format(&self) -> String {
        let mut out = String::new();
        if !self.blockers.is_empty() {
            out.push_str("🚫 Commit blocked:\n");
            for b in &self.blockers {
                out.push_str(&format!("  • {b}\n"));
            }
        }
        if !self.warnings.is_empty() {
            out.push_str("⚠️  Warnings:\n");
            for w in &self.warnings {
                out.push_str(&format!("  • {w}\n"));
            }
        }
        if out.is_empty() {
            out.push_str("✅ No sanitation issues found.");
        }
        out.trim_end().to_string()
    }
}

/// Run all pre-commit sanitation checks in the current working directory.
///
/// `max_file_size` is in bytes. If `None`, [`DEFAULT_MAX_FILE_SIZE`] is used.
/// `status_output` is the raw output of `git status --porcelain` from the
/// directory being checked. Passing it in makes the function easy to test
/// without needing a real git repo.
pub fn check_worktree(
    cwd: &Path,
    status_output: &str,
    max_file_size: Option<u64>,
) -> Result<SanitationReport, String> {
    let max = max_file_size.unwrap_or(DEFAULT_MAX_FILE_SIZE);
    let mut report = SanitationReport::default();

    if status_output.is_empty() {
        report
            .warnings
            .push("Working tree is clean — nothing to commit.".to_string());
        return Ok(report);
    }

    let changed = parse_status(status_output);

    if changed.is_empty() {
        report
            .warnings
            .push("No trackable changes found — nothing to commit.".to_string());
        return Ok(report);
    }

    // Large-file check.
    for entry in &changed {
        if entry.status == StatusCode::Deleted {
            continue;
        }
        let path = cwd.join(&entry.path);
        match std::fs::metadata(&path) {
            Ok(meta) => {
                let size = meta.len();
                if size > max {
                    report.blockers.push(format!(
                        "Large file ({} > {} limit): {}",
                        human_size(size),
                        human_size(max),
                        entry.path.display()
                    ));
                }
            }
            Err(e) => {
                report.warnings.push(format!(
                    "Could not check size of {}: {e}",
                    entry.path.display()
                ));
            }
        }
    }

    // Content scans (secrets + conflict markers) on readable files.
    for entry in &changed {
        if entry.status == StatusCode::Deleted {
            continue;
        }
        let path = cwd.join(&entry.path);
        let content = match read_limited(&path, SCAN_CAP_BYTES) {
            Some(c) => c,
            None => continue,
        };
        let lower = content.to_lowercase();

        for pattern in SECRET_PATTERNS {
            if lower.contains(&pattern.to_lowercase()) {
                report.blockers.push(format!(
                    "Possible secret/credential pattern '{}' in {}",
                    pattern,
                    entry.path.display()
                ));
                // One blocker per pattern is enough.
                break;
            }
        }

        if has_conflict_marker(&content) {
            report.blockers.push(format!(
                "Merge-conflict markers in {}",
                entry.path.display()
            ));
        }
    }

    // Untracked / unstaged debris warnings.
    let untracked: Vec<&StatusEntry> = changed
        .iter()
        .filter(|e| e.status == StatusCode::Untracked)
        .collect();
    if !untracked.is_empty() {
        let names: Vec<String> = untracked
            .iter()
            .map(|e| e.path.to_string_lossy().to_string())
            .take(5)
            .collect();
        let mut msg = format!("Untracked files will be committed: {}", names.join(", "));
        if untracked.len() > 5 {
            msg.push_str(&format!(" and {} more", untracked.len() - 5));
        }
        report.warnings.push(msg);
    }

    let unstaged: Vec<&StatusEntry> = changed
        .iter()
        .filter(|e| e.status == StatusCode::ModifiedUnstaged)
        .collect();
    if !unstaged.is_empty() {
        report.warnings.push(format!(
            "{} modified files are unstaged and will be added by `git add -A`",
            unstaged.len()
        ));
    }

    Ok(report)
}

/// Read at most `limit` bytes from `path` as a UTF-8 string.
/// Returns `None` if the file cannot be read or is not valid UTF-8.
fn read_limited(path: &Path, limit: u64) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0; limit as usize];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    String::from_utf8(buf).ok()
}

/// True if `text` contains a conflict-marker line.
fn has_conflict_marker(text: &str) -> bool {
    text.lines().any(|line| {
        CONFLICT_MARKERS
            .iter()
            .any(|marker| line.starts_with(marker))
    })
}

/// Render a byte count as human-readable `N KB`, `N MB`, etc.
fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let exp = (bytes as f64).log(1024.0).min(UNITS.len() as f64 - 1.0) as usize;
    let value = bytes as f64 / 1024_f64.powi(exp as i32);
    format!("{:.1} {}", value, UNITS[exp])
}

/// Status codes we care about from `git status --porcelain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusCode {
    Staged,
    ModifiedUnstaged,
    Untracked,
    Deleted,
    Other,
}

/// A single changed path parsed from `git status --porcelain`.
#[derive(Debug, Clone, PartialEq)]
struct StatusEntry {
    status: StatusCode,
    path: PathBuf,
}

/// Parse the output of `git status --porcelain`.
///
/// Each line is two status characters followed by a path. We only need a
/// coarse classification for the sanitation pass.
fn parse_status(output: &str) -> Vec<StatusEntry> {
    let mut entries = Vec::new();
    for line in output.lines() {
        if line.len() < 3 {
            continue;
        }
        let (code, rest) = line.split_at(2);
        // Handle rename/copy lines: "R  old -> new" or "C  old -> new".
        let path = if code.starts_with('R') || code.starts_with('C') {
            rest.split(" -> ")
                .last()
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| rest.trim().to_string())
        } else {
            rest.trim().to_string()
        };
        let status = classify(code);
        entries.push(StatusEntry {
            status,
            path: PathBuf::from(path),
        });
    }
    entries
}

/// Classify a two-letter `git status` code.
fn classify(code: &str) -> StatusCode {
    if code == "??" {
        return StatusCode::Untracked;
    }
    let staged = code.chars().next().unwrap_or(' ');
    let unstaged = code.chars().nth(1).unwrap_or(' ');
    if staged == 'D' || unstaged == 'D' {
        return StatusCode::Deleted;
    }
    if staged != ' ' && staged != '?' {
        return StatusCode::Staged;
    }
    if unstaged == 'M' {
        return StatusCode::ModifiedUnstaged;
    }
    StatusCode::Other
}

/// Suggest a conventional-commit style message from a diff-stat string.
///
/// This is intentionally simple: it looks at how many files changed and what
/// extensions dominate. A future pass can ask the model for a richer
/// message, but a deterministic suggestion is enough for the first version.
pub fn suggest_message(status_lines: &[String]) -> String {
    if status_lines.is_empty() {
        return "chore: no changes".to_string();
    }

    let mut extensions: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut any_rust = false;
    let mut any_docs = false;
    let mut any_tests = false;

    for path in status_lines.iter().map(|line| strip_status_code(line)) {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            *extensions.entry(ext.to_lowercase()).or_insert(0) += 1;
            match ext {
                "rs" => any_rust = true,
                "md" | "txt" | "adoc" => any_docs = true,
                _ => {}
            }
        }
        if path
            .file_stem()
            .map(|s| s.to_string_lossy().contains("test"))
            .unwrap_or(false)
        {
            any_tests = true;
        }
    }

    let scope = if extensions.len() == 1 {
        format!(
            "({})",
            extensions.keys().next().unwrap_or(&"misc".to_string())
        )
    } else {
        String::new()
    };

    let kind = if any_tests {
        "test"
    } else if any_rust {
        if any_docs {
            "docs"
        } else {
            "feat"
        }
    } else if any_docs {
        "docs"
    } else {
        "chore"
    };

    let desc = if status_lines.len() == 1 {
        let path = strip_status_code(&status_lines[0]);
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "files".to_string());
        format!("update {name}")
    } else {
        format!("update {} files", status_lines.len())
    };

    if scope.is_empty() {
        format!("{kind}: {desc}")
    } else {
        format!("{kind}{scope}: {desc}")
    }
}

/// Strip the two-letter `git status --porcelain` code and leading
/// whitespace from a line, returning the path. Handles rename lines
/// (`old -> new`) by returning the new path.
fn strip_status_code(line: &str) -> PathBuf {
    let after_code = line
        .trim_start_matches(char::is_whitespace)
        .chars()
        .skip(2)
        .collect::<String>();
    let trimmed = after_code.trim_start();
    if trimmed.contains(" -> ") {
        trimmed
            .split(" -> ")
            .last()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| trimmed.to_string())
            .into()
    } else {
        trimmed.to_string().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_status_handles_tracked_and_untracked() {
        let out = " M src/main.rs\nM  src/lib.rs\n?? target/foo\n D src/old.rs";
        let entries = parse_status(out);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].status, StatusCode::ModifiedUnstaged);
        assert_eq!(entries[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(entries[1].status, StatusCode::Staged);
        assert_eq!(entries[2].status, StatusCode::Untracked);
        assert_eq!(entries[3].status, StatusCode::Deleted);
    }

    #[test]
    fn parse_status_handles_rename() {
        let out = "R  src/old.rs -> src/new.rs";
        let entries = parse_status(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("src/new.rs"));
    }

    #[test]
    fn has_conflict_marker_detects_all_variants() {
        assert!(has_conflict_marker(
            "<<<<<<< HEAD\nfoo\n=======\nbar\n>>>>>>> branch"
        ));
        assert!(!has_conflict_marker("some normal text"));
    }

    #[test]
    fn suggest_message_single_file() {
        let lines = vec![" M src/main.rs".to_string()];
        let msg = suggest_message(&lines);
        assert!(msg.starts_with("feat(rs): update main.rs"), "got: {}", msg);
    }

    #[test]
    fn suggest_message_multiple_files_defaults_to_feat_rs() {
        let lines = vec![" M src/a.rs".to_string(), " M src/b.rs".to_string()];
        let msg = suggest_message(&lines);
        assert!(msg.starts_with("feat(rs): update 2 files"), "got: {}", msg);
    }

    #[test]
    fn suggest_message_docs_only() {
        let lines = vec![" M README.md".to_string()];
        let msg = suggest_message(&lines);
        assert!(
            msg.starts_with("docs(md): update README.md"),
            "got: {}",
            msg
        );
    }

    #[test]
    fn check_worktree_flags_large_file() {
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("big.bin");
        std::fs::write(&big, vec![0u8; 1024]).unwrap();
        let status = format!("?? {}", big.display());
        let report = check_worktree(tmp.path(), &status, Some(512)).unwrap();
        assert!(!report.is_clean());
        assert!(report.blockers.iter().any(|b| b.contains("Large file")));
    }

    #[test]
    fn check_worktree_flags_secret_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("config.toml");
        std::fs::write(&f, "api_key = sk-abc123").unwrap();
        let status = format!("?? {}", f.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(!report.is_clean());
        assert!(report.blockers.iter().any(|b| b.contains("sk-")));
    }

    #[test]
    fn check_worktree_flags_conflict_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("file.rs");
        std::fs::write(&f, "<<<<<<< HEAD\nfn main() {}\n>>>>>>> other").unwrap();
        let status = format!("?? {}", f.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(!report.is_clean());
        assert!(report.blockers.iter().any(|b| b.contains("conflict")));
    }

    #[test]
    fn check_worktree_warns_when_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let report = check_worktree(tmp.path(), "", None).unwrap();
        assert!(report.is_clean());
        assert!(report.warnings.iter().any(|w| w.contains("clean")));
    }

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
