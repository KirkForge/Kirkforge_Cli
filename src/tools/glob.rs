use crate::session::access::{GuardVerdict, PathGuard};
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use globset::{Glob as GlobPattern, GlobSet, GlobSetBuilder};
use std::path::PathBuf;

pub struct Glob {
    path_guard: PathGuard,
}

impl Glob {
    pub fn new(path_guard: PathGuard) -> Self {
        Self { path_guard }
    }
}

#[async_trait::async_trait]
impl Tool for Glob {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "glob",
            description: "List files matching a glob pattern. Uses gitignore-aware matching.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern (e.g., 'src/**/*.rs', '*.toml')"
                    },
                    "base_dir": {
                        "type": "string",
                        "description": "Base directory (default: current directory)",
                        "default": "."
                    },
                    "max_matches": {
                        "type": "integer",
                        "description": "Maximum number of files to return (default: 1000)",
                        "default": 1000
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

        let base_dir = args.get("base_dir").and_then(|b| b.as_str()).unwrap_or(".");
        let max_matches = args
            .get("max_matches")
            .and_then(|m| m.as_u64())
            .unwrap_or(1000) as usize;

        let base_path = PathBuf::from(shellexpand::tilde(base_dir).as_ref());

        if !base_path.is_dir() {
            return ToolOutcome::Failure(ToolError::Internal {
                message: format!("Base directory not found: {}", base_path.display()),
            });
        }

        // Build glob set
        let mut builder = GlobSetBuilder::new();
        match GlobPattern::new(&pattern) {
            Ok(g) => {
                builder.add(g);
            }
            Err(e) => {
                return ToolOutcome::Failure(ToolError::invalid_args(format!(
                    "Invalid glob pattern '{pattern}': {e}"
                )));
            }
        }
        let glob_set = builder.build().unwrap_or_else(|_| GlobSet::empty());

        let mut matches = Vec::new();

        let walker = ignore::WalkBuilder::new(&base_path)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .build();

        for entry in walker.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                continue;
            }

            // Per-file traversal guard: the walker may have followed a
            // symlink from inside the base_dir to outside it, or the file
            // may sit on a denied path. `check_traversal` is the lightweight
            // deny-list + symlink + sandbox check (no size/binary gate,
            // because we are only listing paths).
            if let GuardVerdict::Denied(_) = self.path_guard.check_traversal(entry_path) {
                continue;
            }

            // Relative path matching
            let rel = entry_path.strip_prefix(&base_path).unwrap_or(entry_path);
            if glob_set.is_match(rel) {
                matches.push(rel.to_string_lossy().to_string());
            }
        }

        matches.sort();
        let total = matches.len();
        let truncated = matches.len() > max_matches;
        matches.truncate(max_matches);

        if matches.is_empty() {
            return ToolOutcome::Success {
                content: format!("No files matching '{}' in {}", pattern, base_path.display()),
            };
        }

        let output = matches.join("\n");
        let header = if truncated {
            format!("Found {total} files matching '{pattern}'; showing first {max_matches}:")
        } else {
            format!("Found {total} files matching '{pattern}':")
        };
        ToolOutcome::Success {
            content: format!("{header}\n{output}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;

    #[tokio::test]
    async fn glob_respects_max_matches_and_reports_total() {
        let dir = std::env::temp_dir().join("kirkforge_glob_cap_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..5 {
            std::fs::write(dir.join(format!("file{i}.txt")), "x").unwrap();
        }

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "*.txt",
            "base_dir": dir.to_string_lossy(),
            "max_matches": 2
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("Found 5 files matching '*.txt'; showing first 2:"),
                    "expected truncation header, got: {content}"
                );
                // Two filenames should appear, not all five.
                let lines: Vec<_> = content.lines().skip(1).collect();
                assert_eq!(
                    lines.len(),
                    2,
                    "output should contain exactly max_matches files"
                );
            }
            other => panic!("expected Success, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_no_truncation_when_under_cap() {
        let dir = std::env::temp_dir().join("kirkforge_glob_under_cap_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "*.txt",
            "base_dir": dir.to_string_lossy(),
            "max_matches": 10
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("Found 1 files matching '*.txt':"),
                    "expected non-truncation header, got: {content}"
                );
                assert!(!content.contains("showing first"));
            }
            other => panic!("expected Success, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
