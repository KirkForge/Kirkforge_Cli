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
            None => {
                tracing::debug!(path = %path.display(), "Skipped git-sanitation content scan (unreadable or non-UTF8 file)");
                continue;
            }
        };
        let lower = content.to_lowercase();

        for pattern in SECRET_PATTERNS {
            if contains_secret_pattern(&lower, pattern) {
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
        // `=======` must appear alone on a line; a run of eight or more
        // equals (e.g. a Markdown horizontal rule) must not match.
        line == "=======" || line.starts_with("<<<<<<< ") || line.starts_with(">>>>>>> ")
    })
}

/// Case-insensitive check for a secret pattern, with special handling for
/// `.env` so it matches a standalone filename/token (e.g. `.env`) but not
/// a path-like extension (e.g. `.env.local`) or a larger word
/// (e.g. `.environment`).
fn contains_secret_pattern(lower_text: &str, pattern: &str) -> bool {
    let pat = pattern.to_lowercase();
    if pat == ".env" {
        let mut start = 0;
        while let Some(pos) = lower_text[start..].find(&pat) {
            let abs = start + pos;
            let after_idx = abs + pat.len();

            let prev_ok = abs == 0
                || !lower_text
                    .chars()
                    .nth(abs - 1)
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let next_ok = after_idx >= lower_text.len()
                || !lower_text[after_idx..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');

            if prev_ok && next_ok {
                return true;
            }
            start = after_idx;
        }
        false
    } else {
        lower_text.contains(&pat)
    }
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
        assert!(msg.starts_with("feat(rs): update main.rs"), "got: {msg}");
    }

    #[test]
    fn suggest_message_multiple_files_defaults_to_feat_rs() {
        let lines = vec![" M src/a.rs".to_string(), " M src/b.rs".to_string()];
        let msg = suggest_message(&lines);
        assert!(msg.starts_with("feat(rs): update 2 files"), "got: {msg}");
    }

    #[test]
    fn suggest_message_docs_only() {
        let lines = vec![" M README.md".to_string()];
        let msg = suggest_message(&lines);
        assert!(msg.starts_with("docs(md): update README.md"), "got: {msg}");
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
    fn has_conflict_marker_rejects_overlong_equals_rule() {
        // Regression for C25: `========` used to match `=======`.
        assert!(!has_conflict_marker("======== horizontal rule"));
        assert!(has_conflict_marker("\n=======\n"));
    }

    #[test]
    fn check_worktree_env_extension_not_flagged() {
        // Regression for C25: `.env` substring matched `.env.local`.
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("script.sh");
        std::fs::write(&f, "source .env.local\n").unwrap();
        let status = format!("?? {}", f.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(
            report.blockers.iter().all(|b| !b.contains(".env")),
            "expected no .env false positive, got: {:?}",
            report.blockers
        );
    }

    #[test]
    fn check_worktree_standalone_env_still_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("script.sh");
        std::fs::write(&f, "cat .env\n").unwrap();
        let status = format!("?? {}", f.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(report.blockers.iter().any(|b| b.contains(".env")));
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

    #[test]
    fn human_size_gb_scales() {
        assert_eq!(human_size(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn parse_status_handles_copy() {
        let out = "C  src/old.rs -> src/copy.rs";
        let entries = parse_status(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("src/copy.rs"));
    }

    #[test]
    fn parse_status_skips_short_lines() {
        let out = "??\nM  src/lib.rs\n";
        let entries = parse_status(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn parse_status_handles_blank_lines() {
        let out = " M src/a.rs\n\nM  src/b.rs\n";
        let entries = parse_status(out);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn classify_staged_modified_untracked_deleted_other() {
        assert_eq!(classify("M "), StatusCode::Staged);
        assert_eq!(classify(" M"), StatusCode::ModifiedUnstaged);
        assert_eq!(classify("??"), StatusCode::Untracked);
        assert_eq!(classify("D "), StatusCode::Deleted);
        assert_eq!(classify(" D"), StatusCode::Deleted);
        assert_eq!(classify("A "), StatusCode::Staged);
        assert_eq!(classify(" X"), StatusCode::Other);
    }

    #[test]
    fn has_conflict_marker_handles_each_variant() {
        assert!(has_conflict_marker("<<<<<<< HEAD\nfoo\n"));
        assert!(has_conflict_marker(">>>>>>> branch\n"));
        assert!(has_conflict_marker("\n=======\n"));
        assert!(!has_conflict_marker("not a marker"));
        assert!(!has_conflict_marker("========= long equals rule"));
        assert!(!has_conflict_marker(""));
    }

    #[test]
    fn contains_secret_pattern_matches_known_prefixes() {
        let text = "token=ghp_abc123";
        assert!(contains_secret_pattern(&text.to_lowercase(), "ghp_"));
        assert!(contains_secret_pattern(&"key=sk-xyz".to_lowercase(), "sk-"));
        assert!(contains_secret_pattern(&"AKIA1234".to_lowercase(), "akia"));
    }

    #[test]
    fn contains_secret_pattern_env_in_word_is_rejected() {
        assert!(!contains_secret_pattern(
            &"environment variable".to_lowercase(),
            ".env"
        ));
        assert!(contains_secret_pattern(
            &"see .env for details".to_lowercase(),
            ".env"
        ));
    }

    #[test]
    fn check_worktree_no_trackable_changes_warns() {
        let tmp = tempfile::tempdir().unwrap();
        let report = check_worktree(tmp.path(), "??\n  \n", None).unwrap();
        assert!(report.warnings.iter().any(|w| w.contains("trackable")));
    }

    #[test]
    fn check_worktree_reports_untracked_files_in_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("u.txt");
        std::fs::write(&f, "x").unwrap();
        let status = format!("?? {}", f.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(report.warnings.iter().any(|w| w.contains("Untracked")));
    }

    #[test]
    fn check_worktree_reports_unstaged_modified_files_in_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("m.txt");
        std::fs::write(&f, "x").unwrap();
        let status = format!(" M {}", f.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(report.warnings.iter().any(|w| w.contains("unstaged")));
    }

    #[test]
    fn check_worktree_skips_deleted_files_for_size_and_content() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("gone.txt");
        let status = format!(" D {}", f.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(report.is_clean(), "deleted file should not block");
        assert!(
            !report
                .blockers
                .iter()
                .any(|b| b.contains("Large file") || b.contains("secret")),
            "deleted file should not be scanned: {report:?}"
        );
    }

    #[test]
    fn check_worktree_unreadable_file_warns_about_size() {
        let tmp = tempfile::tempdir().unwrap();
        let status = "?? /nonexistent/kirkforge-test-missing-file".to_string();
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("Could not check size")));
    }

    #[test]
    fn check_worktree_flags_more_than_five_untracked_files_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..7 {
            let f = tmp.path().join(format!("u{i}.txt"));
            std::fs::write(&f, "x").unwrap();
        }
        let status: Vec<String> = (0..7)
            .map(|i| format!("?? {}/u{i}.txt", tmp.path().display()))
            .collect();
        let report = check_worktree(tmp.path(), &status.join("\n"), None).unwrap();
        let untracked_warning = report
            .warnings
            .iter()
            .find(|w| w.contains("Untracked"))
            .expect("untracked warning present");
        assert!(
            untracked_warning.contains("and 2 more"),
            "should mention 2 extra files, got: {untracked_warning}"
        );
    }

    #[test]
    fn check_worktree_default_max_file_size_is_five_mb() {
        assert_eq!(DEFAULT_MAX_FILE_SIZE, 5 * 1024 * 1024);
    }

    #[test]
    fn suggest_message_empty_returns_chore_no_changes() {
        assert_eq!(suggest_message(&[]), "chore: no changes");
    }

    #[test]
    fn suggest_message_test_files_get_test_kind() {
        let lines = vec![" M tests/foo_test.rs".to_string()];
        let msg = suggest_message(&lines);
        assert!(msg.starts_with("test"), "got: {msg}");
    }

    #[test]
    fn suggest_message_mixed_extensions_no_scope() {
        let lines = vec![" M src/main.rs".to_string(), " M README.md".to_string()];
        let msg = suggest_message(&lines);
        assert!(
            msg.starts_with("feat") || msg.starts_with("docs"),
            "mixed ext should pick feat or docs kind, got: {msg}"
        );
        assert!(
            !msg.contains("("),
            "mixed ext should have no scope, got: {msg}"
        );
    }

    #[test]
    fn suggest_message_strip_status_handles_rename() {
        let line = "R  src/old.rs -> src/new.rs";
        let path = strip_status_code(line);
        assert_eq!(path, PathBuf::from("src/new.rs"));
    }

    #[test]
    fn strip_status_code_handles_simple_path() {
        assert_eq!(
            strip_status_code(" M src/lib.rs"),
            PathBuf::from("src/lib.rs")
        );
    }

    #[test]
    fn strip_status_code_handles_untracked_path() {
        assert_eq!(
            strip_status_code("?? untracked.txt"),
            PathBuf::from("untracked.txt")
        );
    }

    #[test]
    fn sanitation_report_format_clean() {
        let report = SanitationReport::default();
        assert_eq!(report.format(), "✅ No sanitation issues found.");
    }

    #[test]
    fn sanitation_report_format_with_blockers_and_warnings() {
        let report = SanitationReport {
            blockers: vec!["blocker one".into()],
            warnings: vec!["warning one".into()],
        };
        let s = report.format();
        assert!(s.contains("🚫 Commit blocked:"));
        assert!(s.contains("  • blocker one"));
        assert!(s.contains("⚠️  Warnings:"));
        assert!(s.contains("  • warning one"));
    }

    #[test]
    fn read_limited_returns_none_for_unreadable() {
        let path = Path::new("/nonexistent/kirkforge-test-read-limited.bin");
        assert!(read_limited(path, 1024).is_none());
    }

    #[test]
    fn read_limited_truncates_to_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("limited.txt");
        std::fs::write(&path, "abcdefghij").unwrap();
        let out = read_limited(&path, 4).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out, "abcd");
    }

    #[test]
    fn read_limited_rejects_non_utf8() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("binary.bin");
        std::fs::write(&path, b"\xff\xfe\xfd").unwrap();
        assert!(read_limited(&path, 1024).is_none());
    }

    #[test]
    fn human_size_tb_scales() {
        let tb = 3 * 1024 * 1024 * 1024 * 1024;
        assert_eq!(human_size(tb), "3.0 TB");
    }

    #[test]
    fn human_size_small_bytes_stays_in_bytes() {
        assert_eq!(human_size(512), "512.0 B");
    }

    #[test]
    fn human_size_one_kb_boundary() {
        assert_eq!(human_size(1023), "1023.0 B");
        assert_eq!(human_size(1024), "1.0 KB");
    }

    #[test]
    fn parse_status_handles_modified_both_staged_and_unstaged() {
        let out = "MM src/both.rs";
        let entries = parse_status(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, StatusCode::Staged);
    }

    #[test]
    fn parse_status_handles_added_staged() {
        let out = "A  src/new.rs";
        let entries = parse_status(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, StatusCode::Staged);
    }

    #[test]
    fn parse_status_handles_unknown_code_as_other() {
        let out = " X src/weird.rs";
        let entries = parse_status(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, StatusCode::Other);
    }

    #[test]
    fn parse_status_empty_output_returns_empty_vec() {
        let entries = parse_status("");
        assert!(entries.is_empty());
    }

    #[test]
    fn parse_status_only_whitespace_lines_returns_empty_vec() {
        let entries = parse_status("\n\n\n");
        assert!(entries.is_empty());
    }

    #[test]
    fn classify_empty_code_returns_other() {
        assert_eq!(classify("  "), StatusCode::Other);
    }

    #[test]
    fn classify_single_char_code_returns_staged_or_other() {
        // "X" has staged='X' (non-space, non-?) so it's Staged.
        assert_eq!(classify("X"), StatusCode::Staged);
    }

    #[test]
    fn classify_double_question_mark_is_untracked() {
        assert_eq!(classify("??"), StatusCode::Untracked);
    }

    #[test]
    fn classify_d_in_staged_position_is_deleted() {
        assert_eq!(classify("D "), StatusCode::Deleted);
    }

    #[test]
    fn classify_d_in_unstaged_position_is_deleted() {
        assert_eq!(classify(" D"), StatusCode::Deleted);
    }

    #[test]
    fn classify_double_d_is_deleted() {
        assert_eq!(classify("DD"), StatusCode::Deleted);
    }

    #[test]
    fn classify_m_staged_is_staged() {
        assert_eq!(classify("M "), StatusCode::Staged);
        assert_eq!(classify("A "), StatusCode::Staged);
        assert_eq!(classify("R "), StatusCode::Staged);
        assert_eq!(classify("C "), StatusCode::Staged);
    }

    #[test]
    fn has_conflict_marker_only_seven_equals_exact() {
        assert!(has_conflict_marker("======="));
        assert!(!has_conflict_marker("======"));
        assert!(!has_conflict_marker("========"));
    }

    #[test]
    fn has_conflict_marker_only_exact_start_marker() {
        assert!(has_conflict_marker("<<<<<<< HEAD"));
        assert!(!has_conflict_marker("<<<<<<<HEAD"));
        assert!(!has_conflict_marker("<<<<<< HEAD"));
    }

    #[test]
    fn has_conflict_marker_only_exact_end_marker() {
        assert!(has_conflict_marker(">>>>>>> branch"));
        assert!(!has_conflict_marker(">>>>>>>> branch"));
        assert!(!has_conflict_marker(">>>>>> branch"));
    }

    #[test]
    fn contains_secret_pattern_detects_id_rsa() {
        assert!(contains_secret_pattern(
            &"path/to/id_rsa".to_lowercase(),
            "id_rsa"
        ));
    }

    #[test]
    fn contains_secret_pattern_detects_id_ed25519() {
        assert!(contains_secret_pattern(
            &"path/to/id_ed25519".to_lowercase(),
            "id_ed25519"
        ));
    }

    #[test]
    fn contains_secret_pattern_detects_github_pat_underscore() {
        assert!(contains_secret_pattern(
            &"github_pat_abc123".to_lowercase(),
            "github_pat_"
        ));
    }

    #[test]
    fn contains_secret_pattern_detects_glpat() {
        assert!(contains_secret_pattern(
            &"glpat-xyz".to_lowercase(),
            "glpat-"
        ));
    }

    #[test]
    fn contains_secret_pattern_detects_begin_rsa_private_key() {
        assert!(contains_secret_pattern(
            &"-----BEGIN RSA PRIVATE KEY-----".to_lowercase(),
            "begin rsa private key"
        ));
    }

    #[test]
    fn contains_secret_pattern_detects_begin_openssh_private_key() {
        assert!(contains_secret_pattern(
            &"-----BEGIN OPENSSH PRIVATE KEY-----".to_lowercase(),
            "begin openssh private key"
        ));
    }

    #[test]
    fn contains_secret_pattern_env_at_start_of_text() {
        assert!(contains_secret_pattern(&".env".to_lowercase(), ".env"));
    }

    #[test]
    fn contains_secret_pattern_env_at_end_of_text() {
        assert!(contains_secret_pattern(&"see .env".to_lowercase(), ".env"));
    }

    #[test]
    fn contains_secret_pattern_env_standalone_word() {
        assert!(contains_secret_pattern(
            &"cat .env now".to_lowercase(),
            ".env"
        ));
    }

    #[test]
    fn contains_secret_pattern_env_with_dot_after_is_rejected() {
        assert!(!contains_secret_pattern(
            &".env.local".to_lowercase(),
            ".env"
        ));
        assert!(!contains_secret_pattern(
            &".environment".to_lowercase(),
            ".env"
        ));
    }

    #[test]
    fn contains_secret_pattern_env_with_alphanumeric_before_is_rejected() {
        assert!(!contains_secret_pattern(&"x.env".to_lowercase(), ".env"));
    }

    #[test]
    fn contains_secret_pattern_env_with_underscore_after_is_rejected() {
        assert!(!contains_secret_pattern(&".env_var".to_lowercase(), ".env"));
    }

    #[test]
    fn suggest_message_single_file_no_extension() {
        let lines = vec![" M Makefile".to_string()];
        let msg = suggest_message(&lines);
        assert!(
            msg.starts_with("chore") || msg.starts_with("feat"),
            "no-ext file should pick chore or feat, got: {msg}"
        );
        assert!(msg.contains("Makefile"), "got: {msg}");
    }

    #[test]
    fn suggest_message_single_file_with_test_stem() {
        let lines = vec![" M src/lib_test.rs".to_string()];
        let msg = suggest_message(&lines);
        assert!(
            msg.starts_with("test"),
            "test stem should pick test kind, got: {msg}"
        );
    }

    #[test]
    fn suggest_message_single_file_txt_extension_is_docs() {
        let lines = vec![" M NOTES.txt".to_string()];
        let msg = suggest_message(&lines);
        assert!(msg.starts_with("docs"), "got: {msg}");
    }

    #[test]
    fn suggest_message_rust_and_docs_picks_docs_kind() {
        let lines = vec![" M src/main.rs".to_string(), " M README.md".to_string()];
        let msg = suggest_message(&lines);
        assert!(
            msg.starts_with("docs"),
            "rust+docs should pick docs, got: {msg}"
        );
    }

    #[test]
    fn strip_status_code_handles_rename_with_spaces() {
        let path = strip_status_code("R  old name.rs -> new name.rs");
        assert_eq!(path, PathBuf::from("new name.rs"));
    }

    #[test]
    fn strip_status_code_handles_copy_with_spaces() {
        let path = strip_status_code("C  old.rs -> copy.rs");
        assert_eq!(path, PathBuf::from("copy.rs"));
    }

    #[test]
    fn check_worktree_staged_file_is_scanned_for_size() {
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("staged.bin");
        std::fs::write(&big, vec![0u8; 2048]).unwrap();
        let status = format!("A  {}", big.display());
        let report = check_worktree(tmp.path(), &status, Some(1024)).unwrap();
        assert!(!report.is_clean());
        assert!(report.blockers.iter().any(|b| b.contains("Large file")));
    }

    #[test]
    fn check_worktree_modified_unstaged_is_scanned_for_size() {
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("modified.bin");
        std::fs::write(&big, vec![0u8; 2048]).unwrap();
        let status = format!(" M {}", big.display());
        let report = check_worktree(tmp.path(), &status, Some(1024)).unwrap();
        assert!(!report.is_clean());
        assert!(report.blockers.iter().any(|b| b.contains("Large file")));
    }

    #[test]
    fn check_worktree_default_max_used_when_none() {
        let tmp = tempfile::tempdir().unwrap();
        let normal = tmp.path().join("normal.txt");
        std::fs::write(&normal, "small").unwrap();
        let status = format!("?? {}", normal.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(report.is_clean(), "small file should pass default limit");
    }

    #[test]
    fn check_worktree_unstaged_only_in_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("unstaged.txt");
        std::fs::write(&f, "x").unwrap();
        let status = format!(" M {}", f.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(
            report.warnings.iter().any(|w| w.contains("unstaged")),
            "got: {:?}",
            report.warnings
        );
    }

    #[test]
    fn check_worktree_mixed_staged_and_unstaged() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = tmp.path().join("staged.txt");
        let unstaged = tmp.path().join("unstaged.txt");
        std::fs::write(&staged, "s").unwrap();
        std::fs::write(&unstaged, "u").unwrap();
        let status = format!("A  {}\n M {}", staged.display(), unstaged.display());
        let report = check_worktree(tmp.path(), &status, None).unwrap();
        assert!(report.warnings.iter().any(|w| w.contains("unstaged")));
    }

    #[test]
    fn sanitation_report_format_only_warnings() {
        let report = SanitationReport {
            blockers: vec![],
            warnings: vec!["just a warning".into()],
        };
        let s = report.format();
        assert!(s.contains("⚠️  Warnings:"));
        assert!(s.contains("just a warning"));
        assert!(!s.contains("Commit blocked"));
    }

    #[test]
    fn sanitation_report_is_clean_true_when_no_blockers() {
        let report = SanitationReport {
            blockers: vec![],
            warnings: vec!["w".into()],
        };
        assert!(report.is_clean(), "warnings don't block");
    }

    #[test]
    fn sanitation_report_is_clean_false_with_blockers() {
        let report = SanitationReport {
            blockers: vec!["b".into()],
            warnings: vec![],
        };
        assert!(!report.is_clean());
    }

    #[test]
    fn sanitation_report_default_is_clean() {
        assert!(SanitationReport::default().is_clean());
    }

    #[test]
    fn read_limited_empty_file_returns_empty_string() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();
        let out = read_limited(&path, 1024).unwrap();
        assert_eq!(out, "");
    }

    #[test]
    fn read_limited_reads_only_limit_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.txt");
        std::fs::write(&path, "0123456789").unwrap();
        let out = read_limited(&path, 4).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out, "0123");
    }

    #[test]
    fn check_worktree_returns_ok_for_empty_status() {
        let tmp = tempfile::tempdir().unwrap();
        let report = check_worktree(tmp.path(), "", None).unwrap();
        assert!(report.is_clean());
    }
}
