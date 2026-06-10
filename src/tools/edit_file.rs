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
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'path' argument".into(),
                }
            }
        };

        let old = match args.get("old_string").and_then(|o| o.as_str()) {
            Some(o) => o.to_string(),
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'old_string' argument".into(),
                }
            }
        };

        let new = match args.get("new_string").and_then(|n| n.as_str()) {
            Some(n) => n.to_string(),
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'new_string' argument".into(),
                }
            }
        };

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                return ToolOutcome::Error {
                    message: format!("Cannot read {}: {}", path.display(), e),
                }
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
                    let newline_positions: Vec<usize> = content
                        .match_indices('\n')
                        .map(|(i, _)| i)
                        .collect();

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
                        Ok(_) => ToolOutcome::FileEdit { path, diff },
                        Err(e) => ToolOutcome::Error {
                            message: format!("Cannot write {}: {}", path.display(), e),
                        },
                    };
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fuzzy_fallback_preserves_original_formatting() {
        // Content with trailing whitespace on some lines
        let content = "fn main() {\n    let x = 1;    \n    println!(\"hello\");\n}\n";
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_edit_fuzzy.txt");
        std::fs::write(&path, content).unwrap();

        let tool = EditFile;
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1;",
            "new_string": "let y = 2;",
        });

        let result = tool.run(args).await;
        match result {
            ToolOutcome::FileEdit { path: _, diff: _ } => {
                let result_content = std::fs::read_to_string(&path).unwrap();
                // The trailing whitespace on line 2 should be preserved
                assert!(
                    result_content.contains("    let y = 2;    "),
                    "Fuzzy fallback should preserve original trailing whitespace, got: {:?}",
                    result_content
                );
            }
            other => panic!("Expected FileEdit, got {:?}", other),
        }

        let _ = std::fs::remove_file(&path);
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

        let tool = EditFile;
        let args = serde_json::json!({
            "path": path.to_string_lossy(),
            "old_string": "let x = 1;",
            "new_string": "let y = 2;",
        });

        let result = tool.run(args).await;
        match result {
            ToolOutcome::FileEdit { path: _, diff: _ } => {
                let result_content = std::fs::read_to_string(&path).unwrap();
                // Every line ending should still be CRLF, and the
                // replacement should have landed at exactly the line
                // boundary (not in the middle of a `\r\n`).
                assert!(
                    result_content.contains("    let y = 2;\r\n"),
                    "Replacement should land at a line boundary with CRLF preserved, got: {:?}",
                    result_content
                );
                // The file should not have any orphaned `\r` characters
                // or missing newlines. Count the CRLFs in the original
                // and check the result has the same count.
                let original_crlf_count = content.matches("\r\n").count();
                let result_crlf_count = result_content.matches("\r\n").count();
                assert_eq!(
                    original_crlf_count, result_crlf_count,
                    "Number of CRLF line endings should be preserved (orig={}, result={})",
                    original_crlf_count, result_crlf_count
                );
            }
            other => panic!("Expected FileEdit, got {:?}", other),
        }

        let _ = std::fs::remove_file(&path);
    }
}
