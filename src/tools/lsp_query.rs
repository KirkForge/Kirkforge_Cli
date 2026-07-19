//! `lsp_query` tool — symbol-aware code navigation via the LSP pool.
//!
//! Ports Vix's `lspQueryImpl` + `lspFileOperation` into KirkForge's Rust
//! runtime. The tool dispatches on an `operation` argument and calls the
//! matching method on [`LspPool`][kirkforge_lsp::LspPool]. File-based
//! operations (`go_to_definition`, `find_references`, `hover`,
//! `document_symbols`, `find_implementations`, `diagnostics`) follow the
//! Vix pattern: resolve the file to an absolute path inside the sandbox,
//! `textDocument/didOpen` → query → `textDocument/didClose`.
//!
//! When no LSP server is configured for the file's language, the tool
//! returns a clear `Error` outcome naming the missing language — it never
//! fakes results (matches the `web_search` precedent).

use crate::session::access::{GuardVerdict, PathGuard};
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use kirkforge_lsp::{uri_to_path, LspPool};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Per-query timeout. Bounded so a frozen LSP server does not hang a turn.
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

pub struct LspQuery {
    pool: Arc<LspPool>,
    path_guard: PathGuard,
}

impl LspQuery {
    pub fn new(pool: Arc<LspPool>, path_guard: PathGuard) -> Self {
        Self { pool, path_guard }
    }
}

#[async_trait::async_trait]
impl Tool for LspQuery {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "lsp_query",
            description: "Query an LSP server for symbol-aware code navigation: \
 go_to_definition, find_references, hover, document_symbols, find_implementations, \
 workspace_symbols, or diagnostics. Requires an LSP server configured for the file's \
 language in config.toml. Line and character are 1-based.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": [
                            "workspace_symbols",
                            "go_to_definition",
                            "find_references",
                            "hover",
                            "document_symbols",
                            "find_implementations",
                            "diagnostics"
                        ],
                        "description": "LSP operation to perform"
                    },
                    "file": {
                        "type": "string",
                        "description": "File path (relative to cwd or absolute). Required for all operations except workspace_symbols."
                    },
                    "line": {
                        "type": "integer",
                        "description": "1-based line number. Required for go_to_definition, find_references, hover, find_implementations."
                    },
                    "character": {
                        "type": "integer",
                        "description": "1-based column. Required for go_to_definition, find_references, hover, find_implementations."
                    },
                    "query": {
                        "type": "string",
                        "description": "Symbol query string. Required for workspace_symbols."
                    },
                    "include_decl": {
                        "type": "boolean",
                        "default": true,
                        "description": "For find_references: include the declaration site in results."
                    }
                },
                "required": ["operation"]
            }),
        }
    }

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let op = match args.get("operation").and_then(|o| o.as_str()) {
            Some(o) => o.to_string(),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing 'operation' argument",
                ));
            }
        };

        let result = match op.as_str() {
            "workspace_symbols" => self.op_workspace_symbols(ctx, &args).await,
            "go_to_definition" => self.op_location_query(ctx, &args, "definition").await,
            "find_references" => self.op_references(ctx, &args).await,
            "hover" => self.op_hover(ctx, &args).await,
            "document_symbols" => self.op_document_symbols(ctx, &args).await,
            "find_implementations" => self.op_location_query(ctx, &args, "implementation").await,
            "diagnostics" => self.op_diagnostics(ctx, &args).await,
            other => {
                return ToolOutcome::Failure(ToolError::invalid_args(format!(
                    "Unknown operation '{other}'. Valid: workspace_symbols, \
                     go_to_definition, find_references, hover, document_symbols, \
                     find_implementations, diagnostics"
                )));
            }
        };
        result
    }
}

impl LspQuery {
    /// `workspace_symbols` — query across all configured languages.
    async fn op_workspace_symbols(
        &self,
        ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> ToolOutcome {
        let query = match args.get("query").and_then(|q| q.as_str()) {
            Some(q) => q.to_string(),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "workspace_symbols requires a 'query' argument",
                ));
            }
        };
        let languages = self.pool.configured_languages();
        if languages.is_empty() {
            return ToolOutcome::Error {
                message: "No LSP servers configured. Add an [[lsp_servers]] entry to \
                          config.toml to enable symbol-aware navigation."
                    .to_string(),
            };
        }
        let mut all_lines: Vec<String> = Vec::new();
        for lang in languages {
            if ctx.token.is_cancelled() {
                return ToolOutcome::Failure(ToolError::Cancelled);
            }
            let client =
                match tokio::time::timeout(QUERY_TIMEOUT, self.pool.get_client(&lang)).await {
                    Ok(Ok(Some(c))) => c,
                    Ok(Ok(None)) => {
                        // Cooldown or unconfigured — skip with a note.
                        all_lines.push(format!(
                            "[{lang}: no LSP server available (cooldown or unconfigured)]"
                        ));
                        continue;
                    }
                    Ok(Err(e)) => {
                        all_lines.push(format!("[{lang}: LSP pool error: {e}]"));
                        continue;
                    }
                    Err(_) => {
                        all_lines.push(format!("[{lang}: timed out getting client]"));
                        continue;
                    }
                };
            let query_fut = client.workspace_symbols(&query);
            let symbols = match tokio::time::timeout(QUERY_TIMEOUT, query_fut).await {
                Ok(Ok(syms)) => syms,
                Ok(Err(e)) => {
                    all_lines.push(format!("[{lang}: workspace_symbols error: {e}]"));
                    continue;
                }
                Err(_) => {
                    all_lines.push(format!("[{lang}: workspace_symbols timed out]"));
                    continue;
                }
            };
            if symbols.is_empty() {
                all_lines.push(format!("[{lang}: no symbols matched]"));
            } else {
                for sym in symbols {
                    let path = uri_to_path(&sym.location.uri);
                    let path = short_path(&path);
                    all_lines.push(format!(
                        "{}:{}:{}  {}  kind={}",
                        path,
                        sym.location.range.start.line + 1,
                        sym.location.range.start.character + 1,
                        sym.name,
                        sym.kind
                    ));
                }
            }
        }
        if all_lines.is_empty() {
            ToolOutcome::Success {
                content: "No symbols found.".to_string(),
            }
        } else {
            ToolOutcome::Success {
                content: all_lines.join("\n"),
            }
        }
    }

    /// `go_to_definition` / `find_implementations` — shared request shape.
    async fn op_location_query(
        &self,
        ctx: &ToolContext,
        args: &serde_json::Value,
        kind: &str,
    ) -> ToolOutcome {
        // Validate line/character up front so a bad-args failure is not
        // masked by a later "no LSP server configured" error.
        let line = match args.get("line").and_then(|l| l.as_u64()) {
            Some(l) => l as u32,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(format!(
                    "{kind} requires a 1-based 'line' argument"
                )));
            }
        };
        let character = match args.get("character").and_then(|c| c.as_u64()) {
            Some(c) => c as u32,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(format!(
                    "{kind} requires a 1-based 'character' argument"
                )));
            }
        };
        let (client, uri, _lang, _path, _text) = match self.prepare_file_op(ctx, args).await {
            Ok(t) => t,
            Err(o) => return o,
        };
        let method = if kind == "definition" {
            "go_to_definition"
        } else {
            "find_implementations"
        };
        // Branch the timeout around each call separately — the two
        // methods return distinct `impl Future` types that can't be
        // unified in a single `if/else`.
        let locs: std::result::Result<Vec<kirkforge_lsp::Location>, anyhow::Error> = if kind
            == "definition"
        {
            let f = client.definition(&uri, line.saturating_sub(1), character.saturating_sub(1));
            match tokio::time::timeout(QUERY_TIMEOUT, f).await {
                Ok(r) => r,
                Err(_) => {
                    return ToolOutcome::Failure(ToolError::Timeout {
                        after_secs: QUERY_TIMEOUT.as_secs(),
                    });
                }
            }
        } else {
            let f =
                client.implementation(&uri, line.saturating_sub(1), character.saturating_sub(1));
            match tokio::time::timeout(QUERY_TIMEOUT, f).await {
                Ok(r) => r,
                Err(_) => {
                    return ToolOutcome::Failure(ToolError::Timeout {
                        after_secs: QUERY_TIMEOUT.as_secs(),
                    });
                }
            }
        };
        let locs = match locs {
            Ok(l) => l,
            Err(e) => {
                return ToolOutcome::Error {
                    message: format!("{method} failed: {e}"),
                };
            }
        };
        if locs.is_empty() {
            return ToolOutcome::Success {
                content: format!("No {kind} locations found."),
            };
        }
        let lines: Vec<String> = locs
            .iter()
            .map(|l| {
                let p = uri_to_path(&l.uri);
                format!(
                    "{}:{}:{}",
                    p,
                    l.range.start.line + 1,
                    l.range.start.character + 1
                )
            })
            .collect();
        ToolOutcome::Success {
            content: lines.join("\n"),
        }
    }

    async fn op_references(&self, ctx: &ToolContext, args: &serde_json::Value) -> ToolOutcome {
        let line = match args.get("line").and_then(|l| l.as_u64()) {
            Some(l) => l as u32,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "find_references requires a 1-based 'line' argument",
                ));
            }
        };
        let character = match args.get("character").and_then(|c| c.as_u64()) {
            Some(c) => c as u32,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "find_references requires a 1-based 'character' argument",
                ));
            }
        };
        let include_decl = args
            .get("include_decl")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let (client, uri, _lang, _path, _text) = match self.prepare_file_op(ctx, args).await {
            Ok(t) => t,
            Err(o) => return o,
        };
        let refs = client.references(
            &uri,
            line.saturating_sub(1),
            character.saturating_sub(1),
            include_decl,
        );
        let refs = match tokio::time::timeout(QUERY_TIMEOUT, refs).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                return ToolOutcome::Error {
                    message: format!("find_references failed: {e}"),
                };
            }
            Err(_) => {
                return ToolOutcome::Failure(ToolError::Timeout {
                    after_secs: QUERY_TIMEOUT.as_secs(),
                });
            }
        };
        if refs.is_empty() {
            return ToolOutcome::Success {
                content: "No references found.".to_string(),
            };
        }
        let lines: Vec<String> = refs
            .iter()
            .map(|l| {
                let p = uri_to_path(&l.uri);
                format!(
                    "{}:{}:{}",
                    p,
                    l.range.start.line + 1,
                    l.range.start.character + 1
                )
            })
            .collect();
        ToolOutcome::Success {
            content: lines.join("\n"),
        }
    }

    async fn op_hover(&self, ctx: &ToolContext, args: &serde_json::Value) -> ToolOutcome {
        let line = match args.get("line").and_then(|l| l.as_u64()) {
            Some(l) => l as u32,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "hover requires a 1-based 'line' argument",
                ));
            }
        };
        let character = match args.get("character").and_then(|c| c.as_u64()) {
            Some(c) => c as u32,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "hover requires a 1-based 'character' argument",
                ));
            }
        };
        let (client, uri, _lang, _path, _text) = match self.prepare_file_op(ctx, args).await {
            Ok(t) => t,
            Err(o) => return o,
        };
        let hover = client.hover(&uri, line.saturating_sub(1), character.saturating_sub(1));
        let hover = match tokio::time::timeout(QUERY_TIMEOUT, hover).await {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => {
                return ToolOutcome::Error {
                    message: format!("hover failed: {e}"),
                };
            }
            Err(_) => {
                return ToolOutcome::Failure(ToolError::Timeout {
                    after_secs: QUERY_TIMEOUT.as_secs(),
                });
            }
        };
        match hover {
            Some(h) => ToolOutcome::Success {
                content: h.contents,
            },
            None => ToolOutcome::Success {
                content: "No hover information available at this position.".to_string(),
            },
        }
    }

    async fn op_document_symbols(
        &self,
        ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> ToolOutcome {
        let (client, uri, _lang, _path, _text) = match self.prepare_file_op(ctx, args).await {
            Ok(t) => t,
            Err(o) => return o,
        };
        let syms = client.document_symbol(&uri);
        let syms = match tokio::time::timeout(QUERY_TIMEOUT, syms).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return ToolOutcome::Error {
                    message: format!("document_symbols failed: {e}"),
                };
            }
            Err(_) => {
                return ToolOutcome::Failure(ToolError::Timeout {
                    after_secs: QUERY_TIMEOUT.as_secs(),
                });
            }
        };
        if syms.is_empty() {
            return ToolOutcome::Success {
                content: "No document symbols found.".to_string(),
            };
        }
        let mut lines = Vec::new();
        render_document_symbols(&syms, 0, &mut lines);
        ToolOutcome::Success {
            content: lines.join("\n"),
        }
    }

    async fn op_diagnostics(&self, ctx: &ToolContext, args: &serde_json::Value) -> ToolOutcome {
        let (client, uri, _lang, _path, _text) = match self.prepare_file_op(ctx, args).await {
            Ok(t) => t,
            Err(o) => return o,
        };
        // Wait for a publishDiagnostics notification. The LSP server may
        // already have sent one as part of didOpen processing.
        let diags = client.wait_for_diagnostics(&uri, QUERY_TIMEOUT).await;
        if diags.is_empty() {
            return ToolOutcome::Success {
                content: "No diagnostics reported for this file.".to_string(),
            };
        }
        let mut lines = Vec::with_capacity(diags.len());
        for d in &diags {
            let sev = match d.severity {
                1 => "ERROR",
                2 => "WARN",
                3 => "INFO",
                4 => "HINT",
                _ => "?",
            };
            let source = if d.source.is_empty() {
                String::new()
            } else {
                format!(" [{}]", d.source)
            };
            lines.push(format!(
                "{}:{}:{} [{}]{} {}",
                uri_to_path(&uri),
                d.range.start.line + 1,
                d.range.start.character + 1,
                sev,
                source,
                d.message.trim()
            ));
        }
        ToolOutcome::Success {
            content: lines.join("\n"),
        }
    }

    /// Resolve `file`, run sandbox check, didOpen, and return the client +
    /// uri + file text. The caller is responsible for `did_close`.
    ///
    /// On any failure this returns a `ToolOutcome` in `Err`.
    async fn prepare_file_op(
        &self,
        ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<
        (
            std::sync::Arc<kirkforge_lsp::LspClient>,
            String,
            String,
            PathBuf,
            String,
        ),
        ToolOutcome,
    > {
        let file = match args.get("file").and_then(|f| f.as_str()) {
            Some(f) => f.to_string(),
            None => {
                return Err(ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing 'file' argument",
                )));
            }
        };
        let path = PathBuf::from(shellexpand::tilde(&file).as_ref());
        let resolved = match &self.path_guard.check_read(&path) {
            GuardVerdict::Allowed(p) => p.clone(),
            GuardVerdict::Denied(reason) => {
                return Err(ToolOutcome::Failure(ToolError::AccessDenied {
                    message: reason.clone(),
                }));
            }
        };
        if ctx.token.is_cancelled() {
            return Err(ToolOutcome::Failure(ToolError::Cancelled));
        }
        let ext = resolved
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let language = match self.pool.language_for_ext(&ext) {
            Some(l) => l,
            None => {
                return Err(ToolOutcome::Error {
                    message: format!(
                        "No LSP server configured for extension '{ext}'. \
                         Add an [[lsp_servers]] entry with this extension to \
                         config.toml."
                    ),
                });
            }
        };
        let client =
            match tokio::time::timeout(QUERY_TIMEOUT, self.pool.get_client(&language)).await {
                Ok(Ok(Some(c))) => c,
                Ok(Ok(None)) => {
                    return Err(ToolOutcome::Error {
                        message: format!(
                            "No LSP server available for language '{language}' \
                         (unconfigured or in fail cooldown after a prior failed start)."
                        ),
                    });
                }
                Ok(Err(e)) => {
                    return Err(ToolOutcome::Error {
                        message: format!("LSP pool error for '{language}': {e}"),
                    });
                }
                Err(_) => {
                    return Err(ToolOutcome::Failure(ToolError::Timeout {
                        after_secs: QUERY_TIMEOUT.as_secs(),
                    }));
                }
            };
        let text = match std::fs::read_to_string(&resolved) {
            Ok(t) => t,
            Err(e) => {
                return Err(ToolOutcome::Failure(ToolError::Internal {
                    message: format!("Cannot read {}: {}", resolved.display(), e),
                }));
            }
        };
        let uri = path_to_uri(&resolved);
        let language_id = language_id_for(&language);
        if let Err(e) = client.did_open(&uri, &language_id, &text).await {
            return Err(ToolOutcome::Error {
                message: format!("didOpen failed: {e}"),
            });
        }
        // Schedule a didClose on the client (best-effort — we can't hold a
        // reference across the await boundary cleanly without spawning).
        let client_close = client.clone();
        let uri_close = uri.clone();
        tokio::spawn(async move {
            let _ = client_close.did_close(&uri_close).await;
        });
        Ok((client, uri, language, resolved, text))
    }
}

/// Render hierarchical document symbols as indented lines.
fn render_document_symbols(
    syms: &[kirkforge_lsp::DocumentSymbol],
    depth: usize,
    out: &mut Vec<String>,
) {
    for sym in syms {
        let indent = "  ".repeat(depth);
        out.push(format!(
            "{}{} (kind={}, {}:{})",
            indent,
            sym.name,
            sym.kind,
            sym.range.start.line + 1,
            sym.range.start.character + 1
        ));
        if !sym.children.is_empty() {
            render_document_symbols(&sym.children, depth + 1, out);
        }
    }
}

/// Map a language name to the LSP `languageId` string.
fn language_id_for(language: &str) -> String {
    match language {
        "rust" => "rust".to_string(),
        "typescript" | "typescriptreact" => "typescript".to_string(),
        "javascript" | "javascriptreact" => "javascript".to_string(),
        "python" => "python".to_string(),
        "go" => "go".to_string(),
        "c" => "c".to_string(),
        "cpp" | "c++" => "cpp".to_string(),
        "java" => "java".to_string(),
        "ruby" => "ruby".to_string(),
        other => other.to_string(),
    }
}

/// Convert a filesystem path to a `file://` URI.
fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// Shorten an absolute path for display by stripping the cwd prefix when
/// possible. Falls back to the full path.
fn short_path(path: &str) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_s = cwd.to_string_lossy().to_string();
        if let Some(rest) = path.strip_prefix(&cwd_s) {
            return rest.trim_start_matches('/').to_string();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::access::PathGuard;
    use kirkforge_lsp::{LanguageConfig, LspPool};
    use tokio_util::sync::CancellationToken;

    fn empty_pool() -> Arc<LspPool> {
        Arc::new(LspPool::new(
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            vec![],
        ))
    }

    fn rust_pool_no_binary() -> Arc<LspPool> {
        Arc::new(LspPool::new(
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            vec![LanguageConfig {
                name: "rust".to_string(),
                extensions: vec![".rs".to_string()],
                lsp: Some(kirkforge_lsp::LspServerConfig {
                    command: "/nonexistent/binary/xyzzy".to_string(),
                    args: vec![],
                }),
            }],
        ))
    }

    #[test]
    fn def_is_valid_json() {
        let pool = empty_pool();
        let tool = LspQuery::new(pool, PathGuard::default());
        let def = tool.def();
        assert_eq!(def.name, "lsp_query");
        assert!(def.parameters.get("properties").is_some());
        let props = def
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("properties is object");
        assert!(props.contains_key("operation"));
        assert!(props.contains_key("file"));
        assert!(props.contains_key("line"));
    }

    #[tokio::test]
    async fn missing_operation_is_invalid_args() {
        let pool = empty_pool();
        let tool = LspQuery::new(pool, PathGuard::default());
        let outcome = tool.run(&ToolContext::new(), serde_json::json!({})).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn unknown_operation_is_invalid_args() {
        let pool = empty_pool();
        let tool = LspQuery::new(pool, PathGuard::default());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"operation": "bogus_op"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn workspace_symbols_no_configs_returns_error() {
        let pool = empty_pool();
        let tool = LspQuery::new(pool, PathGuard::default());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"operation": "workspace_symbols", "query": "foo"}),
            )
            .await;
        let msg = match outcome {
            ToolOutcome::Error { message } => message,
            other => panic!("expected Error, got {other:?}"),
        };
        assert!(msg.contains("No LSP servers configured"), "{msg}");
    }

    #[tokio::test]
    async fn file_op_unconfigured_language_returns_error() {
        // A pool with no configs — any file op should hit the
        // "no LSP server configured for extension" path.
        let pool = empty_pool();
        let tool = LspQuery::new(pool, PathGuard::default());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({
                    "operation": "go_to_definition",
                    "file": "src/lib.rs",
                    "line": 1,
                    "character": 1
                }),
            )
            .await;
        let msg = match outcome {
            ToolOutcome::Error { message } => message,
            other => panic!("expected Error, got {other:?}"),
        };
        assert!(msg.contains("No LSP server configured"), "got {msg}");
    }

    #[tokio::test]
    async fn file_op_missing_file_arg_is_invalid_args() {
        let pool = empty_pool();
        let tool = LspQuery::new(pool, PathGuard::default());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"operation": "hover", "line": 1, "character": 1}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn go_to_definition_missing_line_is_invalid_args() {
        // Use a real file so we get past the file-existence check and hit
        // the line-validation branch.
        let pool = empty_pool();
        let tool = LspQuery::new(pool, PathGuard::default());
        // Use a nonexistent file — the path guard default is unsandboxed,
        // so check_read will fail on the non-existent path. Use a real
        // file (this very source file) instead.
        let me =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tools/lsp_query.rs");
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({
                    "operation": "go_to_definition",
                    "file": me.to_string_lossy(),
                    "character": 1
                }),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn go_to_definition_unconfigured_extension_returns_error() {
        // Pool has rust configured but pointing at a missing binary; the
        // .rs extension resolves to "rust" and we surface the cooldown
        // error (not a fake result).
        let pool = rust_pool_no_binary();
        let tool = LspQuery::new(pool, PathGuard::default());
        let me =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tools/lsp_query.rs");
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({
                    "operation": "go_to_definition",
                    "file": me.to_string_lossy(),
                    "line": 1,
                    "character": 1
                }),
            )
            .await;
        let msg = match outcome {
            ToolOutcome::Error { message } => message,
            other => panic!("expected Error, got {other:?}"),
        };
        assert!(
            msg.contains("No LSP server available") || msg.contains("cooldown"),
            "got {msg}"
        );
    }

    #[tokio::test]
    async fn cancellation_short_circuits_workspace_symbols() {
        let pool = rust_pool_no_binary();
        let tool = LspQuery::new(pool, PathGuard::default());
        let ctx = ToolContext {
            token: CancellationToken::new(),
            dry_run: false,
            task_spawner: None,
        };
        ctx.token.cancel();
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({"operation": "workspace_symbols", "query": "foo"}),
            )
            .await;
        // The pool has a configured language, so we enter the loop; the
        // token is checked before each query. Either we get Cancelled or
        // an Error from the failed binary — both are acceptable as long
        // as we don't get a Success with fake results.
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::Cancelled) | ToolOutcome::Error { .. }
            ),
            "got {outcome:?}"
        );
    }

    #[test]
    fn language_id_for_known_languages() {
        assert_eq!(language_id_for("rust"), "rust");
        assert_eq!(language_id_for("typescript"), "typescript");
        assert_eq!(language_id_for("python"), "python");
        assert_eq!(language_id_for("unknownlang"), "unknownlang");
    }

    #[test]
    fn short_path_strips_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let full = cwd.join("src/lib.rs").to_string_lossy().to_string();
        let short = short_path(&full);
        assert!(short.ends_with("src/lib.rs"));
        assert!(!short.starts_with('/'));
    }
}
