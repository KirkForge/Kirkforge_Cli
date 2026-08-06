use crate::session::access::{GuardVerdict, PathGuard};
use crate::shared::{Match as SearchMatch, ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::path::PathBuf;

/// Quick check whether a string looks like it contains regex metacharacters.
/// If not, we can use the much faster `str::contains` path.
fn looks_like_regex(pattern: &str) -> bool {
    pattern.contains(|c: char| ".|^$*+?()[]{}\\|".contains(c))
}

/// Maximum file size in bytes we'll attempt to read for grep (10 MB).
const MAX_GREP_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum bytes read from a file at once for the content-based binary check.
const BINARY_SCAN_BYTES: usize = 8192;

pub struct Grep {
    path_guard: PathGuard,
}

impl Grep {
    pub fn new(path_guard: PathGuard) -> Self {
        Self { path_guard }
    }
}

#[async_trait::async_trait]
impl Tool for Grep {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "grep",
            description: "Search for a pattern in files using recursive grep. Returns matching lines with context.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Pattern to search for. Treated as a literal substring unless it contains regex metacharacters, in which case it is matched as a regular expression."
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search (default: current directory)",
                        "default": "."
                    },
                    "context_lines": {
                        "type": "integer",
                        "description": "Number of context lines before and after each match (default: 2)",
                        "default": 2
                    },
                    "max_matches": {
                        "type": "integer",
                        "description": "Maximum matches to return (default: 50)",
                        "default": 50
                    },
                    "regex": {
                        "type": "boolean",
                        "description": "Force regex matching even if the pattern has no metacharacters (default: false)",
                        "default": false
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'pattern' argument"));
            }
        };

        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
        let context_lines = args
            .get("context_lines")
            .and_then(|c| c.as_u64())
            .unwrap_or(2) as usize;
        let max_matches = args
            .get("max_matches")
            .and_then(|m| m.as_u64())
            .unwrap_or(50) as usize;
        let force_regex = args.get("regex").and_then(|r| r.as_bool()).unwrap_or(false);
        let use_regex = force_regex || looks_like_regex(&pattern);

        let search_path = PathBuf::from(shellexpand::tilde(path).as_ref());

        let mut results = Vec::new();
        let mut total = 0usize;

        if search_path.is_dir() {
            let walker = ignore::WalkBuilder::new(&search_path)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build();

            for entry in walker.flatten() {
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }

                let file_path = entry.path();

                // ── Extension-based binary pre-check (fast path) ──
                if is_binary_by_ext(file_path) {
                    continue;
                }

                // ── Size check (skip files that are too large) ──
                if let Ok(meta) = std::fs::metadata(file_path) {
                    if meta.len() > MAX_GREP_FILE_SIZE {
                        continue;
                    }
                }

                // ── Content-based binary detection (read first 8K) ──
                if is_binary_content(file_path) {
                    continue;
                }

                // ── PathGuard read check per file (catches symlinks and
                //    paths outside the sandbox that the walker may have
                //    followed from an in-sandbox starting point).
                if let GuardVerdict::Denied(_) = self.path_guard.check_read(file_path) {
                    continue;
                }

                if let Ok(content) = std::fs::read_to_string(file_path) {
                    let matches =
                        find_matches(&content, &pattern, file_path, context_lines, use_regex);
                    let count = matches.len();
                    if count > 0 {
                        total += count;
                        results.extend(matches);
                    }
                }
            }
        } else if search_path.is_file() {
            // ── Size + binary checks for single-file search ──
            if let Ok(meta) = std::fs::metadata(&search_path) {
                if meta.len() > MAX_GREP_FILE_SIZE {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!(
                            "File too large to search ({} bytes): {}",
                            meta.len(),
                            search_path.display()
                        ),
                    });
                }
            }
            if is_binary_content(&search_path) {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("Cannot search binary file: {}", search_path.display()),
                });
            }
            if let GuardVerdict::Denied(msg) = self.path_guard.check_read(&search_path) {
                return ToolOutcome::Failure(ToolError::AccessDenied { message: msg });
            }
            if let Ok(content) = std::fs::read_to_string(&search_path) {
                let matches =
                    find_matches(&content, &pattern, &search_path, context_lines, use_regex);
                total = matches.len();
                results = matches;
            }
        } else {
            return ToolOutcome::Failure(ToolError::Internal {
                message: format!("Path not found: {}", search_path.display()),
            });
        }

        if results.len() > max_matches {
            results.truncate(max_matches);
        }

        if results.is_empty() {
            return ToolOutcome::Success {
                content: format!("No matches found for pattern: {pattern}"),
            };
        }

        ToolOutcome::GrepMatches {
            path: search_path,
            matches: results,
            total,
        }
    }
}

fn find_matches(
    content: &str,
    pattern: &str,
    _file_path: &std::path::Path,
    context: usize,
    use_regex: bool,
) -> Vec<SearchMatch> {
    let mut results = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    let re = if use_regex {
        match regex::Regex::new(pattern) {
            Ok(r) => Some(r),
            Err(_) => {
                for (i, line) in lines.iter().enumerate() {
                    if line.contains(pattern) {
                        let before_start = i.saturating_sub(context);
                        let context_before: Vec<String> = lines[before_start..i]
                            .iter()
                            .enumerate()
                            .map(|(j, l)| format!("{}:{}", before_start + j + 1, l))
                            .collect();
                        let context_after: Vec<String> = lines
                            .iter()
                            .skip(i + 1)
                            .take(context)
                            .enumerate()
                            .map(|(j, l)| format!("{}:{}", i + j + 2, l))
                            .collect();
                        results.push(SearchMatch {
                            line_number: i + 1,
                            line: line.to_string(),
                            context_before,
                            context_after,
                        });
                    }
                }
                return results;
            }
        }
    } else {
        None
    };

    for (i, line) in lines.iter().enumerate() {
        let matched = match &re {
            Some(r) => r.is_match(line),
            None => line.contains(pattern),
        };
        if matched {
            // Capture context before
            let before_start = i.saturating_sub(context);
            let context_before: Vec<String> = lines[before_start..i]
                .iter()
                .enumerate()
                .map(|(j, l)| format!("{}:{}", before_start + j + 1, l))
                .collect();

            let context_after: Vec<String> = lines
                .iter()
                .skip(i + 1)
                .take(context)
                .enumerate()
                .map(|(j, l)| format!("{}:{}", i + j + 2, l))
                .collect();

            results.push(SearchMatch {
                line_number: i + 1,
                line: line.to_string(),
                context_before,
                context_after,
            });
        }
    }

    results
}

/// Fast extension-based binary check.
fn is_binary_by_ext(path: &std::path::Path) -> bool {
    let binary_extensions = [
        "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "ttf", "otf", "woff", "woff2", "eot",
        "mp3", "mp4", "avi", "mov", "mkv", "webm", "zip", "tar", "gz", "bz2", "xz", "zst", "pdf",
        "doc", "docx", "xls", "xlsx", "wasm", "o", "so", "dylib", "exe", "dll", "pyc", "class",
    ];

    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| binary_extensions.contains(&e))
        .unwrap_or(false)
}

/// Content-based binary detection — reads the first 8K and checks for null bytes.
fn is_binary_content(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = vec![0u8; BINARY_SCAN_BYTES];
    let n = file.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0x00)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use std::path::Path;

    /// Matching the last line must not panic when building context_after.
    #[test]
    fn find_matches_last_line_no_context_panic() {
        let content = "line one\nline two\nfn main() {}";
        let matches = find_matches(content, "main", Path::new("test.rs"), 2, false);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_number, 3);
        assert!(matches[0].context_after.is_empty());
    }

    #[test]
    fn find_matches_first_line_has_no_context_before() {
        let content = "needle here\nsecond line\nthird line";
        let matches = find_matches(content, "needle", Path::new("test.rs"), 2, false);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_number, 1);
        assert!(matches[0].context_before.is_empty());
        assert_eq!(matches[0].context_after.len(), 2);
        assert!(matches[0].context_after[0].starts_with("2:"));
    }

    #[test]
    fn find_matches_returns_multiple_for_multiple_occurrences() {
        let content = "needle\nother\nneedle\nother\nneedle";
        let matches = find_matches(content, "needle", Path::new("f.txt"), 0, false);
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].line_number, 1);
        assert_eq!(matches[1].line_number, 3);
        assert_eq!(matches[2].line_number, 5);
        for m in &matches {
            assert!(m.context_before.is_empty());
            assert!(m.context_after.is_empty());
        }
    }

    #[test]
    fn find_matches_zero_context_lines_still_records_match() {
        let content = "a\nneedle\nb";
        let matches = find_matches(content, "needle", Path::new("f.txt"), 0, false);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_number, 2);
        assert!(matches[0].context_before.is_empty());
        assert!(matches[0].context_after.is_empty());
    }

    #[test]
    fn find_matches_empty_pattern_matches_every_line() {
        let content = "a\nb\nc";
        let matches = find_matches(content, "", Path::new("f.txt"), 0, false);
        assert_eq!(matches.len(), 3, "empty pattern matches every line");
    }

    #[test]
    fn find_matches_no_matches_returns_empty() {
        let matches = find_matches("a\nb\nc", "zzz", Path::new("f.txt"), 2, false);
        assert!(matches.is_empty());
    }

    #[test]
    fn is_binary_by_ext_recognizes_known_extensions() {
        for ext in [
            "png", "jpg", "jpeg", "gif", "zip", "tar", "gz", "pdf", "mp3", "mp4", "exe", "so",
            "wasm", "pyc", "class",
        ] {
            let fname = format!("file.{ext}");
            let path = Path::new(&fname);
            assert!(is_binary_by_ext(path), "{ext} should be binary");
        }
    }

    #[test]
    fn is_binary_by_ext_allows_text_extensions() {
        for ext in ["rs", "py", "js", "ts", "go", "txt", "md", "toml", "json"] {
            let fname = format!("file.{ext}");
            let path = Path::new(&fname);
            assert!(!is_binary_by_ext(path), "{ext} should not be binary");
        }
    }

    #[test]
    fn is_binary_by_ext_no_extension_is_not_binary() {
        assert!(!is_binary_by_ext(Path::new("Makefile")));
        assert!(!is_binary_by_ext(Path::new("file")));
    }

    #[test]
    fn is_binary_content_empty_file_is_not_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();
        assert!(!is_binary_content(&path));
    }

    #[test]
    fn is_binary_content_text_file_is_not_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("text.txt");
        std::fs::write(&path, "hello world\nthis is text\n").unwrap();
        assert!(!is_binary_content(&path));
    }

    #[test]
    fn is_binary_content_null_byte_file_is_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, b"abc\x00def").unwrap();
        assert!(is_binary_content(&path));
    }

    #[test]
    fn is_binary_content_missing_file_returns_false() {
        assert!(!is_binary_content(Path::new("/nonexistent/kf-code/file")));
    }

    #[tokio::test]
    async fn grep_missing_pattern_arg_is_invalid_args() {
        let grep = Grep::new(PathGuard::default());
        let outcome = grep
            .run(&ToolContext::default(), serde_json::json!({}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn grep_nonexistent_path_is_internal_error() {
        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "needle",
            "path": "/nonexistent/kf-code/path/does/not/exist"
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { ref message }) if message.contains("Path not found")),
            "expected Path not found, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn grep_no_matches_returns_success_with_message() {
        let dir = std::env::temp_dir().join("kf_code_grep_nomatch_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "nothing relevant here\n").unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "needle",
            "path": dir.to_string_lossy(),
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("No matches found"), "got: {content}");
                assert!(content.contains("needle"));
            }
            other => panic!("expected Success, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_single_file_match_returns_grep_matches() {
        let dir = std::env::temp_dir().join("kf_code_grep_singlefile_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "line one\nneedle line\nline three\n").unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "needle",
            "path": path.to_string_lossy(),
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::GrepMatches { matches, total, .. } => {
                assert_eq!(total, 1);
                assert_eq!(matches.len(), 1);
                assert_eq!(matches[0].line_number, 2);
                assert!(matches[0].line.contains("needle"));
            }
            other => panic!("expected GrepMatches, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_skips_binary_files_by_extension() {
        let dir = std::env::temp_dir().join("kf_code_grep_skipbinext_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), "needle inside a png\n").unwrap();
        std::fs::write(dir.join("a.txt"), "needle inside txt\n").unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "needle",
            "path": dir.to_string_lossy(),
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::GrepMatches { matches, .. } => {
                assert_eq!(
                    matches.len(),
                    1,
                    "only the .txt should match, got: {matches:?}"
                );
                assert!(matches[0].line.contains("txt"));
            }
            other => panic!("expected GrepMatches, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_skips_files_larger_than_max() {
        let dir = std::env::temp_dir().join("kf_code_grep_bigfile_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big = "x".repeat(11 * 1024 * 1024);
        std::fs::write(dir.join("big.txt"), &big).unwrap();
        std::fs::write(dir.join("small.txt"), "needle small\n").unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "needle",
            "path": dir.to_string_lossy(),
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::GrepMatches { matches, .. } => {
                assert_eq!(matches.len(), 1);
                assert!(matches[0].line.contains("small"));
            }
            other => panic!("expected GrepMatches, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_skips_binary_files_by_content() {
        let dir = std::env::temp_dir().join("kf_code_grep_bincontent_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.dat"), b"needle\x00\x01\x02").unwrap();
        std::fs::write(dir.join("a.txt"), "needle text\n").unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "needle",
            "path": dir.to_string_lossy(),
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::GrepMatches { matches, .. } => {
                assert_eq!(
                    matches.len(),
                    1,
                    "only the text file should match, got {matches:?}"
                );
                assert!(matches[0].line.contains("text"));
            }
            other => panic!("expected GrepMatches, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_single_binary_file_returns_internal_error() {
        let dir = std::env::temp_dir().join("kf_code_grep_singlebin_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.dat");
        std::fs::write(&path, b"abc\x00def").unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "abc",
            "path": path.to_string_lossy(),
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { ref message }) if message.contains("binary file")),
            "expected binary-file rejection, got {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_single_oversized_file_returns_internal_error() {
        let dir = std::env::temp_dir().join("kf_code_grep_singlebig_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.txt");
        let big = "x".repeat(11 * 1024 * 1024);
        std::fs::write(&path, &big).unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "x",
            "path": path.to_string_lossy(),
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { ref message }) if message.contains("File too large")),
            "expected too-large rejection, got {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_context_lines_default_is_two() {
        let dir = std::env::temp_dir().join("kf_code_grep_ctxdefault_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "l1\nl2\nneedle\nl4\nl5\n").unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "needle",
            "path": dir.to_string_lossy(),
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::GrepMatches { matches, .. } => {
                assert_eq!(matches.len(), 1);
                assert_eq!(matches[0].context_before.len(), 2, "default context is 2");
                assert_eq!(matches[0].context_after.len(), 2);
            }
            other => panic!("expected GrepMatches, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grep_def_has_correct_name_and_required_pattern() {
        let grep = Grep::new(PathGuard::default());
        let def = grep.def();
        assert_eq!(def.name, "grep");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("pattern")));
    }

    #[tokio::test]
    async fn grep_total_counts_all_matches_not_just_collected() {
        let dir = std::env::temp_dir().join("kf_code_grep_total_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "needle\nneedle\nneedle\n").unwrap();
        std::fs::write(dir.join("b.txt"), "needle\nneedle\n").unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "needle",
            "path": dir.to_string_lossy(),
            "max_matches": 2
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::GrepMatches { matches, total, .. } => {
                assert_eq!(matches.len(), 2, "results should be capped at max_matches");
                assert_eq!(total, 5, "total should count all matches across files");
            }
            other => panic!("expected GrepMatches, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_matches_regex_pattern() {
        let content = "fn hello() {}\nfn world() {}\nfn foo_bar() {}";
        let matches = find_matches(content, r"fn \w+\(\)", Path::new("test.rs"), 0, true);
        assert_eq!(matches.len(), 3);
        assert!(matches[0].line.contains("hello"));
        assert!(matches[1].line.contains("world"));
        assert!(matches[2].line.contains("foo_bar"));
    }

    #[test]
    fn find_matches_auto_regex_with_metacharacters() {
        let content = "log::Error\nlog::info\nlog::debug";
        let matches = find_matches(content, r"log::\w+", Path::new("test.rs"), 0, false);
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn find_matches_invalid_regex_falls_back_to_literal() {
        let content = "fn foo(x: [invalid\nfn bar()";
        let matches = find_matches(content, "[invalid", Path::new("test.rs"), 0, true);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].line.contains("[invalid"));
    }

    #[tokio::test]
    async fn grep_regex_metachar_triggers_regex_mode() {
        let dir = std::env::temp_dir().join("kf_code_grep_regex_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "fn hello() {}\nfn world() {}\n").unwrap();

        let grep = Grep::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": r"fn \w+\(\)",
            "path": dir.to_string_lossy(),
        });
        let outcome = grep.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::GrepMatches { matches, total, .. } => {
                assert_eq!(total, 2);
                assert_eq!(matches.len(), 2);
            }
            other => panic!("expected GrepMatches, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grep_def_includes_regex_property() {
        let grep = Grep::new(PathGuard::default());
        let def = grep.def();
        let props = def
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("regex"),
            "def should include regex property"
        );
    }
}
