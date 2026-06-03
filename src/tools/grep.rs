use crate::shared::{Match as SearchMatch, ToolDef, ToolOutcome};
use crate::tools::Tool;
use std::path::PathBuf;

pub struct Grep;

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
                        "description": "Pattern to search for (literal string or regex)"
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

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutcome::Error { message: "Missing 'pattern' argument".into() },
        };

        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
        let context_lines = args.get("context_lines").and_then(|c| c.as_u64()).unwrap_or(2) as usize;
        let max_matches = args.get("max_matches").and_then(|m| m.as_u64()).unwrap_or(50) as usize;

        let search_path = PathBuf::from(shellexpand::tilde(path).as_ref());

        let mut results = Vec::new();
        let mut total = 0usize;

        if search_path.is_dir() {
            let walker = ignore::WalkBuilder::new(&search_path)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build();

            for entry in walker.flatten().take(max_matches * 5) {
                if results.len() >= max_matches {
                    break;
                }
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }

                let file_path = entry.path();
                // Skip binary-looking files
                if is_binary(file_path) {
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
            if let Ok(content) = std::fs::read_to_string(&search_path) {
                let matches = find_matches(&content, &pattern, &search_path, context_lines);
                total = matches.len();
                results = matches;
            }
        } else {
            return ToolOutcome::Error {
                message: format!("Path not found: {}", search_path.display()),
            };
        }

        if results.len() > max_matches {
            results.truncate(max_matches);
        }

        if results.is_empty() {
            return ToolOutcome::Success {
                content: format!("No matches found for pattern: {}", pattern),
            };
        }

        ToolOutcome::GrepMatches {
            path: search_path,
            matches: results,
            total,
        }
    }
}

fn find_matches(content: &str, pattern: &str, _file_path: &std::path::Path, context: usize) -> Vec<SearchMatch> {
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

fn is_binary(path: &std::path::Path) -> bool {
    let binary_extensions = [
        "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp",
        "ttf", "otf", "woff", "woff2", "eot",
        "mp3", "mp4", "avi", "mov", "mkv", "webm",
        "zip", "tar", "gz", "bz2", "xz", "zst",
        "pdf", "doc", "docx", "xls", "xlsx",
        "wasm", "o", "so", "dylib", "exe", "dll",
        "pyc", "class",
    ];

    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| binary_extensions.contains(&e))
        .unwrap_or(false)
}