use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::path::PathBuf;

pub struct ReadFile;

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file",
            description: "Read the contents of a file. Use offset and limit to read specific sections. Set minify=true to strip comments and collapse whitespace (saves ~30-50% tokens for source files).",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file (relative to project root or absolute)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (0-indexed)",
                        "default": 0
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read",
                        "default": 200
                    },
                    "minify": {
                        "type": "boolean",
                        "description": "Strip comments and collapse whitespace to save tokens (supports .rs, .py, .js, .ts, .go, .md)",
                        "default": false
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(shellexpand::tilde(p).as_ref()),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing 'path' argument",
                ));
            }
        };

        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as usize;
        let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(200) as usize;

        let raw_content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("Cannot read {}: {}", path.display(), e),
                });
            }
        };

        // Apply minification before slicing if requested
        let minify = args
            .get("minify")
            .and_then(|m| m.as_bool())
            .unwrap_or(false);
        let content = if minify {
            crate::shared::minify::minify_source(&path, &raw_content)
        } else {
            raw_content.clone()
        };

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        if offset >= total && total > 0 {
            return ToolOutcome::Failure(ToolError::Internal {
                message: format!("Offset {offset} is beyond file length {total}"),
            });
        }

        // File is empty after minification (e.g., all comments) — return a note
        if total == 0 {
            let note = if minify {
                format!(
                    "{} — file is empty after minification (was {} bytes of comments/whitespace)",
                    path.display(),
                    raw_content.len()
                )
            } else {
                format!("{} — empty file", path.display())
            };
            return ToolOutcome::Success { content: note };
        }

        let end = std::cmp::min(offset + limit, total);
        let selected = lines[offset..end].join("\n");
        let truncated = end < total;

        let display = if offset == 0 && end >= total {
            if minify {
                format!(
                    "{} (minified, was {} bytes → now {} bytes)\n{}",
                    path.display(),
                    raw_content.len(),
                    content.len(),
                    content,
                )
            } else {
                content
            }
        } else {
            let header = format!(
                "{}:{} (showing lines {}-{} of {})",
                path.display(),
                offset + 1,
                offset + 1,
                end,
                total
            );
            format!(
                "{header}\n{sep}\n{selected}",
                sep = "-".repeat(header.len())
            )
        };

        ToolOutcome::FileContent {
            path,
            content: display,
            truncated,
        }
    }
}
