use crate::session::access::PathGuard;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::path::PathBuf;

pub struct ReadFile {
    path_guard: PathGuard,
    minify_write_side: bool,
}

impl ReadFile {
    pub fn new(path_guard: PathGuard, minify_write_side: bool) -> Self {
        Self {
            path_guard,
            minify_write_side,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file",
            description: "Read the contents of a file. Use offset and limit to read specific sections. Set minify=true to strip comments and collapse whitespace (saves ~30-50% tokens for source files). When config.minify_write_side is true, minified reads are wrapped in <minified lang='...'> envelopes; edit_file/write_file will expand them back to readable source.",
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
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'path' argument"));
            }
        };

        if let crate::session::access::GuardVerdict::Denied(reason) =
            self.path_guard.check_read(&path)
        {
            return ToolOutcome::Failure(ToolError::AccessDenied { message: reason });
        }

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

        let raw_lines: Vec<&str> = raw_content.lines().collect();
        let raw_total = raw_lines.len();

        if offset >= raw_total && raw_total > 0 {
            return ToolOutcome::Failure(ToolError::Internal {
                message: format!("Offset {offset} is beyond file length {raw_total}"),
            });
        }

        if raw_total == 0 {
            return ToolOutcome::Success {
                content: format!("{} — empty file", path.display()),
            };
        }

        let end = std::cmp::min(offset.saturating_add(limit), raw_total);
        let selected_raw = raw_lines[offset..end].join("\n");
        let truncated = end < raw_total;

        // Apply minification to the selected slice only, so offset/limit
        // refer to the original file lines. Whole-file reads still show
        // the byte-saved summary.
        let minify = args
            .get("minify")
            .and_then(|m| m.as_bool())
            .unwrap_or(false);
        let selected = if minify {
            crate::shared::minify::minify_source(&path, &selected_raw)
        } else {
            selected_raw
        };

        let display = if offset == 0 && end >= raw_total {
            if minify {
                let body = if self.minify_write_side {
                    let lang = crate::shared::minify::lang_name_for_ext(
                        path.extension().and_then(|e| e.to_str()).unwrap_or("txt"),
                    );
                    crate::shared::minify::wrap_minified_envelope(&lang, &selected)
                } else {
                    selected.clone()
                };
                format!(
                    "{} (minified, was {} bytes → now {} bytes)\n{}",
                    path.display(),
                    raw_content.len(),
                    selected.len(),
                    body,
                )
            } else {
                raw_content
            }
        } else {
            let header = format!(
                "{} (showing lines {}-{} of {})",
                path.display(),
                offset + 1,
                end,
                raw_total
            );
            let body = if minify && self.minify_write_side {
                let lang = crate::shared::minify::lang_name_for_ext(
                    path.extension().and_then(|e| e.to_str()).unwrap_or("txt"),
                );
                crate::shared::minify::wrap_minified_envelope(&lang, &selected)
            } else {
                selected
            };
            format!("{header}\n{sep}\n{body}", sep = "-".repeat(header.len()))
        };

        ToolOutcome::FileContent {
            path,
            content: display,
            truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use serde_json::json;
    use std::io::Write;

    #[tokio::test]
    async fn whole_file_minify_computes_once_and_includes_byte_stats() {
        // Regression for C16: whole-file minified reads used to call
        // minify_source twice (once for the selected slice and again for
        // the full-file stats/body). The output must report the actual
        // minified size and include the minified body.
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_minify_test_{}.rs",
            std::process::id()
        ));
        let source = "// header\npub fn add(a: i32, b: i32) -> i32 { a + b }\n";
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(source.as_bytes()).unwrap();
        }

        let tool = ReadFile::new(PathGuard::default(), false);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({
                    "path": tmp.to_string_lossy(),
                    "minify": true,
                }),
            )
            .await;

        std::fs::remove_file(&tmp).ok();

        let ToolOutcome::FileContent { content, .. } = outcome else {
            panic!("expected FileContent, got {outcome:?}");
        };
        assert!(
            content.contains("(minified, was"),
            "missing minification header: {content}"
        );
        assert!(
            content.contains("pub fn add"),
            "minified body missing source content: {content}"
        );
        assert!(
            !content.contains("// header"),
            "comment should have been stripped: {content}"
        );
    }
}
