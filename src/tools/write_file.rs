use crate::session::undo::UndoKind;
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::{Tool, UndoStackRef};
use std::path::PathBuf;

/// Write content to a file, creating or overwriting it entirely.
///
/// Holds an `Option<UndoStackRef>` so the user can `/undo` the
/// write. Snapshot strategy: read the existing file (if any) BEFORE
/// the destructive write, push it onto the stack. On undo, restore
/// the original bytes — or remove the file if it didn't exist.
///
/// Review.md gap #7: the undo stack is the safety net that makes
/// users trust an AI agent with their code. `write_file` is the
/// most destructive tool in the suite (it can clobber an entire
/// file with one call), so it's the one the user is most likely
/// to want to undo.
pub struct WriteFile {
    undo: Option<UndoStackRef>,
}

impl WriteFile {
    pub fn new(undo: Option<UndoStackRef>) -> Self {
        Self { undo }
    }
}

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

        // Snapshot pre-write bytes BEFORE the destructive write.
        // For a new file (`prev_existed = false`) the bytes are
        // empty but the op is still recorded so `/undo` knows to
        // remove the file.
        let prev_bytes = std::fs::read(&path).unwrap_or_default();
        let prev_existed = std::fs::metadata(&path).is_ok();

        match std::fs::write(&path, &content) {
            Ok(_) => {
                snapshot_for_undo(&self.undo, &path, prev_existed, &prev_bytes);
                ToolOutcome::Success {
                    content: format!("Wrote {} bytes to {}", content.len(), path.display()),
                }
            }
            Err(e) => ToolOutcome::Error {
                message: format!("Cannot write {}: {}", path.display(), e),
            },
        }
    }
}

/// Push a snapshot onto the undo stack. On failure, log a warning
/// and continue — the write still succeeded; it just won't be
/// undoable. Same pattern as `edit_file::snapshot_for_undo`.
fn snapshot_for_undo(
    undo: &Option<UndoStackRef>,
    path: &std::path::Path,
    prev_existed: bool,
    prev_bytes: &[u8],
) {
    if let Some(stack) = undo {
        match stack.lock() {
            Ok(mut s) => {
                if let Err(e) = s.push(UndoKind::Write, path, prev_existed, prev_bytes) {
                    tracing::warn!(
                        path = %path.display(),
                        error = ?e,
                        "write succeeded but undo snapshot failed — write will not be undoable"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "undo stack mutex poisoned; write will not be undoable"
                );
            }
        }
    }
}
