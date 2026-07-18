# ADR 018: LSP integration — kirkforge-lsp crate + lsp_query tool

## Status

Accepted (2026-07-19)

## Context

KirkForge-Cli is terminal-only today (`grep -rn 'lsp\|vscode\|jetbrains' src/` → 0 hits). The model has no symbol-aware way to navigate a codebase: every "find references" or "go to definition" falls back to `grep`/`glob` string matching, which misses overloads, macro expansions, cross-module callers, and type hierarchies. Claude Code and Vix both expose LSP-backed operations to the model, and this is the biggest remaining feature gap.

Vix's `brain/lsp/` package already proves the architecture: a per-language client pool (`pool.go`) over stdio JSON-RPC client (`client.go`), with a fail cooldown and a single `lsp_query` tool that dispatches to the seven operations the model actually needs (`go_to_definition`, `find_references`, `hover`, `document_symbols`, `find_implementations`, `workspace_symbols`, `diagnostics`).

## Decision

Port Vix's pool + client model into a vendored Rust workspace member, `crates/kirkforge-lsp/`, and expose it to the model through a single `lsp_query` tool.

### Architecture

1. **`crates/kirkforge-lsp/`** — a standalone crate (no dependency on the `kirkforge` binary crate) with `tokio`/`serde`/`serde_json`/`anyhow`/`tracing` as its only deps (all already workspace deps). Public API:
   - `LspClient` — one LSP server subprocess over stdio JSON-RPC. Mirrors the MCP-client pattern in `src/session/mcp_client/mod.rs`: `AtomicU64 next_id`, `pending: Arc<Mutex<HashMap<u64, oneshot::Sender<_>>>>`, a background read loop that routes responses by id, and process-group kill + reap on shutdown (`Drop` falls back to a best-effort synchronous kill). Handles LSP `Content-Length` framing (not newline-delimited like MCP). Caches `publishDiagnostics` per URI and wakes a `Notify` waiter so `wait_for_diagnostics` can return the cached payload on or after the notification.
   - `LspPool` — `language -> LspClient` map, lazy-started with a 30-second fail cooldown (`HashMap<String, Instant>`). `get_client(language)` returns `Ok(None)` for unconfigured languages and for cooldown windows; returns `Ok(Some(Arc<LspClient>>)` after a successful start + `initialize` handshake.
   - LSP type structs (`Location`, `Range`, `Position`, `Hover`, `DocumentSymbol`, `SymbolInformation`, `Diagnostic`) typed only where the tool layer needs them; the rest stays `serde_json::Value` for flexibility (matching Vix).

2. **Config seam: `[[lsp_servers]]` in `config.toml`.** Mirrors the existing `[[mcp_servers]]` pattern. New `LspServerEntry { language, extensions, command, args, env_vars }` in `src/shared/mod.rs`, with `lsp_servers: Vec<LspServerEntry>` defaulting to `vec![]` in `Config::default()`. The user configures their own `rust-analyzer`/`typescript-language-server`/`pyright` — KirkForge does not bundle any LSP server binary.

3. **`lsp_query` tool: `src/tools/lsp_query.rs`.** Follows the `Tool` trait pattern (`def()`/`run()`). Dispatches on an `operation` argument to the matching `LspPool`/`LspClient` method. File-based ops (`go_to_definition`, `find_references`, `hover`, `document_symbols`, `find_implementations`, `diagnostics`) follow Vix's `lspFileOperation` shape: resolve the file to an absolute path inside the sandbox (reuses `PathGuard::check_read`, same as `read_file`), `didOpen` → query → `didClose`. `workspace_symbols` queries across all configured languages. 10-second per-query timeout via `tokio::time::timeout` + `ctx.token` cancellation. Returns a clear `Error` outcome ("No LSP server configured for language X") when unconfigured — **never** fakes results (matches the `web_search` precedent).

4. **Wiring.** `all_tools()` gains an `lsp_pool: Option<Arc<LspPool>>` parameter. When `Some`, the `lsp_query` tool is pushed into the tools vec; when `None`, the tool is absent (same gating pattern as `supports_images` → `read_image`). `main/mod.rs` builds the `LspPool` from `Config::lsp_servers` at session start. Subagent/persona toolsets pass `None` (focused toolset; can be revisited later).

## Consequences

- **+1 workspace member** (`crates/kirkforge-lsp/`) picked up by the `members = ["crates/*"]` glob. No edit to the `members` line in the root `Cargo.toml`.
- **+1 optional dependency path** for the `kirkforge` binary crate (`kirkforge-lsp = { workspace = true }`). The crate itself is small (~5 deps, all already workspace deps).
- **+1 tool the model can call.** `lsp_query` is present only when at least one `[[lsp_servers]]` entry is configured, so existing users see no change.
- **No hard dependency on any specific LSP server binary.** The user configures their own; the crate spawns whatever `command` is in `config.toml`. Servers are subprocesses with the same lifecycle/timeout/reap pattern as the MCP client.
- **Binary size impact is small.** `tokio`/`serde`/`serde_json`/`anyhow`/`tracing` are already in the binary; `kirkforge-lsp` adds no new transitive deps.
- **LSP framing differs from MCP.** LSP uses `Content-Length: N\r\n\r\n<json>` framing rather than newline-delimited JSON. The reader loop accumulates headers, reads the declared body length, and parses one JSON-RPC message per body. This is the main wire-level difference from the MCP client.

## Alternatives considered

- **(a) Bundle a specific LSP server binary.** Rejected. LSP servers are per-language (rust-analyzer for Rust, typescript-language-server for TS, pyright for Python, …), each is heavy, and several have licenses that don't fit a static-link distribution. Bundling one would privilege one language and bloat the binary. The config-driven approach lets the user bring whatever servers they need.
- **(b) Wrap a workspace LSP server as an MCP server via stdio.** Rejected. MCP is tool-shaped (request → result), not textDocument-shaped. The LSP protocol has its own `initialize`/`didOpen`/`didClose`/`publishDiagnostics` lifecycle with server-pushed notifications and per-document version tracking, none of which fit cleanly into the MCP tool-call model. Forcing it through MCP would lose diagnostics-on-open, hover, and workspace symbols, and would require a fragile adapter shim.
- **(c) VS Code extension first.** Rejected as the first step. The TUI is the primary surface; an LSP-backed tool benefits the model in TUI mode immediately, and a VS Code extension can later wrap the TUI via PTY. Building the extension first would defer the model-facing benefit and couple the LSP work to the extension's release cadence.

## Test strategy

- `crates/kirkforge-lsp/` unit tests: a mock LSP server (python3 one-liner speaking JSON-RPC over stdio, skipped when python3 is absent) round-trips `initialize` + `definition`. `LspPool` lazy-start, `language_for_ext` normalization, and the 30-second fail cooldown are tested without a real server.
- `src/tools/lsp_query.rs` tests: schema validation (missing `operation` → `InvalidArgs`, unknown operation → `InvalidArgs`), the "no LSP configured" error path (empty pool → clear error, never a fake result), and op dispatch shape.
- The existing `adr_xref_drift` test (3 passed) remains green — this ADR is a native 3-digit CLI ADR and is not added to the plugin3 4-digit index in `docs/adr/README.md` (the test's `count_statuses` skips 3-digit files).