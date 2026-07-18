//! LSP (Language Server Protocol) client pool for KirkForge.
//!
//! Ports Vix's `brain/lsp/` pool + client model into KirkForge's Rust runtime.
//! Each `LspClient` wraps one language-server subprocess speaking JSON-RPC 2.0
//! over stdio. An `LspPool` owns a `language -> LspClient` map, lazily starts
//! servers on first use, applies a 30-second fail cooldown after a failed
//! start, and shuts them all down cleanly.
//!
//! # Architecture
//!
//! - `LspClient` mirrors `src/session/mcp_client/mod.rs`: a `next_id`
//!   counter, a `pending: HashMap<u64, oneshot::Sender<_>>` map, and a
//!   background read loop that routes responses by id. Notifications
//!   (`textDocument/didOpen`, `didClose`, publishDiagnostics) are handled
//!   in the same read loop.
//! - Process kill + reap reuses the same pattern as the MCP client: send
//!   `shutdown` → `exit`, close stdin, wait briefly for the reader, then
//!   kill the process group and reap. `Drop` falls back to a synchronous
//!   best-effort kill.
//! - `LspPool::get_client` is lazy and cooled down: a failed start
//!   remembers `Instant::now()` and refuses retries for 30 s.
//!
//! Wire format:
//!
//! LSP uses `Content-Length: N\r\n\r\n<json>` framing (not newline-delimited
//! like MCP). The reader loop accumulates headers, reads the declared body
//! length, and parses one JSON-RPC message per body.

// Production paths should avoid panicking on unexpected input. `unsafe` is
// confined to the process-group FFI (setpgid/killpg), mirroring
// `src/session/process_group.rs` in the binary crate.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex, Notify};

/// Time budget for the LSP `initialize` handshake.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

/// Time budget for a single JSON-RPC request/response round-trip.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Cooldown after a failed server start. A subsequent `get_client` call
/// during this window returns `Ok(None)` so the caller can surface a
/// "server is cooling down" error rather than hammering a broken binary.
pub const FAIL_COOLDOWN: Duration = Duration::from_secs(30);

/// Maximum bytes accepted in a single LSP message body. Larger bodies
/// are treated as a misbehaving server and disconnect the client.
const MAX_BODY_LEN: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// LSP wire types (typed only where the tool layer needs them; the rest is
// `serde_json::Value` for flexibility, matching Vix's approach).
// ---------------------------------------------------------------------------

/// A 0-based position in a text document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A half-open range `[start, end)` in a text document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A location: URI + range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// Hover result. `contents` is the rendered markdown/plain-text payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hover {
    pub contents: String,
}

/// One entry in a `textDocument/documentSymbol` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    /// LSP `SymbolKind` numeric value (1=File, 2=Module, …, 26=Class, …).
    pub kind: u32,
    pub range: Range,
    pub selection_range: Range,
    #[serde(default)]
    pub children: Vec<DocumentSymbol>,
}

/// One entry in a `workspace/symbol` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolInformation {
    pub name: String,
    pub kind: u32,
    pub location: Location,
}

/// One diagnostic entry from `textDocument/publishDiagnostics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    /// 1=Error, 2=Warning, 3=Info, 4=Hint. 0 = unknown.
    #[serde(default)]
    pub severity: u8,
    pub message: String,
    #[serde(default)]
    pub source: String,
}

// ---------------------------------------------------------------------------
// Config structs.
// ---------------------------------------------------------------------------

/// Configuration for launching a single LSP server subprocess.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Per-language configuration: name, file extensions, and optional LSP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageConfig {
    pub name: String,
    pub extensions: Vec<String>,
    pub lsp: Option<LspServerConfig>,
}

// ---------------------------------------------------------------------------
// LspClient — one language server subprocess over stdio JSON-RPC.
// ---------------------------------------------------------------------------

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value>>>>>;

/// Per-URI diagnostic waiters. The read loop wakes the matching waiter when
/// a `textDocument/publishDiagnostics` notification arrives for that URI.
type DiagWaiters = Arc<Mutex<HashMap<String, Arc<Notify>>>>;

/// One LSP server connection.
pub struct LspClient {
    language: String,
    root_dir: String,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    next_id: AtomicU64,
    pending: PendingMap,
    diag_waiters: DiagWaiters,
    /// Latest diagnostics per URI, kept so a `wait_for_diagnostics` call
    /// after the notification can still return the cached payload.
    diag_cache: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
    child: Arc<std::sync::Mutex<Option<Child>>>,
    alive: Arc<AtomicBool>,
    reader_shutdown_tx: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    stderr_shutdown_tx: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    reader_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    stderr_drain: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl LspClient {
    /// Spawn the LSP server subprocess and start the read loop.
    ///
    /// Does NOT perform the `initialize` handshake — call `initialize()`
    /// next. Splitting these lets the caller apply its own startup timeout
    /// around the handshake without double-spawning.
    pub async fn new(
        language: &str,
        root_dir: &str,
        command: &str,
        args: &[String],
    ) -> Result<Self> {
        Self::new_with_env(language, root_dir, command, args, &[]).await
    }

    /// Like [`new`][Self::new] but with extra environment variables.
    pub async fn new_with_env(
        language: &str,
        root_dir: &str,
        command: &str,
        args: &[String],
        env_vars: &[(&str, String)],
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        for (k, v) in env_vars {
            cmd.env(k, v);
        }
        cmd.current_dir(root_dir);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        setup_process_group(&mut cmd);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn LSP server '{command}'"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("LSP server '{command}' stdin not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("LSP server '{command}' stdout not piped"))?;
        let stderr = child.stderr.take();

        let alive = Arc::new(AtomicBool::new(true));
        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let next_id = AtomicU64::new(1);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let diag_waiters: DiagWaiters = Arc::new(Mutex::new(HashMap::new()));
        let diag_cache: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (reader_shutdown_tx, reader_shutdown_rx) = oneshot::channel();
        let (stderr_shutdown_tx, stderr_shutdown_rx) = oneshot::channel();

        let reader_task = spawn_reader_task(
            stdout,
            pending.clone(),
            diag_waiters.clone(),
            diag_cache.clone(),
            language.to_string(),
            alive.clone(),
            reader_shutdown_rx,
        );
        let stderr_drain = spawn_stderr_drain(stderr, stderr_shutdown_rx);

        Ok(Self {
            language: language.to_string(),
            root_dir: root_dir.to_string(),
            stdin,
            next_id,
            pending,
            diag_waiters,
            diag_cache,
            child: Arc::new(std::sync::Mutex::new(Some(child))),
            alive,
            reader_shutdown_tx: std::sync::Mutex::new(Some(reader_shutdown_tx)),
            stderr_shutdown_tx: std::sync::Mutex::new(Some(stderr_shutdown_tx)),
            reader_task: std::sync::Mutex::new(Some(reader_task)),
            stderr_drain: std::sync::Mutex::new(Some(stderr_drain)),
        })
    }

    /// LSP `initialize` + `initialized` handshake.
    pub async fn initialize(&self) -> Result<()> {
        let root_uri = path_to_uri(&self.root_dir);
        let init_params = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "definition": {},
                    "references": {},
                    "hover": {},
                    "implementation": {},
                    "publishDiagnostics": { "relatedInformation": true }
                },
                "workspace": {
                    "symbol": {}
                }
            }
        });
        let resp = match tokio::time::timeout(STARTUP_TIMEOUT, self.call("initialize", init_params))
            .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => bail!("LSP initialize failed: {e}"),
            Err(_) => bail!("LSP initialize timed out after {STARTUP_TIMEOUT:?}"),
        };
        if resp.get("result").is_none() {
            bail!("LSP initialize response missing result: {resp}");
        }
        self.notify("initialized", serde_json::json!({})).await?;
        Ok(())
    }

    /// Send a JSON-RPC request and return the `result` payload.
    pub async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        if !self.alive.load(Ordering::SeqCst) {
            bail!("LSP client for '{}' is not alive", self.language);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if let Err(e) = self.write_message(&req).await {
            self.pending.lock().await.remove(&id);
            bail!("failed to send LSP '{method}': {e}");
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(e))) => bail!("LSP '{method}' error: {e}"),
            Ok(Err(_)) => bail!("LSP '{method}' response channel closed"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("LSP '{method}' timed out after {REQUEST_TIMEOUT:?}")
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    pub async fn notify(&self, method: &str, params: serde_json::Value) -> Result<()> {
        if !self.alive.load(Ordering::SeqCst) {
            bail!("LSP client for '{}' is not alive", self.language);
        }
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&notif)
            .await
            .with_context(|| format!("failed to send LSP notification '{method}'"))
    }

    /// `textDocument/didOpen`.
    pub async fn did_open(&self, uri: &str, language_id: &str, text: &str) -> Result<()> {
        self.notify(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        )
        .await
    }

    /// `textDocument/didClose`.
    pub async fn did_close(&self, uri: &str) -> Result<()> {
        self.notify(
            "textDocument/didClose",
            serde_json::json!({
                "textDocument": { "uri": uri }
            }),
        )
        .await
    }

    /// `textDocument/definition`. Returns 0..N locations.
    pub async fn definition(&self, uri: &str, line: u32, character: u32) -> Result<Vec<Location>> {
        self.location_query("textDocument/definition", uri, line, character)
            .await
    }

    /// `textDocument/references`.
    pub async fn references(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        include_decl: bool,
    ) -> Result<Vec<Location>> {
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": include_decl }
        });
        let resp = self.call("textDocument/references", params).await?;
        Ok(parse_locations(&resp))
    }

    /// `textDocument/hover`. Returns `None` if the server has nothing to say.
    pub async fn hover(&self, uri: &str, line: u32, character: u32) -> Result<Option<Hover>> {
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });
        let resp = self.call("textDocument/hover", params).await?;
        let Some(result) = resp.get("result") else {
            return Ok(None);
        };
        if result.is_null() {
            return Ok(None);
        }
        let contents = render_hover_contents(result);
        if contents.trim().is_empty() {
            Ok(None)
        } else {
            Ok(Some(Hover { contents }))
        }
    }

    /// `textDocument/documentSymbol`.
    pub async fn document_symbol(&self, uri: &str) -> Result<Vec<DocumentSymbol>> {
        let params = serde_json::json!({
            "textDocument": { "uri": uri }
        });
        let resp = self
            .call("textDocument/documentSymbol", params)
            .await
            .context("documentSymbol failed")?;
        let Some(result) = resp.get("result") else {
            return Ok(vec![]);
        };
        Ok(parse_document_symbols(result))
    }

    /// `textDocument/implementation`.
    pub async fn implementation(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        self.location_query("textDocument/implementation", uri, line, character)
            .await
    }

    /// `workspace/symbol`.
    pub async fn workspace_symbols(&self, query: &str) -> Result<Vec<SymbolInformation>> {
        let params = serde_json::json!({ "query": query });
        let resp = self.call("workspace/symbol", params).await?;
        let Some(result) = resp.get("result") else {
            return Ok(vec![]);
        };
        let Some(arr) = result.as_array() else {
            return Ok(vec![]);
        };
        let mut out = Vec::with_capacity(arr.len());
        for v in arr {
            if let Some(sym) = parse_symbol_information(v) {
                out.push(sym);
            }
        }
        Ok(out)
    }

    /// Wait up to `timeout` for a `publishDiagnostics` notification for
    /// `uri`. Returns the cached diagnostics (possibly empty) on timeout.
    pub async fn wait_for_diagnostics(&self, uri: &str, timeout: Duration) -> Vec<Diagnostic> {
        let notify = {
            let mut waiters = self.diag_waiters.lock().await;
            waiters
                .entry(uri.to_string())
                .or_insert_with(|| Arc::new(Notify::new()))
                .clone()
        };
        // If diagnostics are already cached for this URI (e.g. a prior
        // wait returned them), return them immediately.
        {
            let cache = self.diag_cache.lock().await;
            if let Some(diags) = cache.get(uri) {
                if !diags.is_empty() {
                    return diags.clone();
                }
            }
        }
        let _ = tokio::time::timeout(timeout, notify.notified()).await;
        let cache = self.diag_cache.lock().await;
        cache.get(uri).cloned().unwrap_or_default()
    }

    /// `true` while the read loop is still running.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// The language this client serves (e.g. "rust").
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Send `shutdown` → `exit`, close stdin, kill + reap the child.
    pub async fn close(&self) {
        // Best-effort graceful shutdown. Ignore errors: a server that's
        // already dead will just drop the writes.
        let _ = self.call("shutdown", serde_json::json!(null)).await;
        let _ = self.notify("exit", serde_json::json!(null)).await;

        // Signal the background tasks to stop.
        if let Some(tx) = self.reader_shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.stderr_shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }

        // Close stdin so the server sees EOF.
        {
            let mut guard = self.stdin.lock().await;
            guard.take();
        }

        // Wait briefly for the background tasks to finish (best-effort).
        #[allow(unused_must_use)]
        {
            let reader = self.reader_task.lock().unwrap().take();
            let stderr = self.stderr_drain.lock().unwrap().take();
            if let Some(handle) = reader {
                tokio::time::timeout(Duration::from_secs(2), handle).await;
            }
            if let Some(handle) = stderr {
                tokio::time::timeout(Duration::from_secs(2), handle).await;
            }
        }

        self.alive.store(false, Ordering::SeqCst);

        // Kill + reap the child. The std::sync::Mutex guard must not
        // span an await point, so take the child handle first.
        let mut child_opt: Option<Child> = None;
        if let Ok(mut guard) = self.child.lock() {
            child_opt = guard.take();
        }
        if let Some(mut child) = child_opt {
            kill_process_group(&mut child);
            reap_child(&mut child, Duration::from_secs(2)).await;
        }
    }

    // --- internals -------------------------------------------------------

    /// Helper for `definition` / `implementation` — same request shape.
    async fn location_query(
        &self,
        method: &str,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });
        let resp = self.call(method, params).await?;
        Ok(parse_locations(&resp))
    }

    async fn write_message(&self, value: &serde_json::Value) -> Result<()> {
        let body = serde_json::to_string(value).context("failed to serialize LSP message")?;
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut guard = self.stdin.lock().await;
        let Some(ref mut stdin) = *guard else {
            bail!("LSP stdin closed");
        };
        stdin.write_all(frame.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Synchronous Drop cannot await. Signal the background tasks and
        // kill the child; reaping is best-effort via a detached task.
        if let Some(tx) = self.reader_shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.stderr_shutdown_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                kill_process_group(&mut child);
                if tokio::runtime::Handle::try_current().is_ok() {
                    // Detach: best-effort reap without awaiting.
                    drop(tokio::spawn(async move {
                        reap_child(&mut child, Duration::from_secs(2)).await;
                    }));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// LspPool — language -> LspClient map, lazy-started, fail-cooled.
// ---------------------------------------------------------------------------

/// One entry in the pool's internal map.
struct PoolEntry {
    client: Option<Arc<LspClient>>,
    failed_at: Option<Instant>,
}

/// A pool of LSP clients keyed by language name.
pub struct LspPool {
    root_dir: String,
    configs: HashMap<String, LanguageConfig>,
    ext_to_lang: HashMap<String, String>,
    entries: Mutex<HashMap<String, PoolEntry>>,
}

impl LspPool {
    /// Build a pool from a root directory and a list of language configs.
    pub fn new(root_dir: String, language_configs: Vec<LanguageConfig>) -> Self {
        let mut configs = HashMap::new();
        let mut ext_to_lang = HashMap::new();
        for cfg in language_configs {
            for ext in &cfg.extensions {
                ext_to_lang.insert(normalize_ext(ext), cfg.name.clone());
            }
            configs.insert(cfg.name.clone(), cfg);
        }
        Self {
            root_dir,
            configs,
            ext_to_lang,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Look up the language name for a file extension (e.g. ".rs" → "rust").
    pub fn language_for_ext(&self, ext: &str) -> Option<String> {
        self.ext_to_lang.get(&normalize_ext(ext)).cloned()
    }

    /// Languages with a configured LSP server.
    pub fn configured_languages(&self) -> Vec<String> {
        self.configs.keys().cloned().collect()
    }

    /// Get (or lazily start) the client for `language`.
    ///
    /// Returns `Ok(None)` when:
    /// - the language has no LSP server configured, or
    /// - the previous start failed and the cooldown has not elapsed.
    ///
    /// Returns `Err` only when the start attempt itself fails after the
    /// cooldown has elapsed (the caller can retry on the next turn).
    pub async fn get_client(&self, language: &str) -> Result<Option<Arc<LspClient>>> {
        let Some(cfg) = self.configs.get(language) else {
            return Ok(None);
        };
        let Some(lsp_cfg) = cfg.lsp.as_ref() else {
            return Ok(None);
        };
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get(language) {
            if let Some(client) = entry.client.as_ref() {
                if client.alive() {
                    return Ok(Some(client.clone()));
                }
                // Dead client — fall through to restart, but check cooldown.
                if let Some(failed_at) = entry.failed_at {
                    if failed_at.elapsed() < FAIL_COOLDOWN {
                        return Ok(None);
                    }
                }
            } else if let Some(failed_at) = entry.failed_at {
                if failed_at.elapsed() < FAIL_COOLDOWN {
                    return Ok(None);
                }
            }
        }
        // Try to start.
        let client =
            LspClient::new(language, &self.root_dir, &lsp_cfg.command, &lsp_cfg.args).await;
        match client {
            Ok(c) => {
                let arc = Arc::new(c);
                if let Err(e) = arc.initialize().await {
                    tracing::warn!(language, error = %e, "LSP initialize failed");
                    entries.insert(
                        language.to_string(),
                        PoolEntry {
                            client: None,
                            failed_at: Some(Instant::now()),
                        },
                    );
                    return Ok(None);
                }
                tracing::info!(language, "LSP server started");
                entries.insert(
                    language.to_string(),
                    PoolEntry {
                        client: Some(arc.clone()),
                        failed_at: None,
                    },
                );
                Ok(Some(arc))
            }
            Err(e) => {
                tracing::warn!(language, error = %e, "LSP spawn failed");
                entries.insert(
                    language.to_string(),
                    PoolEntry {
                        client: None,
                        failed_at: Some(Instant::now()),
                    },
                );
                Ok(None)
            }
        }
    }

    /// Shut down all clients.
    pub async fn shutdown(&self) {
        let mut entries = self.entries.lock().await;
        let clients: Vec<Arc<LspClient>> = entries
            .values_mut()
            .filter_map(|e| e.client.take())
            .collect();
        for client in clients {
            client.close().await;
        }
    }
}

// ---------------------------------------------------------------------------
// Reader loop + stderr drain (mirror mcp_client/mod.rs + spawn.rs).
// ---------------------------------------------------------------------------

fn spawn_reader_task(
    stdout: ChildStdout,
    pending: PendingMap,
    diag_waiters: DiagWaiters,
    diag_cache: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
    language: String,
    alive: Arc<AtomicBool>,
    mut shutdown: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut header_buf = String::new();
        loop {
            header_buf.clear();
            // Read headers until a blank line.
            let mut content_length: Option<usize> = None;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown => {
                        tracing::debug!(language = %language, "LSP reader shutting down");
                        alive.store(false, Ordering::SeqCst);
                        fail_all_pending(pending.clone()).await;
                        return;
                    }
                    result = reader.read_line(&mut header_buf) => {
                        match result {
                            Ok(0) => {
                                tracing::debug!(language = %language, "LSP stdout closed");
                                alive.store(false, Ordering::SeqCst);
                                fail_all_pending(pending.clone()).await;
                                return;
                            }
                            Ok(_) => {
                                let line = header_buf.trim_end_matches(['\r', '\n']);
                                if line.is_empty() {
                                    // End of headers.
                                    break;
                                }
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.trim().eq_ignore_ascii_case("Content-Length") {
                                        if let Ok(n) = v.trim().parse::<usize>() {
                                            content_length = Some(n);
                                        }
                                    }
                                }
                                header_buf.clear();
                            }
                            Err(e) => {
                                tracing::warn!(language = %language, error = %e, "LSP stdout read error");
                                alive.store(false, Ordering::SeqCst);
                                fail_all_pending(pending.clone()).await;
                                return;
                            }
                        }
                    }
                }
            }
            let Some(len) = content_length else {
                // No Content-Length — skip. Should not happen for LSP.
                continue;
            };
            if len > MAX_BODY_LEN {
                tracing::warn!(language = %language, len, "LSP body exceeded max; disconnecting");
                alive.store(false, Ordering::SeqCst);
                fail_all_pending(pending.clone()).await;
                return;
            }
            let mut body = vec![0u8; len];
            if let Err(e) = reader.read_exact(&mut body).await {
                tracing::warn!(language = %language, error = %e, "LSP body read error");
                alive.store(false, Ordering::SeqCst);
                fail_all_pending(pending.clone()).await;
                return;
            }
            let body_str = match std::str::from_utf8(&body) {
                Ok(s) => s,
                Err(_) => {
                    tracing::warn!(language = %language, "LSP body not UTF-8; skipping");
                    continue;
                }
            };
            let msg = match serde_json::from_str::<serde_json::Value>(body_str) {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(language = %language, error = %e, "LSP body not JSON; skipping");
                    continue;
                }
            };
            // Route: response (has id + result/error) or notification (no id).
            if let Some(id) = msg.get("id").and_then(|i| i.as_u64()) {
                // Response.
                let to_send = if let Some(err) = msg.get("error") {
                    let message = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(anyhow!("JSON-RPC error: {message}"))
                } else {
                    Ok(msg.clone())
                };
                let sender = {
                    let mut guard = pending.lock().await;
                    guard.remove(&id)
                };
                if let Some(sender) = sender {
                    let _ = sender.send(to_send);
                }
            } else if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                if method == "textDocument/publishDiagnostics" {
                    handle_publish_diagnostics(&msg, &diag_waiters, &diag_cache).await;
                }
                // Other notifications are ignored.
            }
        }
    })
}

async fn handle_publish_diagnostics(
    msg: &serde_json::Value,
    diag_waiters: &DiagWaiters,
    diag_cache: &Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
) {
    let Some(params) = msg.get("params") else {
        return;
    };
    let Some(uri) = params.get("uri").and_then(|u| u.as_str()) else {
        return;
    };
    let diags = params
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|d| serde_json::from_value::<Diagnostic>(d.clone()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    {
        let mut cache = diag_cache.lock().await;
        cache.insert(uri.to_string(), diags);
    }
    if let Some(notify) = diag_waiters.lock().await.get(uri).cloned() {
        notify.notify_one();
    }
}

async fn fail_all_pending(pending: PendingMap) {
    let waiters: Vec<_> = {
        let mut guard = pending.lock().await;
        guard.drain().map(|(_, tx)| tx).collect()
    };
    for tx in waiters {
        let _ = tx.send(Err(anyhow!("LSP client disconnected")));
    }
}

fn spawn_stderr_drain(
    stderr: Option<tokio::process::ChildStderr>,
    mut shutdown: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(stderr) = stderr else { return };
        let mut reader = tokio::io::BufReader::new(stderr);
        let mut buf = String::new();
        loop {
            buf.clear();
            tokio::select! {
                biased;
                _ = &mut shutdown => break,
                result = reader.read_line(&mut buf) => {
                    match result {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let line = buf.trim_end_matches(['\r', '\n']);
                            if !line.is_empty() {
                                tracing::debug!(target: "lsp_stderr", "{}", line);
                            }
                        }
                    }
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Parsers + helpers.
// ---------------------------------------------------------------------------

/// Parse a `result` that may be a single Location, an array of Locations,
/// or null/missing. Returns a flat `Vec<Location>`.
fn parse_locations(resp: &serde_json::Value) -> Vec<Location> {
    let Some(result) = resp.get("result") else {
        return vec![];
    };
    if result.is_null() {
        return vec![];
    }
    if let Some(arr) = result.as_array() {
        return arr
            .iter()
            .filter_map(|v| serde_json::from_value::<Location>(v.clone()).ok())
            .collect();
    }
    if let Ok(loc) = serde_json::from_value::<Location>(result.clone()) {
        return vec![loc];
    }
    vec![]
}

/// Parse a `documentSymbol` result. The result may be an array of
/// hierarchical `DocumentSymbol` or an array of flat `SymbolInformation`
/// (older servers). We handle both.
fn parse_document_symbols(result: &serde_json::Value) -> Vec<DocumentSymbol> {
    let Some(arr) = result.as_array() else {
        return vec![];
    };
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        if let Ok(sym) = serde_json::from_value::<DocumentSymbol>(v.clone()) {
            out.push(sym);
        } else if let Some(flat) = parse_symbol_information(v) {
            // Coerce flat SymbolInformation to a DocumentSymbol.
            let range = flat.location.range;
            out.push(DocumentSymbol {
                name: flat.name,
                kind: flat.kind,
                range: range.clone(),
                selection_range: range,
                children: vec![],
            });
        }
    }
    out
}

fn parse_symbol_information(v: &serde_json::Value) -> Option<SymbolInformation> {
    serde_json::from_value::<SymbolInformation>(v.clone()).ok()
}

/// Render LSP hover contents (which may be a string, a MarkupContent
/// object, or an array of MarkedString) into a single string.
fn render_hover_contents(result: &serde_json::Value) -> String {
    let Some(contents) = result.get("contents") else {
        return String::new();
    };
    if let Some(s) = contents.as_str() {
        return s.to_string();
    }
    if let Some(obj) = contents.as_object() {
        if let Some(value) = obj.get("value").and_then(|v| v.as_str()) {
            return value.to_string();
        }
    }
    if let Some(arr) = contents.as_array() {
        let mut parts = Vec::with_capacity(arr.len());
        for item in arr {
            if let Some(s) = item.as_str() {
                parts.push(s.to_string());
            } else if let Some(value) = item.get("value").and_then(|v| v.as_str()) {
                parts.push(value.to_string());
            }
        }
        return parts.join("\n\n");
    }
    contents.to_string()
}

/// Convert a filesystem path to a `file://` URI.
fn path_to_uri(path: &str) -> String {
    let p = PathBuf::from(path);
    let abs = if p.is_absolute() {
        p
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let s = abs.to_string_lossy();
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// Convert a `file://` URI back to a filesystem path.
///
/// Handles both the common `file:///abs/path` form and the rarer
/// `file://host/abs/path` form (the host segment is dropped, matching
/// rust-analyzer / VS Code behavior).
pub fn uri_to_path(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("file://") else {
        return uri.to_string();
    };
    // `file:///foo` → `rest = "/foo"` (starts with `/`, no host).
    // `file://localhost/foo` → `rest = "localhost/foo"`.
    if rest.starts_with('/') {
        return rest.to_string();
    }
    // Drop the host segment up to the next `/`.
    if let Some(idx) = rest.find('/') {
        return rest[idx..].to_string();
    }
    format!("/{rest}")
}

/// Normalize a file extension so `.rs` and `rs` map to the same key.
fn normalize_ext(ext: &str) -> String {
    let lower = ext.to_ascii_lowercase();
    if lower.starts_with('.') {
        lower
    } else {
        format!(".{lower}")
    }
}

// ---------------------------------------------------------------------------
// Process-group kill + reap (re-implemented here so this crate is standalone
// and doesn't need to depend on the kirkforge binary crate's internals).
// ---------------------------------------------------------------------------

#[cfg(unix)]
extern "C" {
    fn setpgid(pid: i32, pgid: i32) -> i32;
    fn killpg(pgrp: i32, sig: i32) -> i32;
}

#[cfg(unix)]
const SIGKILL: i32 = 9;

#[cfg(unix)]
fn setup_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.as_std_mut().pre_exec(|| {
            #[allow(unused_must_use)]
            {
                setpgid(0, 0);
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn setup_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
fn kill_process_group(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe {
            if killpg(pid as i32, SIGKILL) != 0 {
                tracing::warn!(pid, "failed to kill LSP process group");
            }
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(child: &mut Child) {
    if let Err(e) = child.start_kill() {
        tracing::warn!(error = %e, "failed to start killing LSP child");
    }
}

async fn reap_child(child: &mut Child, timeout: Duration) {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "failed to reap LSP child"),
        Err(_) => tracing::warn!("timed out waiting for LSP child to exit"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Stdio;

    /// Format a JSON-RPC message into the LSP `Content-Length` framing.
    fn frame(msg: &serde_json::Value) -> String {
        let body = serde_json::to_string(msg).unwrap();
        format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
    }

    /// A tiny mock LSP server speaking JSON-RPC over stdio. Responds to
    /// `initialize` and `textDocument/definition` with canned JSON; echoes
    /// `initialized` and `textDocument/didOpen`/`didClose` notifications
    /// by ignoring them.
    fn spawn_mock_lsp_server() -> Option<(
        std::process::Child,
        std::process::ChildStdin,
        std::process::ChildStdout,
    )> {
        // Skip the test entirely if python3 isn't on PATH — the mock
        // server is a python one-liner. This keeps the test hermetic on
        // machines without python3 without failing CI.
        if std::process::Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            return None;
        }
        let mut child = std::process::Command::new("python3")
            .arg("-u")
            .arg("-c")
            .arg(MOCK_SERVER_PY)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("python3 spawn");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Some((child, stdin, stdout))
    }

    const MOCK_SERVER_PY: &str = r#"
import sys, json

def read_msg(stdin):
    headers = {}
    while True:
        line = stdin.readline()
        if not line:
            return None
        if isinstance(line, bytes):
            line = line.decode('utf-8', 'replace')
        line = line.rstrip('\r\n')
        if line == '':
            break
        if ':' in line:
            k, v = line.split(':', 1)
            headers[k.strip().lower()] = v.strip()
    n = int(headers.get('content-length', '0'))
    body = stdin.read(n)
    if isinstance(body, bytes):
        body = body.decode('utf-8', 'replace')
    return json.loads(body) if body else None

def write_msg(stdout, obj):
    body = json.dumps(obj)
    frame = 'Content-Length: %d\r\n\r\n%s' % (len(body), body)
    stdout.write(frame)
    stdout.flush()

def main():
    while True:
        msg = read_msg(sys.stdin)
        if msg is None:
            break
        m = msg.get('method')
        if m == 'initialize':
            write_msg(sys.stdout, {
                'jsonrpc': '2.0',
                'id': msg.get('id'),
                'result': {'capabilities': {'textDocumentSync': 1}},
            })
        elif m == 'textDocument/definition':
            write_msg(sys.stdout, {
                'jsonrpc': '2.0',
                'id': msg.get('id'),
                'result': [{'uri': 'file:///foo.rs', 'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 5}}}],
            })
        elif m == 'shutdown' or m == 'exit':
            break
        # notifications are ignored

main()
"#;

    #[tokio::test]
    async fn client_new_initialize_definition_round_trip() {
        let Some((mut child, stdin, stdout)) = spawn_mock_lsp_server() else {
            eprintln!("skipping: python3 not available");
            return;
        };
        // Pipe the std handles into tokio.
        let tokio_cmd_stdin = tokio::process::ChildStdin::from_std(stdin).unwrap();
        let tokio_stdout = tokio::process::ChildStdout::from_std(stdout).unwrap();

        // Construct an LspClient directly from the pipes by reusing the
        // internal constructor shape. We can't call `new()` (it spawns),
        // so we replicate the wiring here.
        let alive = Arc::new(AtomicBool::new(true));
        let stdin_arc = Arc::new(Mutex::new(Some(tokio_cmd_stdin)));
        let next_id = AtomicU64::new(1);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let diag_waiters: DiagWaiters = Arc::new(Mutex::new(HashMap::new()));
        let diag_cache: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (r_tx, r_rx) = oneshot::channel();
        let (s_tx, s_rx) = oneshot::channel();
        let reader_task = spawn_reader_task(
            tokio_stdout,
            pending.clone(),
            diag_waiters.clone(),
            diag_cache.clone(),
            "mock".to_string(),
            alive.clone(),
            r_rx,
        );
        let stderr_drain = spawn_stderr_drain(None, s_rx);

        let client = LspClient {
            language: "mock".to_string(),
            root_dir: std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            stdin: stdin_arc,
            next_id,
            pending,
            diag_waiters,
            diag_cache,
            child: Arc::new(std::sync::Mutex::new(None)),
            alive,
            reader_shutdown_tx: std::sync::Mutex::new(Some(r_tx)),
            stderr_shutdown_tx: std::sync::Mutex::new(Some(s_tx)),
            reader_task: std::sync::Mutex::new(Some(reader_task)),
            stderr_drain: std::sync::Mutex::new(Some(stderr_drain)),
        };

        client.initialize().await.expect("initialize ok");
        let locs = client
            .definition("file:///foo.rs", 1, 2)
            .await
            .expect("definition ok");
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].uri, "file:///foo.rs");

        client.close().await;
        // Reap the python child.
        let _ = child.wait();
    }

    #[test]
    fn pool_language_for_ext_normalizes() {
        let pool = LspPool::new(
            "/tmp".to_string(),
            vec![LanguageConfig {
                name: "rust".to_string(),
                extensions: vec![".rs".to_string()],
                lsp: None,
            }],
        );
        assert_eq!(pool.language_for_ext(".rs"), Some("rust".to_string()));
        assert_eq!(pool.language_for_ext("rs"), Some("rust".to_string()));
        assert_eq!(pool.language_for_ext(".RS"), Some("rust".to_string()));
        assert_eq!(pool.language_for_ext(".py"), None);
    }

    #[test]
    fn pool_configured_languages_lists_all() {
        let pool = LspPool::new(
            "/tmp".to_string(),
            vec![
                LanguageConfig {
                    name: "rust".to_string(),
                    extensions: vec![".rs".to_string()],
                    lsp: None,
                },
                LanguageConfig {
                    name: "python".to_string(),
                    extensions: vec![".py".to_string()],
                    lsp: None,
                },
            ],
        );
        let mut langs = pool.configured_languages();
        langs.sort();
        assert_eq!(langs, vec!["python", "rust"]);
    }

    #[tokio::test]
    async fn pool_get_client_unconfigured_returns_none() {
        let pool = LspPool::new("/tmp".to_string(), vec![]);
        let res = pool.get_client("rust").await.expect("no error");
        assert!(res.is_none(), "unconfigured language should yield None");
    }

    #[tokio::test]
    async fn pool_get_client_no_lsp_configured_returns_none() {
        let pool = LspPool::new(
            "/tmp".to_string(),
            vec![LanguageConfig {
                name: "rust".to_string(),
                extensions: vec![".rs".to_string()],
                lsp: None,
            }],
        );
        let res = pool.get_client("rust").await.expect("no error");
        assert!(res.is_none(), "language with no lsp should yield None");
    }

    #[tokio::test]
    async fn pool_get_client_failed_start_cooldowns() {
        let pool = LspPool::new(
            "/tmp".to_string(),
            vec![LanguageConfig {
                name: "rust".to_string(),
                extensions: vec![".rs".to_string()],
                lsp: Some(LspServerConfig {
                    command: "/nonexistent/binary/xyzzy".to_string(),
                    args: vec![],
                }),
            }],
        );
        let res1 = pool.get_client("rust").await.expect("no error");
        assert!(res1.is_none(), "failed spawn yields None");
        let res2 = pool.get_client("rust").await.expect("no error");
        assert!(res2.is_none(), "cooldown yields None");
    }

    #[test]
    fn uri_to_path_round_trip() {
        assert_eq!(uri_to_path("file:///foo/bar.rs"), "/foo/bar.rs");
        assert_eq!(uri_to_path("not-a-uri"), "not-a-uri");
    }

    #[test]
    fn parse_locations_handles_array_and_single() {
        let arr = serde_json::json!({
            "result": [
                {"uri": "file:///a.rs", "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}}}
            ]
        });
        assert_eq!(parse_locations(&arr).len(), 1);

        let single = serde_json::json!({
            "result": {"uri": "file:///b.rs", "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 5}}}
        });
        assert_eq!(parse_locations(&single).len(), 1);

        let null = serde_json::json!({ "result": null });
        assert!(parse_locations(&null).is_empty());
    }

    #[test]
    fn render_hover_contents_handles_variants() {
        let str_case = serde_json::json!({ "contents": "hello" });
        assert_eq!(render_hover_contents(&str_case), "hello");

        let markup = serde_json::json!({ "contents": { "kind": "markdown", "value": "# H" } });
        assert_eq!(render_hover_contents(&markup), "# H");

        let arr =
            serde_json::json!({ "contents": ["a", { "language": "rust", "value": "fn x()" }] });
        assert_eq!(render_hover_contents(&arr), "a\n\nfn x()");
    }

    /// Sanity check the framing helper.
    #[test]
    fn frame_format_is_correct() {
        let msg = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
        let f = frame(&msg);
        assert!(f.starts_with("Content-Length: "));
        assert!(f.contains("\r\n\r\n"));
        assert!(f.contains("\"method\":\"ping\""));
    }

    /// Suppress unused-import warning for `Write` when the test module
    /// doesn't use it directly. Kept for future test scaffolding.
    #[allow(dead_code)]
    fn _write_use(_w: &mut impl Write) {}
}
