use crate::session::access::PathGuard;
use crate::session::undo::UndoKind;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext, UndoStackRef};
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
    path_guard: PathGuard,
}

impl WriteFile {
    pub fn new(undo: Option<UndoStackRef>, path_guard: PathGuard) -> Self {
        Self { undo, path_guard }
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

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(shellexpand::tilde(p).as_ref()),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'path' argument"));
            }
        };

        // Enforce deny_paths, deny_extensions, block_dotfiles,
        // allowed_write_dirs, and sandbox containment before any write.
        if let crate::session::access::GuardVerdict::Denied(msg) =
            self.path_guard.check_write(&path).await
        {
            return ToolOutcome::Failure(ToolError::AccessDenied { message: msg });
        }

        let content = match args.get("content").and_then(|c| c.as_str()) {
            Some(c) => c.to_string(),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'content' argument"));
            }
        };

        if ctx.dry_run {
            return ToolOutcome::Success {
                content: format!(
                    "Dry run: would write {} bytes to {}",
                    content.len(),
                    path.display()
                ),
            };
        }

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!("Cannot create directory {}: {}", parent.display(), e),
                    });
                }
            }
        }

        // Snapshot pre-write bytes BEFORE the destructive write.
        // For a new file (`prev_existed = false`) the bytes are
        // empty but the op is still recorded so `/undo` knows to
        // remove the file.
        let prev_existed = std::fs::metadata(&path).is_ok();
        let prev_bytes = if prev_existed {
            match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!(
                            "Cannot read existing file {} for undo snapshot: {}. \
                             Refusing to overwrite without a snapshot.",
                            path.display(),
                            e
                        ),
                    });
                }
            }
        } else {
            Vec::new()
        };

        match crate::tools::atomic_write::atomic_write(&path, &content) {
            Ok(_) => match snapshot_for_undo(&self.undo, &path, prev_existed, &prev_bytes) {
                Ok(()) => ToolOutcome::Success {
                    content: format!("Wrote {} bytes to {}", content.len(), path.display()),
                },
                Err(e) => ToolOutcome::Failure(ToolError::Internal {
                    message: format!(
                        "Wrote {path}, but undo snapshot failed: {e}. \
                         The file was written; use git to revert if needed.",
                        path = path.display()
                    ),
                }),
            },
            Err(e) => ToolOutcome::Failure(ToolError::Internal {
                message: format!("Cannot write {}: {}", path.display(), e),
            }),
        }
    }
}

/// Push a snapshot onto the undo stack. Returns an error so the caller
/// can surface the failure to the user instead of silently leaving the
/// write un-undoable. Same pattern as `edit_file::snapshot_for_undo`.
fn snapshot_for_undo(
    undo: &Option<UndoStackRef>,
    path: &std::path::Path,
    prev_existed: bool,
    prev_bytes: &[u8],
) -> anyhow::Result<()> {
    let Some(stack) = undo else {
        return Ok(());
    };
    match stack.lock() {
        Ok(mut s) => {
            s.push(UndoKind::Write, path, prev_existed, prev_bytes)?;
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!(
            "undo stack mutex poisoned: {e}; write will not be undoable"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolContext};

    fn args(path: &str, content: &str) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "content": content,
        })
    }

    #[tokio::test]
    async fn write_file_dry_run_does_not_create_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dry_run.txt");

        let tool = WriteFile::new(None, crate::session::access::PathGuard::default());
        let ctx = ToolContext::with_dry_run(true);
        let out = tool
            .run(&ctx, args(&path.display().to_string(), "hello"))
            .await;

        assert!(
            matches!(out, ToolOutcome::Success { ref content } if content.contains("Dry run") && content.contains("would write"))
        );
        assert!(!path.exists(), "dry-run must not create the file");
    }

    #[tokio::test]
    async fn write_file_dry_run_does_not_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.txt");
        std::fs::write(&path, "original").unwrap();

        let tool = WriteFile::new(None, crate::session::access::PathGuard::default());
        let ctx = ToolContext::with_dry_run(true);
        let out = tool
            .run(&ctx, args(&path.display().to_string(), "new content"))
            .await;

        assert!(
            matches!(out, ToolOutcome::Success { ref content } if content.contains("Dry run") && content.contains("would write"))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }

    /// `write_file` must respect the PathGuard extension deny list.
    #[tokio::test]
    async fn write_file_respects_path_guard_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.pem");

        let guard = crate::session::access::PathGuard {
            deny_extensions: vec![".pem".to_string()],
            deny_list: crate::session::access::DenyList::new(vec![], vec![]),
            ..Default::default()
        };
        let tool = WriteFile::new(None, guard);
        let ctx = ToolContext::new();
        let out = tool
            .run(
                &ctx,
                args(&path.display().to_string(), "-----BEGIN PRIVATE KEY-----\n"),
            )
            .await;
        assert!(
            matches!(
                out,
                ToolOutcome::Failure(ToolError::AccessDenied { ref message }) if message.contains("Extension '.pem' denied")
            ),
            "expected extension denial, got {out:?}"
        );
        assert!(!path.exists(), "denied write_file must not create the file");
    }

    /// `write_file` must block overwriting an existing file larger than
    /// `max_overwrite_size`.
    #[tokio::test]
    async fn write_file_blocks_large_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let big = "x".repeat(2048);
        std::fs::write(&path, &big).unwrap();

        let guard = crate::session::access::PathGuard {
            max_overwrite_size: 1024,
            ..Default::default()
        };
        let tool = WriteFile::new(None, guard);
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, args(&path.display().to_string(), "small"))
            .await;
        assert!(
            matches!(
                out,
                ToolOutcome::Failure(ToolError::AccessDenied { ref message })
                    if message.contains("Refusing to overwrite")
            ),
            "expected large-file denial, got {out:?}"
        );
    }

    /// `write_file` must use atomic temp+rename so a failure leaves the
    /// original file intact. We make the parent read-only to force the temp
    /// write to fail.
    #[tokio::test]
    async fn write_file_atomic_failure_preserves_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro").join("file.txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "original").unwrap();
        let mut perms = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(path.parent().unwrap(), perms.clone()).unwrap();

        let tool = WriteFile::new(None, crate::session::access::PathGuard::default());
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, args(&path.display().to_string(), "new content"))
            .await;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
        }
        #[cfg(not(unix))]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(path.parent().unwrap(), perms);
        assert!(
            matches!(out, ToolOutcome::Failure(ToolError::Internal { .. })),
            "expected failure, got {out:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "original",
            "original file should be preserved"
        );
    }
}
