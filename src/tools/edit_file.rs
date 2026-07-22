use crate::session::access::PathGuard;
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
    path_guard: PathGuard,
    minify_write_side: bool,
}

impl EditFile {
    pub fn new(undo: Option<UndoStackRef>, path_guard: PathGuard, minify_write_side: bool) -> Self {
        Self {
            undo,
            path_guard,
            minify_write_side,
        }
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

        // Path-guard check before any read or write. This enforces deny_paths,
        // deny_extensions, block_dotfiles, allowed_write_dirs, and sandbox
        // containment from a single source of truth in access.rs.
        if let crate::session::access::GuardVerdict::Denied(msg) =
            self.path_guard.check_write(&path).await
        {
            return ToolOutcome::Failure(ToolError::AccessDenied { message: msg });
        }

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

        // When write-side minification is enabled, expand any minified
        // envelopes so that the edit happens in readable source space. The
        // result is written expanded; this keeps the on-disk file canonical
        // and lets formatters enforce project style.
        let mut any_expanded = false;
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
        let (content, expanded_file) =
            if self.minify_write_side && crate::shared::minify::has_minified_envelope(&content) {
                any_expanded = true;
                (
                    crate::shared::minify::expand_minified(&path, &content),
                    true,
                )
            } else {
                (content, false)
            };
        let (old, old_expanded) =
            if self.minify_write_side && crate::shared::minify::has_minified_envelope(&old) {
                any_expanded = true;
                (crate::shared::minify::expand_minified(&path, &old), true)
            } else {
                (old, false)
            };
        let (new, new_expanded) =
            if self.minify_write_side && crate::shared::minify::has_minified_envelope(&new) {
                any_expanded = true;
                (crate::shared::minify::expand_minified(&path, &new), true)
            } else {
                (new, false)
            };
        let _ = (expanded_file, old_expanded, new_expanded); // used for future diagnostics

        if ctx.dry_run {
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
            let note = if any_expanded {
                " (expanded from minified envelope)"
            } else {
                ""
            };
            return ToolOutcome::Success {
                content: format!("Dry run{}: would edit {}:\n{}", note, path.display(), diff),
            };
        }

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

            if old_normalized.is_empty() {
                return ToolOutcome::Error {
                    message: format!(
                        "old_string is empty or whitespace-only in {}; \
                         edit_file requires a non-empty old_string",
                        path.display()
                    ),
                };
            }

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
                    return match crate::tools::atomic_write::atomic_write(&path, &new_content) {
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

        match crate::tools::atomic_write::atomic_write(&path, &new_content) {
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
    use proptest::prelude::*;

    #[tokio::test]
    async fn test_fuzzy_fallback_preserves_original_formatting() {
        // Content with trailing whitespace on some lines
        let content = "fn main() {\n    let x = 1;    \n    println!(\"hello\");\n}\n";
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_fuzzy.txt");
        std::fs::write(&path, content).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
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

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
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

    /// The fuzzy fallback must work when old_string only matches after
    /// whitespace normalization and the file uses CRLF line endings.
    /// This specifically exercises the byte-offset mapping fix: if the
    /// code uses `str::lines()` (which strips `\r`) to compute offsets,
    /// the replacement lands one byte early on every CRLF line.
    #[tokio::test]
    async fn test_fuzzy_fallback_crlf_via_whitespace_normalization() {
        // File has CRLF endings and the line to match ends with trailing
        // spaces. The old_string is supplied *without* trailing spaces, so
        // the only way to match is through fuzzy normalization.
        let content = "fn main() {\r\n    let x = 1;    \r\n    println!(\"hello\");\r\n}\r\n";
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_fuzzy_crlf_norm.txt");
        std::fs::write(&path, content).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            // No trailing spaces => exact match fails => fuzzy path.
            "old_string": "let x = 1;",
            "new_string": "let y = 2;",
        });

        let result = tool.run(&ctx, args).await;
        match result {
            ToolOutcome::FileEdit { path: _, diff: _ } => {
                let result_content = std::fs::read_to_string(&path).unwrap();
                assert!(
                    result_content.contains("    let y = 2;    \r\n"),
                    "Replacement should land at the normalized line boundary with CRLF and trailing spaces preserved, got: {result_content:?}"
                );
                assert_eq!(
                    content.matches("\r\n").count(),
                    result_content.matches("\r\n").count(),
                    "CRLF count must be preserved"
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

        let tool = EditFile::new(
            Some(stack.clone()),
            crate::session::access::PathGuard::default(),
            false,
        );
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

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
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

    /// Whitespace-only old_string must not reach `slice::windows(0)`.
    #[tokio::test]
    async fn test_edit_file_rejects_whitespace_only_old_string() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_whitespace_old.txt");
        // A single non-whitespace line so normalization is unique, plus an
        // empty line so the whitespace-only old_string can be found exactly
        // once after normalization.
        std::fs::write(&path, "fn main() {\n\n}\n").unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "   ",
            "new_string": "hello",
        });
        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::Error { ref message } if message.contains("whitespace-only")),
            "expected whitespace-only rejection, got {result:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The fuzzy fallback must also reject ambiguous normalized matches.
    #[tokio::test]
    async fn test_edit_file_rejects_ambiguous_fuzzy_match() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_ambiguous_fuzzy.txt");
        // Same line twice with differing trailing whitespace.
        std::fs::write(&path, "let x = 1;    \nlet y = 2;\nlet x = 1;\n").unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
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

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
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

    /// With minify_write_side enabled, a file wrapped in a minified envelope
    /// is expanded before matching, the edit is applied in readable source
    /// space, and the result is written expanded.
    #[tokio::test]
    async fn test_edit_file_expands_minified_envelope_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");

        let original_minified =
            crate::shared::minify::wrap_minified_envelope("rust", "fn main(){println!(\"hi\");}");
        let replacement_minified =
            crate::shared::minify::wrap_minified_envelope("rust", "fn main(){println!(\"bye\");}");

        std::fs::write(&path, &original_minified).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), true);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": original_minified,
            "new_string": replacement_minified,
        });
        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::FileEdit { .. }),
            "expected FileEdit, got {result:?}"
        );

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("<minified"),
            "result must be expanded, got: {written}"
        );
        assert!(
            written.contains("println!(\"bye\")"),
            "expanded result should contain new code, got: {written}"
        );
        assert!(
            !written.contains("println!(\"hi\")"),
            "old code should be gone, got: {written}"
        );
    }

    /// With minify_write_side disabled, minified envelopes are treated as
    /// literal text and written verbatim.
    #[tokio::test]
    async fn test_edit_file_preserves_envelope_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");

        let original_minified =
            crate::shared::minify::wrap_minified_envelope("rust", "fn main(){println!(\"hi\");}");
        let replacement_minified =
            crate::shared::minify::wrap_minified_envelope("rust", "fn main(){println!(\"bye\");}");

        std::fs::write(&path, &original_minified).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": original_minified,
            "new_string": replacement_minified,
        });
        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::FileEdit { .. }),
            "expected FileEdit, got {result:?}"
        );

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("<minified"),
            "envelope should be preserved verbatim, got: {written}"
        );
    }

    /// Dry run with minify_write_side enabled should report that the edit
    /// happens after expanding the minified envelope.
    #[tokio::test]
    async fn test_edit_file_dry_run_reports_expanded_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.rs");

        let original_minified =
            crate::shared::minify::wrap_minified_envelope("rust", "fn main(){println!(\"hi\");}");
        let replacement_minified =
            crate::shared::minify::wrap_minified_envelope("rust", "fn main(){println!(\"bye\");}");

        std::fs::write(&path, &original_minified).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), true);
        let ctx = ToolContext::with_dry_run(true);
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": original_minified,
            "new_string": replacement_minified,
        });
        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::Success { ref content } if content.contains("expanded from minified")),
            "expected dry-run expansion note, got {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original_minified,
            "dry run must not modify file"
        );
    }

    /// `edit_file` must reject paths blocked by the PathGuard, e.g. dotfiles
    /// when `block_dotfiles` is enabled.
    #[tokio::test]
    async fn test_edit_file_respects_path_guard_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".secret");
        std::fs::write(&path, "old").unwrap();

        let guard = crate::session::access::PathGuard {
            block_dotfiles: true,
            ..Default::default()
        };
        let tool = EditFile::new(None, guard, false);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "old",
            "new_string": "new",
        });
        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(
                result,
                ToolOutcome::Failure(ToolError::AccessDenied { ref message }) if message.contains("Dotfiles")
            ),
            "expected dotfile denial, got {result:?}"
        );
    }

    /// `edit_file` must reject overwriting an existing file larger than
    /// `max_overwrite_size`.
    #[tokio::test]
    async fn test_edit_file_blocks_large_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let big = "x".repeat(2048);
        std::fs::write(&path, &big).unwrap();

        let guard = crate::session::access::PathGuard {
            max_overwrite_size: 1024,
            ..Default::default()
        };
        let tool = EditFile::new(None, guard, false);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "xxxx",
            "new_string": "yyyy",
        });
        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(
                result,
                ToolOutcome::Failure(ToolError::AccessDenied { ref message })
                    if message.contains("Refusing to overwrite")
            ),
            "expected large-file denial, got {result:?}"
        );
    }

    /// On write failure the original file must remain untouched (atomic-write
    /// regression guard). We simulate failure by editing into a read-only
    /// directory so the temp file cannot be created.
    #[tokio::test]
    async fn test_edit_file_atomic_failure_preserves_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro").join("file.txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "original content").unwrap();
        // Make parent read-only so the temp file cannot be created.
        let mut perms = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(path.parent().unwrap(), perms.clone()).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "original content",
            "new_string": "new content",
        });
        let result = tool.run(&ctx, args).await;
        // Restore permissions before assertions so cleanup can run.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
        }
        #[cfg(not(unix))]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(path.parent().unwrap(), perms);
        assert!(
            matches!(result, ToolOutcome::Failure(ToolError::Internal { .. })),
            "expected failure, got {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "original content",
            "original file should be preserved on atomic-write failure"
        );
    }

    #[tokio::test]
    async fn test_fuzzy_fallback_exact_match_skips_fuzzy() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_exact_match.txt");
        std::fs::write(&path, content).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1;",
            "new_string": "let y = 2;",
        });

        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::FileEdit { .. }),
            "exact match should produce FileEdit, got {result:?}"
        );
        let got = std::fs::read_to_string(&path).unwrap();
        assert!(
            got.contains("let y = 2;"),
            "replacement should be present, got: {got:?}"
        );
        assert!(
            !got.contains("let x = 1;"),
            "old string should be gone, got: {got:?}"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_fuzzy_fallback_whitespace_tolerant() {
        let content = "fn main() {\n    let x = 1;   \n}\n";
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_whitespace.txt");
        std::fs::write(&path, content).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
        let ctx = ToolContext::new();
        // old_string has no trailing whitespace — fuzzy match should find it
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1;",
            "new_string": "let y = 2;",
        });

        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::FileEdit { .. }),
            "fuzzy match should produce FileEdit, got {result:?}"
        );
        let got = std::fs::read_to_string(&path).unwrap();
        assert!(
            got.contains("let y = 2;"),
            "replacement should be present via fuzzy match, got: {got:?}"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_fuzzy_fallback_no_match() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_no_match.txt");
        std::fs::write(&path, content).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
        let ctx = ToolContext::new();
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "this string does not exist",
            "new_string": "replacement",
        });

        let result = tool.run(&ctx, args).await;
        assert!(
            matches!(result, ToolOutcome::Failure(_) | ToolOutcome::Error { .. }),
            "no match should produce Failure or Error, got {result:?}"
        );
        // File should be unchanged
        let got = std::fs::read_to_string(&path).unwrap();
        assert_eq!(got, content, "file should be unchanged on no match");
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_fuzzy_fallback_partial_match_rejects() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_partial_match.txt");
        std::fs::write(&path, content).unwrap();

        let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
        let ctx = ToolContext::new();
        // "let x" is a substring of "let x = 1;" — partial match, not exact
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "let x",
            "new_string": "let y",
        });

        let result = tool.run(&ctx, args).await;
        // "let x" is a unique exact match in the file content (it's a
        // substring of "let x = 1;"), so edit_file will match it exactly
        // and replace just that substring.
        assert!(
            matches!(
                result,
                ToolOutcome::FileEdit { .. } | ToolOutcome::Failure(_)
            ),
            "partial substring match should be handled, got {result:?}"
        );
        remove_test_file(&path);
    }

    // ── Property-based tests ───────────────────────────────────────────

    // Property-based tests: for any content where `old` occurs exactly
    // once, replacing it with `new` must equal `content.replacen(old, new, 1)`.
    // We guarantee a unique match by constructing content from alphabets
    // that never overlap with `old`/`new`.
    proptest! {
        #[test]
        fn edit_file_replacement_equals_replacen(
            old in "[a-z]{3,8}",
            new in "[a-z]{3,8}",
            prefix in "[A-Z]{0,20}",
            suffix in "[A-Z]{0,20}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let _ = rt.block_on(async {
                let content = format!("{prefix}{old}{suffix}");
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("edit.txt");
                std::fs::write(&path, content.as_bytes()).unwrap();

                let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
                let ctx = ToolContext::new();
                let args = serde_json::json!({
                    "path": path.to_string_lossy(),
                    "old_string": old,
                    "new_string": new,
                });
                let result = tool.run(&ctx, args).await;
                prop_assert!(
                    matches!(result, ToolOutcome::FileEdit { .. }),
                    "expected FileEdit for unique exact match, got {result:?}"
                );

                let got = std::fs::read_to_string(&path).unwrap();
                let expected = content.replacen(&old, &new, 1);
                prop_assert_eq!(got, expected);
                Ok(())
            });
        }

        /// Replacing `old` with `new` and then `new` back with `old` must
        /// restore the original content — a round-trip / involution property
        /// that catches both replacement and undo-relevant byte preservation.
        #[test]
        fn edit_file_roundtrip_reverses_replacement(
            old in "[a-z]{3,8}",
            new in "[a-z]{3,8}",
            prefix in "[A-Z]{0,20}",
            suffix in "[A-Z]{0,20}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let _ = rt.block_on(async {
                let content = format!("{prefix}{old}{suffix}");
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("edit.txt");
                std::fs::write(&path, content.as_bytes()).unwrap();

                let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
                let ctx = ToolContext::new();
                let args = serde_json::json!({
                    "path": path.to_string_lossy(),
                    "old_string": old.clone(),
                    "new_string": new.clone(),
                });
                let first = tool.run(&ctx, args).await;
                prop_assert!(matches!(first, ToolOutcome::FileEdit { .. }), "first edit failed: {first:?}");

                let args2 = serde_json::json!({
                    "path": path.to_string_lossy(),
                    "old_string": new,
                    "new_string": old,
                });
                let second = tool.run(&ctx, args2).await;
                prop_assert!(matches!(second, ToolOutcome::FileEdit { .. }), "second edit failed: {second:?}");

                let got = std::fs::read_to_string(&path).unwrap();
                prop_assert_eq!(got, content);
                Ok(())
            });
        }

        /// Fuzzy fallback preserves non-matching lines byte-for-byte while
        /// still performing the replacement.
        #[test]
        fn edit_file_fuzzy_preserves_unmatched_lines(
            target in "[a-z]{3,8}",
            replacement in "[a-z]{3,8}",
            before in "[A-Z]{0,30}",
            after in "[A-Z]{0,30}",
            pad in proptest::collection::vec("[ \t]{0,5}", 2..=2),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let _ = rt.block_on(async {
                // Two lines: one unique lowercase target with trailing
                // whitespace, and surrounding uppercase context lines.
                let line0 = format!("{before}\n");
                let line1 = format!("{target}{}\n", pad[0]);
                let line2 = format!("{after}\n");
                let content = format!("{line0}{line1}{line2}");
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("edit.txt");
                std::fs::write(&path, content.as_bytes()).unwrap();

                let tool = EditFile::new(None, crate::session::access::PathGuard::default(), false);
                let ctx = ToolContext::new();
                let args = serde_json::json!({
                    "path": path.to_string_lossy(),
                    "old_string": target,
                    "new_string": replacement,
                });
                let result = tool.run(&ctx, args).await;
                prop_assert!(
                    matches!(result, ToolOutcome::FileEdit { .. }),
                    "expected fuzzy FileEdit, got {result:?}"
                );

                let got = std::fs::read_to_string(&path).unwrap();
                // Context lines must be unchanged.
                prop_assert!(got.starts_with(&line0), "prefix context changed: {got:?}");
                prop_assert!(got.ends_with(&line2), "suffix context changed: {got:?}");
                // The replacement should appear exactly once.
                prop_assert_eq!(got.matches(&replacement).count(), 1);
                Ok(())
            });
        }
    }
}
