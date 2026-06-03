use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;
use std::path::PathBuf;

pub struct ReadFile;

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file",
            description: "Read the contents of a file. Use offset and limit to read specific sections.",
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
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(shellexpand::tilde(p).as_ref()),
            None => return ToolOutcome::Error { message: "Missing 'path' argument".into() },
        };

        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(0) as usize;
        let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(200) as usize;

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutcome::Error {
                message: format!("Cannot read {}: {}", path.display(), e),
            },
        };

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        if offset >= total {
            return ToolOutcome::Error {
                message: format!("Offset {} is beyond file length {}", offset, total),
            };
        }

        let end = std::cmp::min(offset + limit, total);
        let selected = lines[offset..end].join("\n");
        let truncated = end < total;

        let display = if offset == 0 && end >= total {
            content.clone()
        } else {
            let header = format!("{}:{} (showing lines {}-{} of {})", path.display(), offset + 1, offset + 1, end, total);
            format!("{header}\n{sep}\n{selected}", sep = "-".repeat(header.len()))
        };

        ToolOutcome::FileContent {
            path,
            content: display,
            truncated,
        }
    }
}