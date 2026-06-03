use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;
use similar::{ChangeTag, TextDiff};
use std::path::PathBuf;

pub struct EditFile;

#[async_trait::async_trait]
impl Tool for EditFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "edit_file",
            description: "Edit a file by finding an exact string match and replacing it. Shows a diff of the change. Prefer over write_file for targeted changes.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Exact string to find (must match exactly; try surrounding context if uncertain)"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement string"
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(shellexpand::tilde(p).as_ref()),
            None => return ToolOutcome::Error { message: "Missing 'path' argument".into() },
        };

        let old = match args.get("old_string").and_then(|o| o.as_str()) {
            Some(o) => o.to_string(),
            None => return ToolOutcome::Error { message: "Missing 'old_string' argument".into() },
        };

        let new = match args.get("new_string").and_then(|n| n.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolOutcome::Error { message: "Missing 'new_string' argument".into() },
        };

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutcome::Error {
                message: format!("Cannot read {}: {}", path.display(), e),
            },
        };

        if !content.contains(&old) {
            // Try fuzzy match: strip trailing whitespace per line, re-check
            let normalized: String = content.lines()
                .map(|l| l.trim_end())
                .collect::<Vec<_>>()
                .join("\n");
            let old_normalized: String = old.lines()
                .map(|l| l.trim_end())
                .collect::<Vec<_>>()
                .join("\n");

            if normalized.contains(&old_normalized) {
                // Found fuzzy match — use normalized content but warn
                let new_content = normalized.replacen(&old_normalized, &new, 1);
                let diff = render_diff(&content, &new_content);

                return match std::fs::write(&path, &new_content) {
                    Ok(_) => ToolOutcome::FileEdit { path, diff },
                    Err(e) => ToolOutcome::Error {
                        message: format!("Cannot write {}: {}", path.display(), e),
                    },
                };
            }

            // Still not found — find nearby context to help the model
            let context_lines: Vec<&str> = content.lines().take(10).collect();
            let preview = context_lines.join("\n");
            return ToolOutcome::Error {
                message: format!(
                    "String not found in {}. First {} lines:\n{}",
                    path.display(),
                    context_lines.len(),
                    preview
                ),
            };
        }

        let new_content = content.replacen(&old, &new, 1);
        let diff = render_diff(&content, &new_content);

        match std::fs::write(&path, &new_content) {
            Ok(_) => ToolOutcome::FileEdit { path, diff },
            Err(e) => ToolOutcome::Error {
                message: format!("Cannot write {}: {}", path.display(), e),
            },
        }
    }
}

fn render_diff(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();

    for change in diff.iter_all_changes() {
        let marker = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        out.push_str(&format!("{}{}", marker, change.value()));
    }

    out
}