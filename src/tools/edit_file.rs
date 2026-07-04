use crate::session::undo::UndoKind;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext, UndoStackRef};
use similar::{ChangeTag, TextDiff};
use std::path::PathBuf;

/// Edit file by exact-string match with fuzzy fallback.
///
/// Holds an `Option<UndoStackRef>` for the session's undo stack.
/// When set, the tool snapshots the pre-edit bytes before writing
/// the new content, so the user can `/undo` to revert.
///
/// Review.md gap #7: the undo stack is the safety net that makes
/// users trust an AI agent with their code. Without it, the only
/// recourse on a bad edit is `git checkout` — fine for git repos,
/// useless for untracked files.
pub struct EditFile {
    undo: Option<UndoStackRef>,
}

impl EditFile {
    pub fn new(undo: Option<UndoStackRef>) -> Self {
        Self { undo }
    }
}

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

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(shellexpand::tilde(p).as_ref()),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'path' argument"));
            }
        };

        let old = match args.get("old_string").and_then(|o| o.as_str()) {
            Some(o) => o.to_string(),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing 'old_string' argument",
                ));
            }
        };

        let new = match args.get("new_string").and_then(|n| n.as_str()) {
            Some(n) => n.to_string(),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing 'new_string' argument",
                ));
            }
        };

        // Snapshot pre-edit bytes BEFORE the destructive write, so
        // the user can `/undo` even if the write succeeds. We
        // capture the file as it exists *now* (which is the
        // pre-edit state) — including the trailing-newline,
        // CRLF/LF, encoding. Same byte-for-byte restoration on
        // `/undo`.
        let prev_existed = std::fs::metadata(&path).is_ok();
        let prev_bytes = if prev_existed {
            match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!(
                            "Cannot read existing file {} for undo snapshot: {}. \
                             Refusing to edit without a snapshot.",
                            path.display(),
                            e
                        ),
                    });
                }
            }
        } else {
            Vec::new()
        };

        if ctx.dry_run {
            let content = match String::from_utf8(prev_bytes.clone()) {
                Ok(c) => c,
                Err(_) => {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!(
                            "{} is not valid UTF-8; cannot edit_file (use bash for binary content)",
                            path.display()
                        ),
                    });
                }
            };
            if !content.contains(&old) {
                return ToolOutcome::Failure(ToolError::Execution {
                    message: format!("Dry run: string not found in {}", path.display()),
                    exit_code: None,
                    stderr: String::new(),
                });
            }
            let occurrences = content.matches(&old).count();
            if occurrences > 1 {
                return ToolOutcome::Failure(ToolError::Execution {
                    message: format!(
                        "Dry run: old_string matches {} times in {}; edit_file requires a unique match",
                        occurrences,
                        path.display()
                    ),
                    exit_code: None,
                    stderr: String::new(),
                });
            }
            let new_content = content.replacen(&old, &new, 1);
            let diff = render_diff(&content, &new_content);
            return ToolOutcome::Success {
                content: format!("Dry run: would edit {}:\n{}", path.display(), diff),
            };
        }

        let content = match String::from_utf8(prev_bytes.clone()) {
            Ok(c) => c,
            Err(_) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!(
                        "{} is not valid UTF-8; cannot edit_file (use bash for binary content)",
                        path.display()
                    ),
                });
            }
        };

        if !content.contains(&old) {
            // Try fuzzy match: strip trailing whitespace per line, re-check
            let normalized: String = content
                .lines()
                .map(|l| l.trim_end())
                .collect::<Vec<_>>()
                .join("\n");
            let old_normalized: String = old
                .lines()
                .map(|l| l.trim_end())
                .collect::<Vec<_>>()
                .join("\n");

            if normalized.contains(&old_normalized) {
                // Ambiguous fuzzy match guard: if the normalized old_string
                // appears more than once, we cannot safely choose one.
                let fuzzy_occurrences = normalized.matches(&old_normalized).count();
                if fuzzy_occurrences > 1 {
                    return ToolOutcome::Failure(ToolError::Execution {
                        message: format!(
                            "old_string matches {} times in {} after whitespace normalization; \
                             edit_file requires a unique match",
                            fuzzy_occurrences,
                            path.display()
                        ),
                        exit_code: None,
                        stderr: String::new(),
                    });
                }

                // Found fuzzy match — find the span in the normalized content,
                // then apply the replacement to the ORIGINAL content instead
                // of writing the whole normalized file.
                //
                // Strategy: compute LCS-style bounds. Walk normalized to find
                // the old_normalized span, map line ranges back to original.
                let n_lines: Vec<&str> = normalized.lines().collect();
                let o_lines: Vec<&str> = old_normalized.lines().collect();

                // Find the first line index where normalized starts matching old_normalized
                let n_start = n_lines
                    .windows(o_lines.len())
                    .position(|w| w == o_lines.as_slice());

                if let Some(n_start) = n_start {
                    // Compute byte offset in the ORIGINAL content using the
                    // actual `\n` positions, not `str::lines()`. The old
                    // approach used `content.lines()`, which strips the
                    // trailing `\r` from `\r\n` line endings, so on CRLF
                    // content every line's offset was undercounted by 1 and
                    // the replacement landed one byte early — corrupting
                    // the file. This was the source of deepseek-v4's review
                    // finding on edit_file's fuzzy fallback.
                    let newline_positions: Vec<usize> =
                        content.match_indices('\n').map(|(i, _)| i).collect();

                    // byte offset of the start of `line_idx` in `content`
                    let line_byte_start = |line_idx: usize| -> usize {
                        if line_idx == 0 || newline_positions.is_empty() {
                            0
                        } else if line_idx - 1 < newline_positions.len() {
                            newline_positions[line_idx - 1] + 1
                        } else {
                            content.len()
                        }
                    };

                    // byte offset just past the end of `line_idx` (i.e.,
                    // after the `\n`)
                    let line_byte_end = |line_idx: usize| -> usize {
                        if line_idx < newline_positions.len() {
                            newline_positions[line_idx] + 1
                        } else {
                            content.len()
                        }
                    };

                    let byte_start = line_byte_start(n_start);
                    let byte_end = line_byte_end(n_start + o_lines.len() - 1);
                    let span_orig_len = byte_end - byte_start;

                    // Build the new content by replacing the span in original
                    let mut new_content = String::with_capacity(content.len() + new.len());
                    new_content.push_str(&content[..byte_start]);
                    new_content.push_str(&new);
                    new_content.push_str(&content[byte_start + span_orig_len..]);

                    let diff = render_diff(&content, &new_content);
                    return match std::fs::write(&path, &new_content) {
                        Ok(_) => {
                            match snapshot_for_undo(
                                &self.undo,
                                UndoKind::Edit,
                                &path,
                                prev_existed,
                                &prev_bytes,
                            ) {
                                Ok(()) => ToolOutcome::FileEdit { path, diff },
                                Err(e) => ToolOutcome::Failure(ToolError::Internal {
                                    message: format!(
                                        "Edited {path}, but undo snapshot failed: {e}. \
                                         The edit was applied; use git to revert if needed.",
                                        path = path.display()
                                    ),
                                }),
                            }
                        }
                        Err(e) => ToolOutcome::Failure(ToolError::Internal {
                            message: format!("Cannot write {}: {}", path.display(), e),
                        }),
                    };
                }
            }

            // Still not found — find nearby context to help the model
            let context_lines: Vec<&str> = content.lines().take(10).collect();
            let preview = context_lines.join("\n");
            return ToolOutcome::Failure(ToolError::Execution {
                message: format!(
                    "String not found in {}. First {} lines:\n{}",
                    path.display(),
                    context_lines.len(),
                    preview
                ),
                exit_code: None,
                stderr: String::new(),
            });
        }

        // Ambiguous exact match guard: if old_string appears more than
        // once, replacing the first occurrence silently would be
        // surprising. Force the model to include more context.
        let occurrences = content.matches(&old).count();
        if occurrences > 1 {
            return ToolOutcome::Failure(ToolError::Execution {
                message: format!(
                    "old_string matches {} times in {}; edit_file requires a unique match",
                    occurrences,
                    path.display()
                ),
                exit_code: None,
                stderr: String::new(),
            });
        }

        let new_content = content.replacen(&old, &new, 1);
        let diff = render_diff(&content, &new_content);

        match std::fs::write(&path, &new_content) {
            Ok(_) => match snapshot_for_undo(
                &self.undo,
                UndoKind::Edit,
                &path,
                prev_existed,
                &prev_bytes,
            ) {
                Ok(()) => ToolOutcome::FileEdit { path, diff },
                Err(e) => ToolOutcome::Failure(ToolError::Internal {
                    message: format!(
                        "Edited {path}, but undo snapshot failed: {e}. \
                         The edit was applied; use git to revert if needed.",
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

/// Push a snapshot onto the undo stack, if one was supplied.
/// Returns an error so the caller can surface the failure to the user
/// instead of silently leaving the edit un-undoable.
fn snapshot_for_undo(
    undo: &Option<UndoStackRef>,
    kind: UndoKind,
    path: &std::path::Path,
    prev_existed: bool,
    prev_bytes: &[u8],
) -> anyhow::Result<()> {
    let Some(stack) = undo else {
        return Ok(());
    };
    match stack.lock() {
        Ok(mut s) => {
            s.push(kind, path, prev_existed, prev_bytes)?;
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!(
            "undo stack mutex poisoned: {e}; edit will not be undoable"
        )),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::remove_test_file;
    use crate::tools::ToolContext;

    #[tokio::test]
    async fn test_fuzzy_fallback_preserves_original_formatting() {
        // Content with trailing whitespace on some lines
        let content = "fn main() {\n    let x = 1;    \n    println!(\"hello\");\n}\n";
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_fuzzy.txt");
        std::fs::write(&path, content).unwrap();

        let tool = EditFile::new(None);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1;",
            "new_string": "let y = 2;",
        });

        let result = tool.run(&ctx, args).await;
        match result {
            ToolOutcome::FileEdit { path: _, diff: _ } => {
                let result_content = std::fs::read_to_string(&path).unwrap();
                // The trailing whitespace on line 2 should be preserved
                assert!(
                    result_content.contains("    let y = 2;    "),
                    "Fuzzy fallback should preserve original trailing whitespace, got: {result_content:?}"
                );
            }
            other => panic!("Expected FileEdit, got {other:?}"),
        }

        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_fuzzy_fallback_preserves_crlf_line_endings() {
        // Content with CRLF (Windows-style) line endings. The old
        // fuzzy-fallback byte-offset math used `str::lines()` which
        // strips the trailing `\r`, so on CRLF content every line's
        // offset was undercounted by 1 and the replacement landed one
        // byte early — corrupting the file. This is the regression
        // test for deepseek-v4's review finding.
        let content = "fn main() {\r\n    let x = 1;\r\n    println!(\"hello\");\r\n}\r\n";
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_fuzzy_crlf.txt");
        std::fs::write(&path, content).unwrap();

        let tool = EditFile::new(None);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1;",
            "new_string": "let y = 2;",
        });

        let result = tool.run(&ctx, args).await;
        match result {
            ToolOutcome::FileEdit { path: _, diff: _ } => {
                let result_content = std::fs::read_to_string(&path).unwrap();
                // Every line ending should still be CRLF, and the
                // replacement should have landed at exactly the line
                // boundary (not in the middle of a `\r\n`).
                assert!(
                    result_content.contains("    let y = 2;\r\n"),
                    "Replacement should land at a line boundary with CRLF preserved, got: {result_content:?}"
                );
                // The file should not have any orphaned `\r` characters
                // or missing newlines. Count the CRLFs in the original
                // and check the result has the same count.
                let original_crlf_count = content.matches("\r\n").count();
                let result_crlf_count = result_content.matches("\r\n").count();
                assert_eq!(
                    original_crlf_count, result_crlf_count,
                    "Number of CRLF line endings should be preserved (orig={original_crlf_count}, result={result_crlf_count})"
                );
            }
            other => panic!("Expected FileEdit, got {other:?}"),
        }

        remove_test_file(&path);
    }

    /// When the tool is constructed with an `UndoStackRef`, every
    /// successful edit must snapshot the pre-edit bytes so `/undo`
    /// can revert.
    #[tokio::test]
    async fn test_edit_file_snapshots_for_undo() {
        use crate::session::undo::{UndoKind, UndoStack};

        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_undo.txt");
        std::fs::write(&path, b"original content").unwrap();

        // Fresh stack with a unique session id.
        let id = format!(
            "test-edit-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let stack =
            std::sync::Arc::new(std::sync::Mutex::new(UndoStack::for_session(&id).unwrap()));

        let tool = EditFile::new(Some(stack.clone()));
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "original",
            "new_string": "modified",
        });
        let result = tool.run(&ctx, args).await;
        assert!(matches!(result, ToolOutcome::FileEdit { .. }));

        // Stack should have one Edit entry, and pop should restore
        // "original content".
        let list = stack.lock().unwrap().list();
        assert_eq!(list.len(), 1, "expected one undo entry, got {}", list.len());
        assert_eq!(list[0].kind, UndoKind::Edit);
        assert_eq!(list[0].path, path);

        let restored = stack.lock().unwrap().pop().unwrap().unwrap();
        assert_eq!(restored.path, path);
        assert!(restored.prev_existed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original content");
    }

    /// When old_string appears more than once, edit_file must refuse rather
    /// than silently replace the first occurrence.
    #[tokio::test]
    async fn test_edit_file_rejects_ambiguous_exact_match() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_ambiguous.txt");
        std::fs::write(&path, "fn foo() {}\nfn bar() {}\nfn foo() {}\n").unwrap();

        let tool = EditFile::new(None);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "fn foo() {}",
            "new_string": "fn baz() {}",
        });
        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::Failure(ToolError::Execution { ref message, .. }) if message.contains("matches 2 times")),
            "expected ambiguous-match error, got {result:?}"
        );
        remove_test_file(&path);
    }

    /// The fuzzy fallback must also reject ambiguous normalized matches.
    #[tokio::test]
    async fn test_edit_file_rejects_ambiguous_fuzzy_match() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_ambiguous_fuzzy.txt");
        // Same line twice with differing trailing whitespace.
        std::fs::write(&path, "let x = 1;    \nlet y = 2;\nlet x = 1;\n").unwrap();

        let tool = EditFile::new(None);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1;",
            "new_string": "let z = 3;",
        });
        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::Failure(ToolError::Execution { ref message, .. }) if message.contains("matches 2 times")),
            "expected ambiguous fuzzy-match error, got {result:?}"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_edit_file_dry_run_does_not_modify_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dry_run_edit.txt");
        std::fs::write(&path, "fn old() {}\n").unwrap();

        let tool = EditFile::new(None);
        let ctx = ToolContext::with_dry_run(true);
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "fn old() {}",
            "new_string": "fn new() {}",
        });
        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::Success { ref content } if content.contains("Dry run") && content.contains("would edit")),
            "expected dry-run success, got {result:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn old() {}\n");
    }
}
