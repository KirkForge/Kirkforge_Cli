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
