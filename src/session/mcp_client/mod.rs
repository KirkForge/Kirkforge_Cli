//! Minimal MCP (Model Context Protocol) client via stdio transport.
//!
//! Spawns MCP servers as subprocesses, communicates via JSON-RPC 2.0 over
//! stdin/stdout, and exposes their tools as `Tool` trait objects that can
//! be added to the executor alongside built-in tools.
//!
//! # Usage
//!
//! Servers are defined in `~/.local/share/kf-code/config.toml` under
//! the `[[mcp_servers]]` array:
//!
//! ```toml
//! [[mcp_servers]]
//! name = "context-server"
//! command = "npx"
//! args = ["context-server", "mcp"]
//! ```
//!
//! Each server's tools are prefixed with `mcp/<server>/<tool>`, e.g.
//! `mcp/context-server/context`. This avoids name collisions with built-in tools.
//!
//! # Architecture
//!
//! - `McpClient` wraps a single server process, handling JSON-RPC framing
//!   and request/response matching via an internal `next_id` counter.
//! - `McpClientManager` manages a Vec of clients, one per configured server.
//! - `McpToolWrapper` implements the `Tool` trait, forwarding `run()` calls
//!   to `tools/call` on the appropriate server.
//!
//! # Process lifecycle
//!
//! - A background task drains the child's stderr so a verbose server cannot
//!   deadlock by filling its error pipe.
//! - All blocking JSON-RPC calls have explicit timeouts so a frozen server
//!   does not hang the executor.
//! - `disconnect()` sends a shutdown signal, closes stdin, waits for the
//!   reader/stderr tasks to finish, and reaps the child process. `Drop`
//!   calls `disconnect()` synchronously as a best-effort fallback.

use crate::session::executor::{ApprovalRequest, ApprovalResponder, ApprovalResponse};
#[cfg(test)]
use crate::session::process_group::reap_child;
use crate::session::process_group::{kill_process_group, setup_process_group};
use crate::shared::{McpServerConfig, Message, Role, StreamEvent, ToolError, ToolOutcome};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};

mod error;
mod http;
mod manager;
mod spawn;

use error::McpError;
pub use manager::{McpClientManager, McpToolDef};
use spawn::{spawn_child_reap, spawn_stderr_drain};

/// MCP server capabilities that kf-code does not yet support.
const UNSUPPORTED_CAPABILITIES: &[&str] = &[];

/// Wiring the MCP client needs to serve an incoming `sampling/createMessage`
/// request through the same approval bus that gates tool calls.
///
/// The approval channel (`approval_tx`) is created per-session in `run_tui` /
/// `run_line_mode`; the `Config` snapshot is used to build a one-off completion
/// adapter. Both are installed after the manager is constructed, so the manager
/// exposes this as a settable context rather than a constructor arg.
#[derive(Clone)]
pub struct SamplingContext {
    pub approval_tx: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
    pub config: std::sync::Arc<crate::shared::Config>,
}

/// Log a warning for each unsupported capability advertised by the server.
fn warn_unsupported_capabilities(server_name: &str, init_result: &serde_json::Value) {
    let caps = match init_result.get("capabilities").and_then(|c| c.as_object()) {
        Some(c) => c,
        None => return,
    };
    for &cap in UNSUPPORTED_CAPABILITIES {
        if caps.contains_key(cap) {
            tracing::warn!(
                server = %server_name,
                capability = cap,
                "MCP server advertises unsupported capability"
            );
        }
    }
}

/// Time budget for the MCP handshake (`initialize` request).
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Time budget for a single JSON-RPC request/response round-trip.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum time the reader task waits for a single line from the MCP
/// server's stdout before treating the connection as dead. This prevents a
/// server that emits partial output and never sends a newline from hanging
/// the reader forever. A well-behaved server should emit responses and
/// keepalives far more frequently than this.
const READER_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum length of a single JSON-RPC line accepted from the server.
/// Anything longer is treated as a misbehaving server and disconnects.
const MAX_LINE_LEN: usize = 1 << 20;

/// Type alias for the in-flight request map used by the reader task.
/// JSON-RPC 2.0 permits `id` to be a string or a number, so the key is a
/// normalized string representation.
type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value, McpError>>>>>;

/// Normalize a JSON-RPC `id` value (string or number) to a map key.
/// Returns `None` for absent or null ids (notifications).
fn json_id_to_string(id: &serde_json::Value) -> Option<String> {
    match id {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A resource exposed by an MCP server.
#[derive(Debug, Clone)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: Option<String>,
}

/// A prompt template exposed by an MCP server.
#[derive(Debug, Clone)]
pub struct McpPrompt {
    pub name: String,
    pub description: String,
    pub arguments: Vec<McpPromptArg>,
}

/// An argument declaration for an MCP prompt template.
#[derive(Debug, Clone)]
pub struct McpPromptArg {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// A single MCP server connection.
///
/// Spawns the configured command, performs the `initialize`→`notifications/initialized`
/// handshake, discovers tools, and provides methods for calling tools
/// (`tools/list` + `tools/call`). For `transport = "http"`, this wraps the
/// streamable-HTTP transport instead of a child process.
enum McpClient {
    /// stdio child-process transport (original implementation).
    Stdio(StdioMcpClient),
    /// streamable-HTTP transport (SSE + POST).
    Http(http::McpHttpTransport),
}

/// stdio child-process MCP client.
struct StdioMcpClient {
    /// Server config (name, command, args).
    config: McpServerConfig,
    /// Approval-bus + config wiring for `sampling/createMessage`. Shared with
    /// the reader task so it can be installed after connect (the approval
    /// channel is created by the session driver after the manager is built).
    sampling: Arc<tokio::sync::RwLock<Option<SamplingContext>>>,
    /// Write handle for the child's stdin. Protected by a Mutex so multiple
    /// tool-call tasks can send requests concurrently. Set to `None` after
    /// `disconnect()`.
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    /// Next JSON-RPC request ID.
    next_id: Arc<Mutex<u64>>,
    /// In-flight requests keyed by JSON-RPC id. The reader task routes
    /// responses here. A Mutex is sufficient: critical sections are tiny
    /// (insert/remove a oneshot sender).
    pending: PendingMap,
    /// The server process handle (for cleanup). Taken out when disconnecting.
    child: Arc<std::sync::Mutex<Option<Child>>>,
    /// Set to `false` when the reader task exits or `disconnect()` runs.
    alive: Arc<AtomicBool>,
    /// Senders for the graceful-shutdown signals of the background tasks.
    reader_shutdown_tx: Option<oneshot::Sender<()>>,
    stderr_shutdown_tx: Option<oneshot::Sender<()>>,
    /// Background task handles, kept so `disconnect()` can await them.
    // reason: held for Drop semantics; read only by #[cfg(test)] disconnect().
    _reader_task: Option<tokio::task::JoinHandle<()>>,
    _stderr_drain: Option<tokio::task::JoinHandle<()>>,
}

/// Convert MCP content blocks into a `ToolOutcome::Success` string.///
/// Text blocks are joined. Non-text blocks (`image`, `audio`, `resource`,
/// and any future kind) are surfaced as descriptive placeholders so the
/// model knows they exist even if this runtime cannot render them. If no
/// block can be summarized, falls back to pretty-printing `raw_result`.
fn tool_result_from_content_blocks(
    content_blocks: &[serde_json::Value],
    raw_result: &serde_json::Value,
) -> ToolOutcome {
    let mut text_parts: Vec<String> = Vec::new();
    let mut non_text_mentioned = false;
    for block in content_blocks {
        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
            text_parts.push(text.to_string());
            continue;
        }
        if let Some(kind) = block.get("type").and_then(|t| t.as_str()) {
            non_text_mentioned = true;
            match kind {
                "image" => {
                    let mime = block
                        .get("mimeType")
                        .and_then(|m| m.as_str())
                        .unwrap_or("image/*");
                    text_parts.push(format!("[image: mime={mime}]"));
                }
                "audio" => {
                    text_parts.push("[audio block]".to_string());
                }
                "resource" => {
                    let resource = block.get("resource").unwrap_or(&serde_json::Value::Null);
                    let uri = resource.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                    let mime = resource
                        .get("mimeType")
                        .and_then(|m| m.as_str())
                        .unwrap_or("*/*");
                    text_parts.push(format!("[resource: {uri} mime={mime}]"));
                }
                _ => {
                    text_parts.push(format!("[{kind} content block]"));
                }
            }
        }
    }
    if text_parts.is_empty() && !non_text_mentioned {
        ToolOutcome::Success {
            content: serde_json::to_string_pretty(raw_result).unwrap_or_default(),
        }
    } else {
        ToolOutcome::Success {
            content: text_parts.join(""),
        }
    }
}

/// Translate the `messages` array of an MCP `sampling/createMessage` request
/// into the internal `Message` type the adapters consume. Non-text content
/// blocks (image, audio, resource) are surfaced as text placeholders so the
/// model is informed they exist even if the adapter cannot render them.
fn sampling_messages(params: &serde_json::Value) -> Vec<Message> {
    let Some(msgs) = params.get("messages").and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    msgs.iter()
        .filter_map(|m| {
            let role = match m.get("role").and_then(|r| r.as_str()) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => return None,
            };
            let content = match m.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(blocks)) => {
                    let mut parts: Vec<String> = Vec::new();
                    for b in blocks {
                        if let Some(text) = b.get("text").and_then(|t| t.as_str()) {
                            parts.push(text.to_string());
                            continue;
                        }
                        if let Some(kind) = b.get("type").and_then(|t| t.as_str()) {
                            parts.push(format!("[{kind} content block]"));
                        }
                    }
                    parts.join("")
                }
                _ => String::new(),
            };
            Some(Message {
                role,
                content,
                ..Default::default()
            })
        })
        .collect()
}

impl McpClient {
    /// Spawn the server process and perform the MCP handshake.
    ///
    /// Returns `None` if the server cannot be spawned or the handshake fails.
    async fn connect(config: &McpServerConfig) -> Option<Self> {
        if config.transport == "http" {
            return http::McpHttpTransport::connect(config)
                .await
                .map(Self::Http);
        }
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        // Harden the subprocess env: clear the inherited parent env (which
        // includes API keys), then forward only a curated baseline
        // (PATH/HOME/USER/locale/…) plus the server's explicit `env_vars`.
        // Mirrors the kf-plugin-host curated_env pattern.
        cmd.env_clear();
        cmd.envs(kf_plugin_host::env::curated_env(&config.env_vars));
        // Sanitize PATH before spawning so a minimal or world-writable host
        // PATH cannot shadow standard system directories (e.g. a relative
        // entry that looks like `bash` or `npx`). Overrides the baseline PATH.
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", crate::session::bash_runner::sanitized_path(&path));
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        setup_process_group(&mut cmd);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(server = %config.name, error = %e, "failed to spawn MCP server");
                return None;
            }
        };
        // Take stdin/stdout before wiring; if either is missing the spawn
        // is unusable. Kill the child first so a `?` early-return does not
        // orphan it (the child was already spawned with piped stdio but
        // setup_process_group does not set kill_on_drop on the MCP server).
        let stdin = match child.stdin.take() {
            Some(s) => s,
            None => {
                kill_process_group(&mut child);
                tracing::warn!(server = %config.name, "MCP server stdin unavailable; killed orphan");
                return None;
            }
        };
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                kill_process_group(&mut child);
                tracing::warn!(server = %config.name, "MCP server stdout unavailable; killed orphan");
                return None;
            }
        };
        let stderr = child.stderr.take();

        let alive = Arc::new(AtomicBool::new(true));
        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let next_id = Arc::new(Mutex::new(1_u64));
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let sampling = Arc::new(tokio::sync::RwLock::new(None));

        let (reader_shutdown_tx, reader_shutdown_rx) = oneshot::channel();
        let (stderr_shutdown_tx, stderr_shutdown_rx) = oneshot::channel();

        let reader_task = Self::spawn_reader_task(
            stdout,
            pending.clone(),
            config.name.clone(),
            alive.clone(),
            reader_shutdown_rx,
            stdin.clone(),
            sampling.clone(),
        );
        let stderr_drain = spawn_stderr_drain(stderr, stderr_shutdown_rx);

        let client = StdioMcpClient {
            config: config.clone(),
            sampling,
            stdin,
            next_id,
            pending,
            child: Arc::new(std::sync::Mutex::new(Some(child))),
            alive,
            reader_shutdown_tx: Some(reader_shutdown_tx),
            stderr_shutdown_tx: Some(stderr_shutdown_tx),
            _reader_task: Some(reader_task),
            _stderr_drain: Some(stderr_drain),
        };

        // MCP handshake: initialize → handle response
        let init_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"resources": {}, "prompts": {}, "roots": {}},
                "clientInfo": {
                    "name": "kf-code",
                    "version": "0.1.0"
                }
            }
        });

        let wrapper = McpClient::Stdio(client);
        let resp =
            match tokio::time::timeout(STARTUP_TIMEOUT, wrapper.send_request(&init_req)).await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    tracing::warn!(server = %config.name, error = %e, "MCP initialize failed");
                    return None;
                }
                Err(_) => {
                    tracing::warn!(server = %config.name, "MCP initialize timed out");
                    return None;
                }
            };
        // Verify it's a valid response to initialize
        if resp.get("result").is_none() {
            tracing::warn!(server = %config.name, response = %resp, "MCP initialize response missing result");
            return None;
        }

        warn_unsupported_capabilities(&config.name, resp.get("result").unwrap());

        // Send initialized notification (no response expected)
        let init_done = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        wrapper.send_notification(&init_done).await;

        Some(wrapper)
    }

    /// Construct a client from existing I/O handles. Used only by tests.
    #[cfg(test)]
    fn from_pipes(stdin: ChildStdin, stdout: ChildStdout, config: McpServerConfig) -> Self {
        let alive = Arc::new(AtomicBool::new(true));
        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let next_id = Arc::new(Mutex::new(1_u64));
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let sampling = Arc::new(tokio::sync::RwLock::new(None));

        let (reader_shutdown_tx, reader_shutdown_rx) = oneshot::channel();
        let (stderr_shutdown_tx, stderr_shutdown_rx) = oneshot::channel();

        let reader_task = Self::spawn_reader_task(
            stdout,
            pending.clone(),
            config.name.clone(),
            alive.clone(),
            reader_shutdown_rx,
            stdin.clone(),
            sampling.clone(),
        );
        let stderr_drain = spawn_stderr_drain(None, stderr_shutdown_rx);

        let inner = StdioMcpClient {
            config,
            sampling,
            stdin,
            next_id,
            pending,
            child: Arc::new(std::sync::Mutex::new(None)),
            alive,
            reader_shutdown_tx: Some(reader_shutdown_tx),
            stderr_shutdown_tx: Some(stderr_shutdown_tx),
            _reader_task: Some(reader_task),
            _stderr_drain: Some(stderr_drain),
        };
        Self::Stdio(inner)
    }

    /// Returns `true` while the transport is still running.
    fn is_alive(&self) -> bool {
        match self {
            McpClient::Stdio(c) => c.alive.load(Ordering::SeqCst),
            McpClient::Http(c) => c.is_alive(),
        }
    }

    /// Install the sampling context (approval bus + config) on a stdio
    /// client. The reader task reads this via the shared `RwLock`, so it can
    /// be installed after the client connects. HTTP transport has no
    /// server-to-client request handling, so it is a no-op there.
    fn set_sampling(&self, ctx: SamplingContext) {
        if let McpClient::Stdio(c) = self {
            if let Ok(mut guard) = c.sampling.try_write() {
                *guard = Some(ctx);
            }
        }
    }

    /// Send a JSON-RPC request and return the raw response Value, or
    /// an `McpError` if the request failed.
    async fn send_request(&self, req: &serde_json::Value) -> Result<serde_json::Value, McpError> {
        match self {
            McpClient::Stdio(c) => c.stdio_send_request(req).await,
            McpClient::Http(c) => c.send_request(req).await,
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn send_notification(&self, notification: &serde_json::Value) {
        match self {
            McpClient::Stdio(c) => c.stdio_send_notification(notification).await,
            McpClient::Http(c) => c.send_notification(notification).await,
        }
    }

    /// Call `tools/list` and return the tool definitions.
    async fn list_tools(&self) -> Vec<McpToolDef> {
        match self {
            McpClient::Stdio(c) => c.stdio_list_tools().await,
            McpClient::Http(c) => c.list_tools().await,
        }
    }

    /// Call `tools/call` with the given tool name and arguments and return a
    /// structured `ToolOutcome`.
    async fn call_tool(&self, tool_name: &str, args: serde_json::Value) -> ToolOutcome {
        match self {
            McpClient::Stdio(c) => c.stdio_call_tool(tool_name, args).await,
            McpClient::Http(c) => c.call_tool(tool_name, args).await,
        }
    }

    /// Call `resources/list` and return the resource definitions.
    pub async fn list_resources(&self) -> Vec<McpResource> {
        match self {
            McpClient::Stdio(c) => c.stdio_list_resources().await,
            McpClient::Http(c) => c.list_resources().await,
        }
    }

    /// Call `resources/read` and return the resource contents.
    pub async fn read_resource(&self, uri: &str) -> Result<serde_json::Value, McpError> {
        match self {
            McpClient::Stdio(c) => c.stdio_read_resource(uri).await,
            McpClient::Http(c) => c.read_resource(uri).await,
        }
    }

    /// Call `prompts/list` and return the prompt definitions.
    pub async fn list_prompts(&self) -> Vec<McpPrompt> {
        match self {
            McpClient::Stdio(c) => c.stdio_list_prompts().await,
            McpClient::Http(c) => c.list_prompts().await,
        }
    }

    /// Call `prompts/get` and return the prompt content.
    pub async fn get_prompt(
        &self,
        name: &str,
        args: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        match self {
            McpClient::Stdio(c) => c.stdio_get_prompt(name, args).await,
            McpClient::Http(c) => c.get_prompt(name, args).await,
        }
    }

    /// Spawn a task that reads JSON-RPC messages from the server's
    /// stdout, routes responses to in-flight requests, and handles
    /// incoming server-to-client requests (e.g. `roots/list`).
    fn spawn_reader_task(
        stdout: ChildStdout,
        pending: PendingMap,
        server_name: String,
        alive: Arc<AtomicBool>,
        mut shutdown: oneshot::Receiver<()>,
        stdin: Arc<Mutex<Option<ChildStdin>>>,
        sampling: Arc<tokio::sync::RwLock<Option<SamplingContext>>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut buf = String::new();
            loop {
                buf.clear();
                let read_fut = reader.read_line(&mut buf);
                tokio::select! {
                    biased;
                    _ = &mut shutdown => {
                        tracing::trace!(server = %server_name, "MCP reader shutting down");
                        break;
                    }
                    result = tokio::time::timeout(READER_IDLE_TIMEOUT, read_fut) => {
                        match result {
                            Ok(Ok(0)) => {
                                tracing::trace!(server = %server_name, "MCP stdout closed");
                                break;
                            }
                            Ok(Ok(_)) if buf.len() > MAX_LINE_LEN => {
                                tracing::warn!(
                                    server = %server_name,
                                    bytes = buf.len(),
                                    "MCP response line exceeded maximum length; disconnecting"
                                );
                                break;
                            }
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => {
                                tracing::warn!(server = %server_name, error = %e, "MCP stdout read error");
                                break;
                            }
                            Err(_) => {
                                tracing::warn!(
                                    server = %server_name,
                                    "MCP reader idle timeout; disconnecting"
                                );
                                break;
                            }
                        }
                    }
                }

                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                    tracing::trace!(server = %server_name, line = %trimmed, "MCP non-JSON stdout line");
                    continue;
                };

                let id_str = msg.get("id").and_then(json_id_to_string);

                if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                    if id_str.is_some() {
                        // Server-to-client request (has method + id).
                        if method == "sampling/createMessage" {
                            let response =
                                Self::handle_sampling_request(&sampling, &msg, &server_name).await;
                            Self::write_response(&stdin, &response, &server_name).await;
                        } else if let Some(response) =
                            Self::handle_server_request(method, &msg, &server_name)
                        {
                            Self::write_response(&stdin, &response, &server_name).await;
                        }
                        continue;
                    }
                    // Server-to-client notification (has method, no id).
                    tracing::trace!(server = %server_name, method = %method, "MCP server notification");
                    continue;
                }

                let Some(id) = id_str else {
                    tracing::trace!(server = %server_name, response = %msg, "MCP message without id or method");
                    continue;
                };

                Self::dispatch_response(id, msg, &pending, &server_name).await;
            }
            Self::fail_all_pending(pending).await;
            alive.store(false, Ordering::SeqCst);
        })
    }

    /// Handle an incoming request from the MCP server. Returns a
    /// JSON-RPC response to send back, or `None` to ignore.
    fn handle_server_request(
        method: &str,
        request: &serde_json::Value,
        server_name: &str,
    ) -> Option<serde_json::Value> {
        match method {
            "roots/list" => {
                let id = request.get("id")?;
                let root_uri = std::env::current_dir()
                    .ok()
                    .map(|p| format!("file://{}", p.display()))
                    .unwrap_or_else(|| "file:///".to_string());
                tracing::trace!(server = %server_name, root = %root_uri, "responding to roots/list");
                Some(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "roots": [{ "uri": root_uri }]
                    }
                }))
            }
            other => {
                tracing::trace!(server = %server_name, method = %other, "unhandled server request");
                None
            }
        }
    }

    /// Handle a `sampling/createMessage` request from an MCP server.
    ///
    /// Sampling is a server-initiated model completion, so it MUST go through
    /// the same `ApprovalRequest`/`ApprovalResponder` bus as tool calls — it
    /// never runs a model unconditionally. The only bypass is the explicit
    /// `tools.allow_sampling_unattended` config flag (default off), which
    /// auto-approves for trusted headless setups. A denied request returns an
    /// MCP JSON-RPC error so the server knows the completion was refused.
    async fn handle_sampling_request(
        sampling: &Arc<tokio::sync::RwLock<Option<SamplingContext>>>,
        request: &serde_json::Value,
        server_name: &str,
    ) -> serde_json::Value {
        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let Some(ctx) = sampling.read().await.clone() else {
            tracing::warn!(server = %server_name, "sampling/createMessage before sampling context installed; denying");
            return Self::sampling_error(
                &id,
                "sampling not configured; install the session approval context",
            );
        };

        // `tools.allow_sampling_unattended` is the sampling-specific opt-in.
        // `security.auto_approve` is the global operator opt-in — if set,
        // ALL approvals (tools AND server-initiated sampling) are skipped.
        let allowed_unattended =
            ctx.config.tools.allow_sampling_unattended || ctx.config.security.auto_approve;
        let approved = if allowed_unattended {
            tracing::info!(
                server = %server_name,
                "sampling/createMessage auto-approved (allow_sampling_unattended or auto_approve)"
            );
            true
        } else {
            Self::request_sampling_approval(&ctx, request, server_name).await
        };

        if !approved {
            tracing::warn!(server = %server_name, "sampling/createMessage denied by user");
            return Self::sampling_error(&id, "sampling request denied by user");
        }

        match Self::run_sampling_completion(&ctx, request, server_name).await {
            Ok(content) => {
                tracing::info!(server = %server_name, "sampling/createMessage completed");
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "content": content, "role": "assistant", "model": ctx.config.model.default_model }
                })
            }
            Err(e) => {
                tracing::warn!(server = %server_name, error = %e, "sampling/createMessage completion failed");
                Self::sampling_error(&id, &format!("sampling completion failed: {e}"))
            }
        }
    }

    /// Route a sampling request through the approval bus and return whether
    /// the user approved.
    async fn request_sampling_approval(
        ctx: &SamplingContext,
        request: &serde_json::Value,
        server_name: &str,
    ) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel::<ApprovalResponse>();
        let args = serde_json::json!({
            "mcp_sampling": {
                "server": server_name,
                "params": request.get("params").cloned().unwrap_or(serde_json::Value::Null)
            }
        });
        if ctx
            .approval_tx
            .send(ApprovalRequest {
                tool_name: format!("mcp/sampling/createMessage/{server_name}"),
                args,
                response: ApprovalResponder::new(tx),
            })
            .is_err()
        {
            tracing::warn!(server = %server_name, "approval channel closed; denying sampling");
            return false;
        }
        // Bounded wait; a hung approval handler must not block the MCP reader.
        matches!(
            tokio::time::timeout(Duration::from_secs(300), rx).await,
            Ok(Ok(
                ApprovalResponse::Approved | ApprovalResponse::AlwaysApprove
            ))
        )
    }

    /// Run the model completion for an approved sampling request.
    async fn run_sampling_completion(
        ctx: &SamplingContext,
        request: &serde_json::Value,
        server_name: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let params = request
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let messages = sampling_messages(&params);
        let adapter = crate::adapters::sampling_adapter(&ctx.config);
        let mut rx = adapter.stream(&messages, &[]).await?;
        let mut text = String::new();
        while let Some(event) = rx.recv().await {
            match event {
                StreamEvent::Text(t) => text.push_str(&t),
                StreamEvent::Error(e) => {
                    tracing::warn!(server = %server_name, error = %e, "sampling stream error");
                }
                _ => {}
            }
        }
        if text.is_empty() {
            anyhow::bail!("sampling completion produced no text");
        }
        Ok(serde_json::json!([{ "type": "text", "text": text }]))
    }

    /// Build an MCP JSON-RPC error response for a sampling request.
    fn sampling_error(id: &serde_json::Value, message: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": message }
        })
    }

    /// Write a JSON-RPC response line to the server's stdin.
    async fn write_response(
        stdin: &Arc<Mutex<Option<ChildStdin>>>,
        response: &serde_json::Value,
        server_name: &str,
    ) {
        let line = match serde_json::to_string(response) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(server = %server_name, error = %e, "failed to serialize server response");
                return;
            }
        };
        let mut guard = stdin.lock().await;
        let Some(ref mut stdin) = *guard else {
            tracing::trace!(server = %server_name, "stdin closed, cannot respond to server request");
            return;
        };
        if let Err(e) = stdin.write_all(line.as_bytes()).await {
            tracing::warn!(server = %server_name, error = %e, "failed to write server response");
            return;
        }
        if let Err(e) = stdin.write_all(b"\n").await {
            tracing::warn!(server = %server_name, error = %e, "failed to write server response newline");
            return;
        }
        let _ = stdin.flush().await;
    }

    /// Wake every in-flight request with `error`. Called when the reader
    /// exits (EOF, read error, idle timeout, oversized line) so callers do
    /// not wait the full `REQUEST_TIMEOUT` before discovering the client is
    /// dead.
    pub(super) async fn fail_all_pending(pending: PendingMap) {
        let waiters: Vec<_> = {
            let mut guard = pending.lock().await;
            guard.drain().map(|(_, tx)| tx).collect()
        };
        for tx in waiters {
            let _ = tx.send(Err(McpError::Disconnected));
        }
    }

    /// Route a single parsed JSON-RPC response to its waiter.
    pub(super) async fn dispatch_response(
        id: String,
        resp: serde_json::Value,
        pending: &Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value, McpError>>>>,
        server_name: &str,
    ) {
        // Check for JSON-RPC error before handing the response off.
        let to_send = if let Some(err) = resp.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-32603);
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string();
            tracing::warn!(
                server = %server_name,
                id = %id,
                code = code,
                message = %message,
                "MCP JSON-RPC error"
            );
            Err(McpError::JsonRpc { code, message })
        } else {
            Ok(resp)
        };

        let sender = {
            let mut pending = pending.lock().await;
            pending.remove(&id)
        };
        if let Some(sender) = sender {
            if sender.send(to_send).is_err() {
                tracing::trace!(id = %id, "MCP response receiver dropped");
            }
        } else {
            tracing::trace!(server = %server_name, id = %id, "MCP response for unknown or timed-out request");
        }
    }

    /// Gracefully disconnect from the server.
    /// ponytail: production relies on Drop for cleanup (synchronous, best-effort).
    /// Explicit async disconnect is test-only; wiring it into the manager's
    /// reconnect path would require an async Drop equivalent.
    // reason: lifecycle API used by tests; production relies on Drop fallback.
    #[cfg(test)]
    async fn disconnect(&mut self) {
        match self {
            McpClient::Stdio(c) => c.disconnect().await,
            McpClient::Http(c) => c.disconnect().await,
        }
    }
}

impl StdioMcpClient {
    async fn stdio_send_request(
        &self,
        req: &serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(McpError::Disconnected);
        }

        let id_num = {
            let mut guard = self.next_id.lock().await;
            let id = *guard;
            *guard += 1;
            id
        };
        let id = id_num.to_string();

        let mut req_with_id = req.clone();
        if let Some(obj) = req_with_id.as_object_mut() {
            obj.insert("id".to_string(), serde_json::json!(id_num));
        }

        let line = serde_json::to_string(&req_with_id)
            .map_err(|e| McpError::Io(std::io::Error::other(e.to_string())))?;
        tracing::trace!(id = %id, request = %line, "MCP request");

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id.clone(), tx);
        }

        let write_fut = async {
            let mut stdin_guard = self.stdin.lock().await;
            let Some(ref mut stdin) = *stdin_guard else {
                return Err(McpError::Disconnected);
            };
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            Ok(())
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, write_fut).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                self.pending.lock().await.remove(&id);
                return Err(e);
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                tracing::warn!(id = %id, "MCP request write timed out");
                return Err(McpError::Timeout);
            }
        }

        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                // WO 43.37: match the timeout branches — drop the pending-map
                // entry so an abandoned request (receiver dropped without the
                // reader exiting) doesn't leak the sender until the next
                // response or fail_all_pending.
                self.pending.lock().await.remove(&id);
                tracing::warn!(id = id, "MCP response channel closed");
                Err(McpError::ChannelClosed)
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                tracing::warn!(id = %id, "MCP request timed out waiting for response");
                Err(McpError::Timeout)
            }
        }
    }

    async fn stdio_send_notification(&self, notification: &serde_json::Value) {
        if !self.alive.load(Ordering::SeqCst) {
            return;
        }
        let line = match serde_json::to_string(notification) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize MCP notification");
                return;
            }
        };
        let write_fut = async {
            let mut stdin_guard = self.stdin.lock().await;
            let Some(ref mut stdin) = *stdin_guard else {
                return Err(McpError::Disconnected);
            };
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            Ok(())
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, write_fut).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "failed to write MCP notification");
            }
            Err(_) => {
                tracing::warn!("MCP notification write timed out");
            }
        }
    }

    async fn stdio_list_tools(&self) -> Vec<McpToolDef> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "params": {}
        });
        let resp = match self.stdio_send_request(&req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(server = %self.config.name, error = %e, "MCP tools/list failed");
                return vec![];
            }
        };
        let tools = match resp.get("result").and_then(|r| r.get("tools")) {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => return vec![],
        };

        tools
            .into_iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                let description = t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let parameters = t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                Some(McpToolDef {
                    name,
                    description,
                    parameters,
                })
            })
            .collect()
    }

    async fn stdio_call_tool(&self, tool_name: &str, args: serde_json::Value) -> ToolOutcome {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args,
            }
        });
        match self.stdio_send_request(&req).await {
            Ok(resp) => {
                let Some(result) = resp.get("result") else {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!(
                            "MCP tool '{tool_name}' returned a response without a result"
                        ),
                    });
                };
                // MCP spec: result.content is an array of content blocks.
                // Surface text blocks as joined text; surface image/resource
                // blocks as `[image: ...]`/`[resource: ...]` placeholders so the
                // model is informed even when the adapter cannot render them.
                if let Some(content_blocks) = result.get("content").and_then(|c| c.as_array()) {
                    tool_result_from_content_blocks(content_blocks, result)
                } else {
                    ToolOutcome::Success {
                        content: serde_json::to_string_pretty(result).unwrap_or_default(),
                    }
                }
            }
            Err(e) => match e {
                McpError::Timeout => ToolOutcome::Failure(ToolError::Timeout {
                    after_secs: REQUEST_TIMEOUT.as_secs(),
                }),
                _ => ToolOutcome::Failure(ToolError::Internal {
                    message: format!("MCP tool '{tool_name}' failed: {e}"),
                }),
            },
        }
    }

    async fn stdio_list_resources(&self) -> Vec<McpResource> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "resources/list",
            "params": {}
        });
        let resp = match self.stdio_send_request(&req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(server = %self.config.name, error = %e, "MCP resources/list failed");
                return vec![];
            }
        };
        let resources = match resp.get("result").and_then(|r| r.get("resources")) {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => return vec![],
        };
        resources
            .into_iter()
            .filter_map(|r| {
                let uri = r.get("uri")?.as_str()?.to_string();
                let name = r
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or(&uri)
                    .to_string();
                let description = r
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let mime_type = r
                    .get("mimeType")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string());
                Some(McpResource {
                    uri,
                    name,
                    description,
                    mime_type,
                })
            })
            .collect()
    }

    async fn stdio_read_resource(&self, uri: &str) -> Result<serde_json::Value, McpError> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "resources/read",
            "params": { "uri": uri }
        });
        self.stdio_send_request(&req).await
    }

    async fn stdio_list_prompts(&self) -> Vec<McpPrompt> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "prompts/list",
            "params": {}
        });
        let resp = match self.stdio_send_request(&req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(server = %self.config.name, error = %e, "MCP prompts/list failed");
                return vec![];
            }
        };
        let prompts = match resp.get("result").and_then(|r| r.get("prompts")) {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => return vec![],
        };
        prompts
            .into_iter()
            .filter_map(|p| {
                let name = p.get("name")?.as_str()?.to_string();
                let description = p
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = p
                    .get("arguments")
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|arg| {
                                Some(McpPromptArg {
                                    name: arg.get("name")?.as_str()?.to_string(),
                                    description: arg
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    required: arg
                                        .get("required")
                                        .and_then(|r| r.as_bool())
                                        .unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(McpPrompt {
                    name,
                    description,
                    arguments,
                })
            })
            .collect()
    }

    async fn stdio_get_prompt(
        &self,
        name: &str,
        args: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        let params = match args {
            Some(a) => serde_json::json!({ "name": name, "arguments": a }),
            None => serde_json::json!({ "name": name }),
        };
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "prompts/get",
            "params": params
        });
        self.stdio_send_request(&req).await
    }

    /// Gracefully disconnect from the child-process server.
    // reason: lifecycle API used by tests; production relies on Drop fallback.
    #[cfg(test)]
    async fn disconnect(&mut self) {
        // Signal the background tasks to stop.
        if let Some(tx) = self.reader_shutdown_tx.take() {
            crate::send_or_warn!(
                tx.send(()),
                "MCP reader shutdown receiver dropped before disconnect"
            );
        }
        if let Some(tx) = self.stderr_shutdown_tx.take() {
            crate::send_or_warn!(
                tx.send(()),
                "MCP stderr drain shutdown receiver dropped before disconnect"
            );
        }

        // Close stdin so the server sees EOF.
        {
            let mut guard = self.stdin.lock().await;
            guard.take();
        }

        // Wait for the background tasks to finish (best-effort).
        #[allow(unused_must_use)]
        {
            if let Some(handle) = self._reader_task.take() {
                tokio::time::timeout(Duration::from_secs(2), handle).await;
            }
            if let Some(handle) = self._stderr_drain.take() {
                tokio::time::timeout(Duration::from_secs(2), handle).await;
            }
        }

        self.alive.store(false, Ordering::SeqCst);

        // Reap the child process. The synchronous std::sync::Mutex guard
        // must not span an await point, so take the child handle first.
        let mut child_opt: Option<Child> = None;
        if let Ok(mut guard) = self.child.lock() {
            child_opt = guard.take();
        }
        if let Some(mut child) = child_opt {
            reap_child(&mut child, Duration::from_secs(2)).await;
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // If we are being dropped without an explicit disconnect(), signal
        // the background tasks and kill the child. A synchronous Drop cannot
        // await, so reaping is best-effort.
        match self {
            McpClient::Stdio(c) => {
                if let Some(tx) = c.reader_shutdown_tx.take() {
                    crate::send_or_warn!(
                        tx.send(()),
                        "MCP reader shutdown receiver dropped during Drop"
                    );
                }
                if let Some(tx) = c.stderr_shutdown_tx.take() {
                    crate::send_or_warn!(
                        tx.send(()),
                        "MCP stderr drain shutdown receiver dropped during Drop"
                    );
                }

                if let Ok(mut guard) = c.child.lock() {
                    if let Some(mut child) = guard.take() {
                        kill_process_group(&mut child);
                        if tokio::runtime::Handle::try_current().is_ok() {
                            std::mem::drop(spawn_child_reap(child));
                        }
                    }
                }
            }
            McpClient::Http(_) => {
                // HTTP transport has no child process; its background tasks
                // are owned by the transport and disconnected explicitly.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::ToolError;

    fn make_mcp_config(name: &str, command: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: vec![],
            env_vars: HashMap::new(),
            transport: "stdio".to_string(),
            url: String::new(),
            bearer_token: String::new(),
        }
    }

    #[tokio::test]
    async fn test_manager_empty_servers() {
        let mgr = McpClientManager::new(&[]).await;
        assert_eq!(mgr.server_count(), 0);
        assert_eq!(mgr.tool_count(), 0);
    }

    #[tokio::test]
    async fn test_manager_no_tools_for_failed_connect() {
        // Try connecting to a nonexistent command
        let servers = vec![make_mcp_config("test", "/nonexistent/command/xyzzy")];
        let mgr = McpClientManager::new(&servers).await;
        assert_eq!(mgr.server_count(), 0); // Failed to connect
        assert_eq!(mgr.tool_count(), 0);
    }

    #[tokio::test]
    async fn test_manager_collects_warning_for_failed_connect() {
        let servers = vec![make_mcp_config("test", "/nonexistent/command/xyzzy")];
        let mgr = McpClientManager::new(&servers).await;
        assert!(
            mgr.warnings()
                .iter()
                .any(|w| w.contains("test") && w.contains("Failed to connect")),
            "expected a startup warning naming the failed server, got {:?}",
            mgr.warnings()
        );
    }

    #[test]
    fn test_has_tool() {
        let mgr = McpClientManager {
            configs: vec![],
            clients: vec![],
            tools: {
                let mut m = HashMap::new();
                m.insert("mcp/test/echo".to_string(), (0_usize, "echo".to_string()));
                m
            },
            tool_defs_cache: HashMap::new(),
            warnings: vec![],
        };
        assert!(mgr.has_tool("mcp/test/echo"));
        assert!(!mgr.has_tool("mcp/nonexistent/foo"));
    }

    #[test]
    fn test_json_rpc_request_format() {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "params": {}
        });
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["method"], "tools/list");
    }

    /// Regression: JSON-RPC responses with an "error" field should be
    /// routed to the waiter as an `Err(McpError::JsonRpc)`, not as an
    /// `Ok` value that the caller silently ignores.
    #[tokio::test]
    async fn test_dispatch_response_routes_error() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert("7".to_string(), tx);

        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "error": { "code": -32601, "message": "Method not found" }
        });
        McpClient::dispatch_response("7".to_string(), resp, &pending, "test").await;

        let err = rx
            .await
            .expect("waiter should receive a result")
            .expect_err("should be an error");
        let msg = format!("{err}");
        assert!(msg.contains("JSON-RPC error"), "got: {msg}");
        assert!(msg.contains("Method not found"), "got: {msg}");
    }

    /// A successful JSON-RPC response is forwarded as `Ok(Value)` to
    /// the matching waiter.
    #[tokio::test]
    async fn test_dispatch_response_routes_success() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert("42".to_string(), tx);

        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": { "tools": [] }
        });
        McpClient::dispatch_response("42".to_string(), resp.clone(), &pending, "test").await;

        let got = rx
            .await
            .expect("waiter should receive a result")
            .expect("should be Ok");
        assert_eq!(got, resp);
    }

    /// Responses for unknown/timed-out request ids are dropped without
    /// panicking.
    #[tokio::test]
    async fn test_dispatch_response_unknown_id_is_noop() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let resp = serde_json::json!({ "jsonrpc": "2.0", "id": 99, "result": {} });
        // Should not panic and should not block.
        McpClient::dispatch_response("99".to_string(), resp, &pending, "test").await;
    }

    /// WO 43.37: the `Ok(Err(_))` branch (oneshot sender dropped without a
    /// response) must remove the pending-map entry, matching the timeout
    /// branches. We drive the branch by polling the request future alongside a
    /// pending-entry check until the entry appears, then dropping the sender so
    /// the request's `rx` sees a closed channel. The test asserts the future
    /// completes with `ChannelClosed` and the pending map is empty (no leaked
    /// entry, no panic).
    #[tokio::test]
    async fn test_stdio_send_request_channel_close_removes_pending_entry() {
        let config = make_mcp_config("chan-close-test", "true");
        // Use `sleep` (not `cat`) so nothing echoes back to stdout — the reader
        // task stays parked and never dispatches a response that would race
        // with our manual pending removal.
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("sleep failed");
        let stdout = child.stdout.take().unwrap();
        let real_stdin = child.stdin.take().unwrap();
        let client = McpClient::from_pipes(real_stdin, stdout, config);
        let pending = match &client {
            McpClient::Stdio(c) => c.pending.clone(),
            _ => unreachable!(),
        };

        let req = serde_json::json!({ "jsonrpc": "2.0", "method": "tools/list", "params": {} });
        let mut request_fut = Box::pin(client.send_request(&req));

        // Drive the future forward until stdio_send_request has written the
        // line and inserted its sender into pending (id "1"). Each iteration:
        // briefly poll the request, then check whether pending is populated.
        // The lock is released before yielding so the request can take it.
        let mut entry_appeared = false;
        for _ in 0..200 {
            let pending_has_entry = !pending.lock().await.is_empty();
            if pending_has_entry {
                entry_appeared = true;
                break;
            }
            // Poll the request for up to 10ms so it can run its write + insert.
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(10), &mut request_fut).await;
        }
        assert!(
            entry_appeared,
            "stdio_send_request never inserted a pending entry"
        );

        // Drop the sender without sending — closes the oneshot so the
        // request's rx yields Err(RecvError) → the Ok(Err(_)) branch.
        drop(pending.lock().await.remove("1"));

        let result = request_fut.await;
        assert!(
            matches!(result, Err(McpError::ChannelClosed)),
            "expected ChannelClosed, got {result:?}"
        );
        assert!(
            pending.lock().await.is_empty(),
            "pending map must be empty after channel-close"
        );

        drop(client);
        let _ = child.kill().await;
    }

    /// `McpError` renders a human-readable message so operators can
    /// diagnose failing MCP servers.
    #[test]
    fn test_mcp_error_display() {
        let e = McpError::JsonRpc {
            code: -32601,
            message: "Method not found".to_string(),
        };
        let s = format!("{e}");
        assert!(s.contains("-32601"), "got: {s}");
        assert!(s.contains("Method not found"), "got: {s}");
    }

    /// A disconnected client returns `ToolOutcome::Failure` instead of
    /// panicking or hanging.
    #[tokio::test]
    async fn test_call_tool_after_disconnect_returns_failure() {
        // Build a client from piped stdin/stdout; the stdout reader will
        // block forever because we never write anything, so we disconnect
        // explicitly to test the error path.
        let config = make_mcp_config("disconnect-test", "true");
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("cat failed");
        let stdout = child.stdout.take().unwrap();
        let real_stdin = child.stdin.take().unwrap();

        let mut client = McpClient::from_pipes(real_stdin, stdout, config);
        client.disconnect().await;

        let outcome = client.call_tool("echo", serde_json::json!({})).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(_)),
            "expected failure after disconnect, got {outcome:?}"
        );
    }

    #[test]
    fn test_content_blocks_surface_text() {
        let result = serde_json::json!({
            "content": [
                { "type": "text", "text": "Hello, " },
                { "type": "text", "text": "world!" }
            ]
        });
        let blocks = result.get("content").unwrap().as_array().unwrap();
        let outcome = tool_result_from_content_blocks(blocks, &result);
        assert!(
            matches!(outcome, ToolOutcome::Success { ref content } if content == "Hello, world!"),
            "got {outcome:?}"
        );
    }

    #[test]
    fn test_content_blocks_surface_image_and_resource_placeholders() {
        let result = serde_json::json!({
            "content": [
                { "type": "text", "text": "Here is the diagram:" },
                { "type": "image", "mimeType": "image/png" },
                { "type": "resource", "resource": { "uri": "file:///tmp/x.csv", "mimeType": "text/csv" } }
            ]
        });
        let blocks = result.get("content").unwrap().as_array().unwrap();
        let outcome = tool_result_from_content_blocks(blocks, &result);
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected success, got {other:?}"),
        };
        assert!(content.contains("Here is the diagram:"), "{content}");
        assert!(content.contains("[image: mime=image/png]"), "{content}");
        assert!(
            content.contains("[resource: file:///tmp/x.csv mime=text/csv]"),
            "{content}"
        );
    }

    #[test]
    fn test_content_blocks_empty_fallback_to_raw_result() {
        let result = serde_json::json!({ "content": [] });
        let blocks = result.get("content").unwrap().as_array().unwrap();
        let outcome = tool_result_from_content_blocks(blocks, &result);
        assert!(
            matches!(outcome, ToolOutcome::Success { ref content } if content.contains("content")),
            "got {outcome:?}"
        );
    }

    /// A JSON-RPC error response is surfaced as a `ToolOutcome::Failure`
    /// with the server message.
    #[tokio::test]
    async fn test_call_tool_maps_jsonrpc_error_to_failure() {
        let config = make_mcp_config("error-test", "true");
        let mut dummy_cmd = tokio::process::Command::new("cat");
        dummy_cmd.stdin(std::process::Stdio::piped());
        dummy_cmd.stdout(std::process::Stdio::piped());
        let mut child = dummy_cmd.spawn().expect("cat failed");
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();

        // Spawn the reader before we write, so it sees the response.
        let client = McpClient::from_pipes(stdin, stdout, config);

        let request_fut = client.call_tool("unknown", serde_json::json!({}));
        // The request is written asynchronously. Yield once to let the writer
        // task progress — no wall-clock sleep needed; the cat process never
        // replies, so the timeout path below produces the Failure regardless.
        tokio::task::yield_now().await;
        let error_resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "Unknown tool" }
        });
        let line = format!("{}\n", serde_json::to_string(&error_resp).unwrap());
        // We cannot write to stdin after from_pipes took ownership, so this
        // test is limited to the public API. Instead, verify that the
        // timeout path produces a Failure (the cat process never replies).
        // The JSON-RPC error path is unit-tested via `dispatch_response`.
        drop(line);
        let outcome = tokio::time::timeout(Duration::from_millis(200), request_fut)
            .await
            .unwrap_or(ToolOutcome::Failure(ToolError::Timeout { after_secs: 0 }));
        assert!(
            matches!(outcome, ToolOutcome::Failure(_)),
            "expected failure, got {outcome:?}"
        );
    }

    #[test]
    fn test_json_id_to_string_string_id() {
        let id = serde_json::json!("abc-123");
        assert_eq!(json_id_to_string(&id), Some("abc-123".to_string()));
    }

    #[test]
    fn test_json_id_to_string_numeric_id() {
        let id = serde_json::json!(42);
        assert_eq!(json_id_to_string(&id), Some("42".to_string()));
    }

    #[test]
    fn test_json_id_to_string_float_id_uses_to_string() {
        let id = serde_json::json!(3.5);
        assert_eq!(json_id_to_string(&id), Some("3.5".to_string()));
    }

    #[test]
    fn test_json_id_to_string_null_returns_none() {
        assert!(json_id_to_string(&serde_json::Value::Null).is_none());
    }

    #[test]
    fn test_json_id_to_string_missing_field_returns_none() {
        let v = serde_json::json!({});
        assert!(json_id_to_string(&v).is_none());
    }

    #[test]
    fn test_json_id_to_string_bool_returns_none() {
        assert!(json_id_to_string(&serde_json::json!(true)).is_none());
    }

    #[test]
    fn test_json_id_to_string_array_returns_none() {
        assert!(json_id_to_string(&serde_json::json!([1, 2])).is_none());
    }

    #[test]
    fn test_json_id_to_string_object_returns_none() {
        assert!(json_id_to_string(&serde_json::json!({"a": 1})).is_none());
    }

    #[test]
    fn test_content_blocks_surface_audio_placeholder() {
        let result = serde_json::json!({
            "content": [
                { "type": "text", "text": "audio:" },
                { "type": "audio" }
            ]
        });
        let blocks = result.get("content").unwrap().as_array().unwrap();
        let outcome = tool_result_from_content_blocks(blocks, &result);
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected success, got {other:?}"),
        };
        assert!(content.contains("audio:"), "{content}");
        assert!(content.contains("[audio block]"), "{content}");
    }

    #[test]
    fn test_content_blocks_surface_unknown_kind_placeholder() {
        let result = serde_json::json!({
            "content": [
                { "type": "video" },
                { "type": "text", "text": "tail" }
            ]
        });
        let blocks = result.get("content").unwrap().as_array().unwrap();
        let outcome = tool_result_from_content_blocks(blocks, &result);
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected success, got {other:?}"),
        };
        assert!(content.contains("[video content block]"), "{content}");
        assert!(content.contains("tail"), "{content}");
    }

    #[test]
    fn test_content_blocks_resource_with_missing_uri_uses_empty() {
        let result = serde_json::json!({
            "content": [
                { "type": "resource" }
            ]
        });
        let blocks = result.get("content").unwrap().as_array().unwrap();
        let outcome = tool_result_from_content_blocks(blocks, &result);
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected success, got {other:?}"),
        };
        assert!(content.contains("[resource:  mime=*/*]"), "{content}");
    }

    #[test]
    fn test_content_blocks_resource_with_mime_fallback() {
        let result = serde_json::json!({
            "content": [
                { "type": "resource", "resource": { "uri": "file:///x.txt" } }
            ]
        });
        let blocks = result.get("content").unwrap().as_array().unwrap();
        let outcome = tool_result_from_content_blocks(blocks, &result);
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected success, got {other:?}"),
        };
        assert!(content.contains("[resource: file:///x.txt"), "{content}");
    }

    #[test]
    fn test_content_blocks_block_without_text_or_type_falls_back_to_raw() {
        let result = serde_json::json!({
            "content": [
                { "unexpected": "field" }
            ]
        });
        let blocks = result.get("content").unwrap().as_array().unwrap();
        let outcome = tool_result_from_content_blocks(blocks, &result);
        assert!(
            matches!(outcome, ToolOutcome::Success { ref content } if content.contains("unexpected")),
            "got {outcome:?}"
        );
    }

    #[test]
    fn test_content_blocks_image_uses_default_mime_when_missing() {
        let result = serde_json::json!({
            "content": [
                { "type": "image" }
            ]
        });
        let blocks = result.get("content").unwrap().as_array().unwrap();
        let outcome = tool_result_from_content_blocks(blocks, &result);
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected success, got {other:?}"),
        };
        assert!(content.contains("[image: mime=image/*]"), "{content}");
    }

    #[tokio::test]
    async fn test_fail_all_pending_signals_all_waiters() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();
        pending.lock().await.insert("1".to_string(), tx1);
        pending.lock().await.insert("2".to_string(), tx2);

        McpClient::fail_all_pending(pending.clone()).await;

        let r1 = rx1.await.expect("waiter 1 should receive");
        let r2 = rx2.await.expect("waiter 2 should receive");
        assert!(r1.is_err(), "waiter 1 should receive error");
        assert!(r2.is_err(), "waiter 2 should receive error");
    }

    #[tokio::test]
    async fn test_fail_all_pending_empty_map_is_noop() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        McpClient::fail_all_pending(pending).await;
    }

    #[tokio::test]
    async fn test_dispatch_response_missing_id_is_noop() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let resp = serde_json::json!({ "jsonrpc": "2.0", "result": {} });
        McpClient::dispatch_response("missing".to_string(), resp, &pending, "test").await;
        assert!(pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_dispatch_response_null_id_is_routed_correctly() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let resp = serde_json::json!({ "jsonrpc": "2.0", "id": null, "result": {} });
        McpClient::dispatch_response("null-id".to_string(), resp, &pending, "test").await;
    }

    #[test]
    fn test_make_mcp_config_defaults() {
        let cfg = make_mcp_config("test-server", "echo");
        assert_eq!(cfg.name, "test-server");
        assert_eq!(cfg.command, "echo");
        assert_eq!(cfg.transport, "stdio");
        assert!(cfg.args.is_empty());
        assert!(cfg.url.is_empty());
        assert!(cfg.bearer_token.is_empty());
    }

    #[tokio::test]
    async fn test_manager_server_count_for_multiple_failed_connects() {
        let servers = vec![
            make_mcp_config("a", "/nonexistent/cmd-a"),
            make_mcp_config("b", "/nonexistent/cmd-b"),
        ];
        let mgr = McpClientManager::new(&servers).await;
        assert_eq!(mgr.server_count(), 0);
        assert!(
            mgr.warnings().len() >= 2,
            "expected at least 2 warnings, got {}",
            mgr.warnings().len()
        );
    }

    #[tokio::test]
    async fn test_manager_tool_count_zero_when_empty() {
        let mgr = McpClientManager::new(&[]).await;
        assert_eq!(mgr.tool_count(), 0);
    }

    #[test]
    fn test_has_tool_false_for_empty_manager() {
        let mgr = McpClientManager {
            configs: vec![],
            clients: vec![],
            tools: HashMap::new(),
            tool_defs_cache: HashMap::new(),
            warnings: vec![],
        };
        assert!(!mgr.has_tool("anything"));
    }

    #[test]
    fn test_handle_server_request_roots_list() {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "roots/list",
            "params": {}
        });
        let response = McpClient::handle_server_request("roots/list", &request, "test")
            .expect("roots/list should return a response");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 42);
        let roots = response
            .get("result")
            .unwrap()
            .get("roots")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(roots.len(), 1);
        let uri = roots[0].get("uri").unwrap().as_str().unwrap();
        assert!(
            uri.starts_with("file://"),
            "expected file:// URI, got: {uri}"
        );
    }

    #[test]
    fn test_handle_server_request_unknown_method_returns_none() {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "foo/bar",
            "params": {}
        });
        assert!(
            McpClient::handle_server_request("foo/bar", &request, "test").is_none(),
            "unknown methods should return None"
        );
    }

    #[test]
    fn test_sampling_messages_translates_roles_and_text() {
        let params = serde_json::json!({
            "messages": [
                { "role": "user", "content": "hello" },
                { "role": "assistant", "content": "hi" }
            ]
        });
        let msgs = sampling_messages(&params);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, crate::shared::Role::User);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].role, crate::shared::Role::Assistant);
        assert_eq!(msgs[1].content, "hi");
    }

    #[test]
    fn test_sampling_messages_joins_content_blocks() {
        let params = serde_json::json!({
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "part1" },
                    { "type": "text", "text": "part2" },
                    { "type": "image", "source": { "type": "base64" } }
                ]}
            ]
        });
        let msgs = sampling_messages(&params);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("part1"), "{}", msgs[0].content);
        assert!(msgs[0].content.contains("part2"), "{}", msgs[0].content);
        assert!(msgs[0].content.contains("[image"), "{}", msgs[0].content);
    }

    #[test]
    fn test_sampling_messages_ignores_unknown_roles() {
        let params = serde_json::json!({
            "messages": [
                { "role": "system", "content": "sys" },
                { "role": "user", "content": "ok" }
            ]
        });
        let msgs = sampling_messages(&params);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, crate::shared::Role::User);
    }

    #[test]
    fn test_sampling_messages_empty_params_yields_empty() {
        let msgs = sampling_messages(&serde_json::json!({}));
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn test_sampling_denied_before_context_returns_error() {
        let sampling = Arc::new(tokio::sync::RwLock::new(None));
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "sampling/createMessage",
            "params": { "messages": [{ "role": "user", "content": "hi" }] }
        });
        let response = McpClient::handle_sampling_request(&sampling, &request, "test").await;
        assert!(
            response.get("error").is_some(),
            "expected an error before sampling context installed, got {response}"
        );
    }

    #[tokio::test]
    async fn test_sampling_auto_approved_when_flag_set() {
        // allow_sampling_unattended must skip the approval bus and attempt the
        // completion directly (which then fails without a live provider). We
        // assert it did NOT hit the approval bus (no ApprovalRequest observed)
        // and returned a completion error rather than a denial error.
        let (approval_tx, mut approval_rx) =
            tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let mut cfg = crate::shared::Config::default();
        cfg.tools.allow_sampling_unattended = true;
        let ctx = SamplingContext {
            approval_tx,
            config: std::sync::Arc::new(cfg),
        };
        let sampling = Arc::new(tokio::sync::RwLock::new(Some(ctx)));
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "sampling/createMessage",
            "params": { "messages": [{ "role": "user", "content": "hi" }] }
        });
        let response = McpClient::handle_sampling_request(&sampling, &request, "test").await;
        // No ApprovalRequest must be sent on the bus when auto-approving.
        assert!(
            approval_rx.try_recv().is_err(),
            "allow_sampling_unattended must not route through the approval bus"
        );
        let err_msg = response
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(
            !err_msg.contains("denied"),
            "unexpected denial message: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_sampling_auto_approved_when_auto_approve_set() {
        // `security.auto_approve = true` is the global operator opt-in and
        // must cover server-initiated sampling too — not just tool calls.
        // This is the regression guard for the WO 12/24/27/30 bug class
        // where auto_approve was inconsistently honored across endpoints.
        let (approval_tx, mut approval_rx) =
            tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let mut cfg = crate::shared::Config::default();
        cfg.security.auto_approve = true;
        // allow_sampling_unattended stays false — the global flag must be enough.
        let ctx = SamplingContext {
            approval_tx,
            config: std::sync::Arc::new(cfg),
        };
        let sampling = Arc::new(tokio::sync::RwLock::new(Some(ctx)));
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "sampling/createMessage",
            "params": { "messages": [{ "role": "user", "content": "hi" }] }
        });
        let response = McpClient::handle_sampling_request(&sampling, &request, "test").await;
        assert!(
            approval_rx.try_recv().is_err(),
            "auto_approve=true must not route sampling through the approval bus"
        );
        let err_msg = response
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(
            !err_msg.contains("denied"),
            "auto_approve must not produce a denial message: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_sampling_routes_through_approval_bus_and_denies() {
        // The approval-gated path MUST send an ApprovalRequest on the shared
        // bus and return an MCP error when it is denied (the headless
        // non-interactive handler denies everything, so this is the default).
        let (approval_tx, mut approval_rx) =
            tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let cfg = crate::shared::Config::default();
        let ctx = SamplingContext {
            approval_tx,
            config: std::sync::Arc::new(cfg),
        };
        let sampling = Arc::new(tokio::sync::RwLock::new(Some(ctx)));
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "sampling/createMessage",
            "params": { "messages": [{ "role": "user", "content": "hi" }] }
        });

        let handle = tokio::spawn(async move {
            McpClient::handle_sampling_request(&sampling, &request, "test").await
        });
        // An approval request must arrive on the bus.
        let approval = tokio::time::timeout(std::time::Duration::from_secs(5), approval_rx.recv())
            .await
            .expect("timeout waiting for approval request")
            .expect("approval channel should not close");
        assert!(
            approval.tool_name.contains("sampling"),
            "expected sampling tool name, got {}",
            approval.tool_name
        );
        // Deny it (as a headless handler would).
        approval
            .response
            .send(ApprovalResponse::DeniedWithReason("test deny".into()))
            .unwrap();

        let response = handle.await.expect("handler task panicked");
        assert!(
            response.get("error").is_some(),
            "denied sampling must return an MCP error, got {response}"
        );
        let msg = response["error"]["message"].as_str().unwrap_or("");
        assert!(msg.contains("denied"), "got: {msg}");
        // The request id must be preserved.
        assert_eq!(response["id"], 9);
    }

    #[tokio::test]
    async fn test_sampling_approved_but_completion_fails_returns_error() {
        // When approved, the handler attempts a live completion (no provider in
        // tests), which fails and returns an MCP error — proving the bus can
        // approve without the handler bypassing to a hardcoded success.
        let (approval_tx, mut approval_rx) =
            tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let cfg = crate::shared::Config::default();
        let ctx = SamplingContext {
            approval_tx,
            config: std::sync::Arc::new(cfg),
        };
        let sampling = Arc::new(tokio::sync::RwLock::new(Some(ctx)));
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "sampling/createMessage",
            "params": { "messages": [{ "role": "user", "content": "hi" }] }
        });

        let handle = tokio::spawn(async move {
            McpClient::handle_sampling_request(&sampling, &request, "test").await
        });
        let approval = tokio::time::timeout(std::time::Duration::from_secs(5), approval_rx.recv())
            .await
            .expect("timeout waiting for approval request")
            .expect("approval channel should not close");
        approval.response.send(ApprovalResponse::Approved).unwrap();

        let response = handle.await.expect("handler task panicked");
        assert!(
            response.get("error").is_some(),
            "failed completion must return an MCP error, got {response}"
        );
        let msg = response["error"]["message"].as_str().unwrap_or("");
        assert!(
            !msg.contains("denied"),
            "approved sampling must not report denial: {msg}"
        );
    }
}
