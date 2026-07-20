use crate::session::access::PathGuard;
use crate::session::undo::UndoKind;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{atomic_write, Tool, ToolContext, UndoStackRef};
use std::path::PathBuf;

/// Edit a single cell in a Jupyter notebook (.ipynb).
///
/// Parses the notebook JSON, replaces the source of the cell at `index`,
/// and writes the result back atomically. The pre-edit bytes are snapshotted
/// for `/undo` when an undo stack is available.
pub struct NotebookEdit {
    undo: Option<UndoStackRef>,
    path_guard: PathGuard,
}

impl NotebookEdit {
    pub fn new(undo: Option<UndoStackRef>, path_guard: PathGuard) -> Self {
        Self { undo, path_guard }
    }
}

#[async_trait::async_trait]
impl Tool for NotebookEdit {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "notebook_edit",
            description: "Edit a single cell in a Jupyter notebook (.ipynb). Replaces the source of the cell at the given zero-based index and writes the notebook back atomically.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the .ipynb file"
                    },
                    "index": {
                        "type": "integer",
                        "description": "Zero-based index of the cell to edit"
                    },
                    "source": {
                        "type": "string",
                        "description": "New cell source (string or multi-line string)"
                    }
                },
                "required": ["path", "index", "source"]
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

        if let crate::session::access::GuardVerdict::Denied(msg) =
            self.path_guard.check_write(&path).await
        {
            return ToolOutcome::Failure(ToolError::AccessDenied { message: msg });
        }

        let index = match args.get("index").and_then(|i| i.as_u64()) {
            Some(i) => i as usize,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "'index' must be a non-negative integer",
                ));
            }
        };

        let new_source = match args.get("source") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(arr)) => {
                let mut lines = Vec::with_capacity(arr.len());
                for v in arr {
                    match v.as_str() {
                        Some(s) => lines.push(s.to_string()),
                        None => {
                            return ToolOutcome::Failure(ToolError::invalid_args(
                                "'source' array must contain only strings",
                            ));
                        }
                    }
                }
                lines
            }
            _ => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing or invalid 'source' argument; expected string or array of strings",
                ));
            }
        };

        if ctx.dry_run {
            return ToolOutcome::Success {
                content: format!(
                    "Dry run: would edit cell {index} of {} with {} source line(s).",
                    path.display(),
                    new_source.len()
                ),
            };
        }

        let old_bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("failed to read notebook: {e}"),
                });
            }
        };

        let mut notebook: serde_json::Value = match serde_json::from_slice(&old_bytes) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("notebook is not valid JSON: {e}"),
                });
            }
        };

        let cells = match notebook.get_mut("cells").and_then(|c| c.as_array_mut()) {
            Some(c) => c,
            None => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: "notebook has no 'cells' array".to_string(),
                });
            }
        };

        if index >= cells.len() {
            return ToolOutcome::Failure(ToolError::Internal {
                message: format!(
                    "cell index {index} out of range (notebook has {} cells)",
                    cells.len()
                ),
            });
        }

        let cell = &mut cells[index];
        if let Some(existing) = cell.get_mut("source") {
            *existing = serde_json::Value::Array(
                new_source
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            );
        } else {
            cell.as_object_mut().unwrap().insert(
                "source".to_string(),
                serde_json::Value::Array(
                    new_source
                        .iter()
                        .map(|s| serde_json::Value::String(s.clone()))
                        .collect(),
                ),
            );
        }

        let new_bytes = match serde_json::to_vec_pretty(&notebook) {
            Ok(b) => b,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("failed to serialize notebook: {e}"),
                });
            }
        };

        if let Some(undo) = &self.undo {
            if let Err(e) = undo
                .lock()
                .unwrap()
                .push(UndoKind::Write, &path, true, &old_bytes)
            {
                tracing::warn!("failed to push notebook edit snapshot: {e}");
            }
        }

        if let Err(e) = atomic_write::atomic_write(&path, &new_bytes) {
            return ToolOutcome::Failure(ToolError::Internal {
                message: format!("failed to write notebook: {e}"),
            });
        }

        ToolOutcome::Success {
            content: format!("Updated cell {index} in {}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> PathGuard {
        PathGuard::default()
    }

    fn make_notebook() -> (tempfile::TempDir, PathBuf, serde_json::Value) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ipynb");
        let notebook = serde_json::json!({
            "cells": [
                {
                    "cell_type": "code",
                    "execution_count": 1,
                    "metadata": {},
                    "outputs": [],
                    "source": ["print('hello')\n"]
                },
                {
                    "cell_type": "markdown",
                    "metadata": {},
                    "source": ["# heading\n"]
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&notebook).unwrap()).unwrap();
        (dir, path, notebook)
    }

    #[tokio::test]
    async fn edits_code_cell_by_index() {
        let (_dir, path, _nb) = make_notebook();
        let tool = NotebookEdit::new(None, guard());
        let outcome = tool
            .run(
                &ToolContext::default(),
                serde_json::json!({
                    "path": path,
                    "index": 0,
                    "source": "print('world')\n"
                }),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Success { .. }));
        let updated: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        let source = updated["cells"][0]["source"].as_array().unwrap();
        assert_eq!(source.len(), 1);
        assert_eq!(source[0].as_str().unwrap(), "print('world')\n");
    }

    #[tokio::test]
    async fn edits_markdown_cell_by_index() {
        let (_dir, path, _nb) = make_notebook();
        let tool = NotebookEdit::new(None, guard());
        let outcome = tool
            .run(
                &ToolContext::default(),
                serde_json::json!({
                    "path": path,
                    "index": 1,
                    "source": "## subheading\n"
                }),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Success { .. }));
        let updated: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        let source = updated["cells"][1]["source"].as_array().unwrap();
        assert_eq!(source[0].as_str().unwrap(), "## subheading\n");
    }

    #[tokio::test]
    async fn rejects_out_of_range_index() {
        let (_dir, path, _nb) = make_notebook();
        let tool = NotebookEdit::new(None, guard());
        let outcome = tool
            .run(
                &ToolContext::default(),
                serde_json::json!({
                    "path": path,
                    "index": 99,
                    "source": "x"
                }),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Failure(_)));
    }

    #[tokio::test]
    async fn accepts_source_as_array() {
        let (_dir, path, _nb) = make_notebook();
        let tool = NotebookEdit::new(None, guard());
        let outcome = tool
            .run(
                &ToolContext::default(),
                serde_json::json!({
                    "path": path,
                    "index": 0,
                    "source": ["a = 1", "print(a)"]
                }),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Success { .. }));
        let updated: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        let source = updated["cells"][0]["source"].as_array().unwrap();
        assert_eq!(source.len(), 2);
    }

    #[tokio::test]
    async fn dry_run_does_not_modify() {
        let (_dir, path, _nb) = make_notebook();
        let tool = NotebookEdit::new(None, guard());
        let outcome = tool
            .run(
                &ToolContext::with_dry_run(true),
                serde_json::json!({
                    "path": path,
                    "index": 0,
                    "source": "should not apply"
                }),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Success { .. }));
        let updated: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(
            updated["cells"][0]["source"][0].as_str().unwrap(),
            "print('hello')\n"
        );
    }
}
