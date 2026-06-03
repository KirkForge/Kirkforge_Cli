use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;
use std::path::PathBuf;

pub struct WriteFile;

#[async_trait::async_trait]
impl Tool for WriteFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write_file",
            description: "Write content to a file, creating or overwriting it entirely. Prefer edit_file for small changes.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "Full content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(shellexpand::tilde(p).as_ref()),
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'path' argument".into(),
                }
            }
        };

        let content = match args.get("content").and_then(|c| c.as_str()) {
            Some(c) => c.to_string(),
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'content' argument".into(),
                }
            }
        };

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolOutcome::Error {
                        message: format!("Cannot create directory {}: {}", parent.display(), e),
                    };
                }
            }
        }

        match std::fs::write(&path, &content) {
            Ok(_) => ToolOutcome::Success {
                content: format!("Wrote {} bytes to {}", content.len(), path.display()),
            },
            Err(e) => ToolOutcome::Error {
                message: format!("Cannot write {}: {}", path.display(), e),
            },
        }
    }
}
