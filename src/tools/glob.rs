use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;
use globset::{Glob as GlobPattern, GlobSet, GlobSetBuilder};
use std::path::PathBuf;

pub struct Glob;

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
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'pattern' argument".into(),
                }
            }
        };

        let base_dir = args.get("base_dir").and_then(|b| b.as_str()).unwrap_or(".");

        let base_path = PathBuf::from(shellexpand::tilde(base_dir).as_ref());

        if !base_path.is_dir() {
            return ToolOutcome::Error {
                message: format!("Base directory not found: {}", base_path.display()),
            };
        }

        // Build glob set
        let mut builder = GlobSetBuilder::new();
        match GlobPattern::new(&pattern) {
            Ok(g) => {
                builder.add(g);
            }
            Err(e) => {
                return ToolOutcome::Error {
                    message: format!("Invalid glob pattern '{}': {}", pattern, e),
                }
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

            // Relative path matching
            let rel = entry_path.strip_prefix(&base_path).unwrap_or(entry_path);
            if glob_set.is_match(rel) {
                matches.push(rel.to_string_lossy().to_string());
            }
        }

        matches.sort();

        if matches.is_empty() {
            return ToolOutcome::Success {
                content: format!("No files matching '{}' in {}", pattern, base_path.display()),
            };
        }

        let output = matches.join("\n");
        ToolOutcome::Success {
            content: format!(
                "Found {} files matching '{}':\n{}",
                matches.len(),
                pattern,
                output
            ),
        }
    }
}
