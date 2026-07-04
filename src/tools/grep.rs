use crate::session::access::{GuardVerdict, PathGuard};
use crate::shared::{Match as SearchMatch, ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::path::PathBuf;

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
                        "description": "Literal substring to search for (not a regex)"
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
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing 'pattern' argument",
                ));
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
                if results.len() >= max_matches {
                    break;
                }
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
                    let matches = find_matches(&content, &pattern, file_path, context_lines);
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
                return ToolOutcome::Failure(ToolError::AccessDenied {
                    message: msg,
                });
            }
            if let Ok(content) = std::fs::read_to_string(&search_path) {
                let matches = find_matches(&content, &pattern, &search_path, context_lines);
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
) -> Vec<SearchMatch> {
    let mut results = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        if line.contains(pattern) {
            // Capture context before
            let before_start = i.saturating_sub(context);
            let context_before: Vec<String> = lines[before_start..i]
                .iter()
                .enumerate()
                .map(|(j, l)| format!("{}:{}", before_start + j + 1, l))
                .collect();

            let context_after: Vec<String> = lines[i + 1..=(i + context).min(lines.len() - 1)]
                .iter()
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
