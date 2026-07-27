use crate::session::access::PathGuard;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::path::PathBuf;

pub struct ReadFile {
    path_guard: PathGuard,
    minify_write_side: bool,
    #[allow(dead_code)]
    minify_above_bytes: usize,
}

impl ReadFile {
    pub fn new(path_guard: PathGuard, minify_write_side: bool, minify_above_bytes: usize) -> Self {
        Self {
            path_guard,
            minify_write_side,
            minify_above_bytes,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file",
            description: "Read the contents of a file. Use offset and limit to read specific sections. Set minify=true to strip comments and collapse whitespace (saves ~30-50% tokens for source files). When config.tools.minify_write_side is true, minified reads are wrapped in <minified lang='...'> envelopes; edit_file/write_file will expand them back to readable source.",
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
        //
        // `minify` is tri-state:
        //   - Some(true)  → force minify (caller asked for it)
        //   - Some(false) → force raw   (caller opted out)
        //   - None        → auto: minify when the whole file exceeds
        //                   `minify_above_bytes`. The note appended
        //                   below tells the model how to see the full
        //                   content.
        let minify_arg = args.get("minify").and_then(|m| m.as_bool());
        let auto_minified = minify_arg.is_none() && raw_content.len() > self.minify_above_bytes;
        let minify = minify_arg.unwrap_or(auto_minified);
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
                let header = format!(
                    "{} (minified, was {} bytes → now {} bytes)\n{}",
                    path.display(),
                    raw_content.len(),
                    selected.len(),
                    body,
                );
                if auto_minified {
                    let note = format!(
                        "[minified: {} lines → {} lines, use read_file with minify=false to see full content]",
                        raw_total,
                        selected.lines().count(),
                    );
                    format!("{header}\n{note}")
                } else {
                    header
                }
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

        let tool = ReadFile::new(PathGuard::default(), false, 4096);
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

    // ── WO 9.7: VFS minification threshold + override ──────────────────

    /// A small file (under `minify_above_bytes`) with no explicit `minify`
    /// arg is returned raw — no auto-minification, no note.
    #[tokio::test]
    async fn threshold_skip_small_file_not_minified() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_threshold_skip_{}.rs",
            std::process::id()
        ));
        let source = "// tiny comment\nfn add(a: i32, b: i32) -> i32 { a + b }\n";
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(source.as_bytes()).unwrap();
        }

        let tool = ReadFile::new(PathGuard::default(), false, 4096);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({ "path": tmp.to_string_lossy() }),
            )
            .await;
        std::fs::remove_file(&tmp).ok();

        let ToolOutcome::FileContent { content, .. } = outcome else {
            panic!("expected FileContent, got {outcome:?}");
        };
        assert!(
            content.contains("// tiny comment"),
            "small file should not be auto-minified: {content}"
        );
        assert!(
            !content.contains("[minified:"),
            "small file should not carry the minified note: {content}"
        );
    }

    /// A file larger than `minify_above_bytes` with no explicit `minify`
    /// arg is auto-minified and the output carries the WO's note.
    #[tokio::test]
    async fn auto_minify_large_file_emits_note() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_auto_minify_{}.rs",
            std::process::id()
        ));
        let mut source = String::new();
        for _ in 0..40 {
            source.push_str("// filler comment line that should be stripped\n");
        }
        source.push_str("pub fn add(a: i32, b: i32) -> i32 { a + b }\n");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(source.as_bytes()).unwrap();
        }

        let tool = ReadFile::new(PathGuard::default(), false, 64);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({ "path": tmp.to_string_lossy() }),
            )
            .await;
        std::fs::remove_file(&tmp).ok();

        let ToolOutcome::FileContent { content, .. } = outcome else {
            panic!("expected FileContent, got {outcome:?}");
        };
        assert!(
            content.contains("(minified, was"),
            "auto-minified file should carry the byte header: {content}"
        );
        assert!(
            content.contains("[minified:"),
            "auto-minified file should carry the lines note: {content}"
        );
        assert!(
            content.contains("use read_file with minify=false"),
            "note should tell the model how to opt out: {content}"
        );
        assert!(
            content.contains("pub fn add"),
            "code should survive: {content}"
        );
        assert!(
            !content.contains("filler comment"),
            "comments should be stripped: {content}"
        );
    }

    /// Even a large file is returned raw when the model passes `minify=false`.
    #[tokio::test]
    async fn explicit_minify_false_returns_raw() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_explicit_false_{}.rs",
            std::process::id()
        ));
        let mut source = String::new();
        for _ in 0..40 {
            source.push_str("// filler comment line that should remain\n");
        }
        source.push_str("pub fn add(a: i32, b: i32) -> i32 { a + b }\n");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(source.as_bytes()).unwrap();
        }

        let tool = ReadFile::new(PathGuard::default(), false, 64);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({ "path": tmp.to_string_lossy(), "minify": false }),
            )
            .await;
        std::fs::remove_file(&tmp).ok();

        let ToolOutcome::FileContent { content, .. } = outcome else {
            panic!("expected FileContent, got {outcome:?}");
        };
        assert!(
            content.contains("filler comment"),
            "explicit minify=false must return raw content: {content}"
        );
        assert!(
            !content.contains("[minified:"),
            "raw output must not carry the minified note: {content}"
        );
        assert!(
            !content.contains("(minified, was"),
            "raw output must not carry the minified header: {content}"
        );
    }

    #[tokio::test]
    async fn missing_path_arg_is_invalid_args() {
        let tool = ReadFile::new(PathGuard::default(), false, 4096);
        let outcome = tool.run(&ToolContext::new(), json!({})).await;
        match outcome {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(message.contains("Missing 'path'"), "got {message}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn path_guard_denial_propagates_access_denied() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_read_file_deny_test.pem");
        std::fs::write(&path, "secret").unwrap();
        let tool = ReadFile::new(PathGuard::default(), false, 4096);
        let outcome = tool
            .run(&ToolContext::new(), json!({ "path": path.to_string_lossy() }))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::AccessDenied { .. })),
            "default deny list should block .pem, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn read_nonexistent_file_returns_internal_error() {
        let tool = ReadFile::new(PathGuard::default(), false, 4096);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({ "path": "/nonexistent/kirkforge/no/such/file.txt" }),
            )
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::Internal { message }) => {
                assert!(message.contains("Cannot read"), "got {message}");
            }
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_file_returns_empty_marker() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_empty_{}.txt",
            std::process::id()
        ));
        std::fs::write(&tmp, "").unwrap();
        let tool = ReadFile::new(PathGuard::default(), false, 4096);
        let outcome = tool
            .run(&ToolContext::new(), json!({ "path": tmp.to_string_lossy() }))
            .await;
        std::fs::remove_file(&tmp).ok();
        match outcome {
            ToolOutcome::FileContent { content, .. } => {
                assert!(content.contains("empty file"), "expected empty marker, got: {content}");
            }
            other => panic!("expected FileContent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn offset_beyond_file_length_returns_internal_error() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_offset_{}.txt",
            std::process::id()
        ));
        std::fs::write(&tmp, "a\nb\nc\n").unwrap();
        let tool = ReadFile::new(PathGuard::default(), false, 4096);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({ "path": tmp.to_string_lossy(), "offset": 999 }),
            )
            .await;
        std::fs::remove_file(&tmp).ok();
        match outcome {
            ToolOutcome::Failure(ToolError::Internal { message }) => {
                assert!(message.contains("Offset 999 is beyond"), "got {message}");
            }
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn partial_read_reports_truncated_lines_range() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_partial_{}.txt",
            std::process::id()
        ));
        let mut source = String::new();
        for i in 0..10 {
            source.push_str(&format!("line {i}\n"));
        }
        std::fs::write(&tmp, &source).unwrap();
        let tool = ReadFile::new(PathGuard::default(), false, 4096);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({ "path": tmp.to_string_lossy(), "offset": 2, "limit": 3 }),
            )
            .await;
        std::fs::remove_file(&tmp).ok();
        match outcome {
            ToolOutcome::FileContent { content, truncated, .. } => {
                assert!(truncated, "expected truncated=true");
                assert!(content.contains("showing lines 3-5 of 10"), "got: {content}");
                assert!(content.contains("line 2"));
                assert!(content.contains("line 4"));
            }
            other => panic!("expected FileContent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn whole_file_read_reports_not_truncated() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_whole_{}.txt",
            std::process::id()
        ));
        std::fs::write(&tmp, "hello\nworld\n").unwrap();
        let tool = ReadFile::new(PathGuard::default(), false, 4096);
        let outcome = tool
            .run(&ToolContext::new(), json!({ "path": tmp.to_string_lossy() }))
            .await;
        std::fs::remove_file(&tmp).ok();
        match outcome {
            ToolOutcome::FileContent { truncated, .. } => {
                assert!(!truncated, "whole file should not be truncated");
            }
            other => panic!("expected FileContent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn minify_write_side_wraps_envelope_when_enabled() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_envelope_{}.rs",
            std::process::id()
        ));
        let source = "// header\npub fn add(a: i32, b: i32) -> i32 { a + b }\n";
        std::fs::write(&tmp, source).unwrap();
        let tool = ReadFile::new(PathGuard::default(), true, 4096);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({ "path": tmp.to_string_lossy(), "minify": true }),
            )
            .await;
        std::fs::remove_file(&tmp).ok();
        let ToolOutcome::FileContent { content, .. } = outcome else {
            panic!("expected FileContent, got {outcome:?}");
        };
        assert!(
            content.contains("minified") && content.contains("pub fn add"),
            "minified header + body should be present: {content}"
        );
    }

    #[tokio::test]
    async fn partial_read_with_minify_write_side_wraps_envelope() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_read_file_partial_min_{}.rs",
            std::process::id()
        ));
        let mut source = String::new();
        for _ in 0..20 {
            source.push_str("// filler\n");
        }
        source.push_str("pub fn add(a: i32, b: i32) -> i32 { a + b }\n");
        std::fs::write(&tmp, &source).unwrap();
        let tool = ReadFile::new(PathGuard::default(), true, 4096);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({ "path": tmp.to_string_lossy(), "offset": 0, "limit": 10, "minify": true }),
            )
            .await;
        std::fs::remove_file(&tmp).ok();
        let ToolOutcome::FileContent { content, truncated, .. } = outcome else {
            panic!("expected FileContent, got {outcome:?}");
        };
        assert!(truncated, "should be truncated");
        assert!(
            content.contains("showing lines") && content.contains("pub fn add"),
            "header + minified body should be present: {content}"
        );
    }

    #[test]
    fn def_has_correct_name_and_required_path() {
        let tool = ReadFile::new(PathGuard::default(), false, 4096);
        let def = tool.def();
        assert_eq!(def.name, "read_file");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("path")));
    }
}
