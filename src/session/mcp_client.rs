// Public/future surface in a binary crate: suppress dead-code warnings for pub items.
#![allow(dead_code)]

//! Minimal MCP (Model Context Protocol) client via stdio transport.
//!
//! Spawns MCP servers as subprocesses, communicates via JSON-RPC 2.0 over
//! stdin/stdout, and exposes their tools as `Tool` trait objects that can
//! be added to the executor alongside built-in tools.
//!
//! # Usage
//!
//! Servers are defined in `~/.local/share/kirkforge/config.toml` under
//! the `[[mcp_servers]]` array:
//!
//! ```toml
//! [[mcp_servers]]
//! name = "gitnexus"
//! command = "npx"
//! args = ["gitnexus", "mcp"]
//! ```
//!
//! Each server's tools are prefixed with `mcp/<server>/<tool>`, e.g.
//! `mcp/gitnexus/context`. This avoids name collisions with built-in tools.
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
//! - `Drop` kills the child and spawns a best-effort reap task; a
//!   synchronous `Drop` cannot `.await`, so zombie reaping is best-effort.

use crate::session::process_group::{kill_process_group, setup_process_group};
use crate::shared::McpServerConfig;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, Command};
use tokio::sync::{oneshot, Mutex};

/// Time budget for the MCP handshake (`initialize` request).
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Time budget for a single JSON-RPC request/response round-trip.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors that can occur when sending a JSON-RPC request to an MCP
/// server.
#[derive(Debug)]
enum McpError {
    /// The request could not be written to the server's stdin, or
    /// the server closed its stdin pipe.
    Io(std::io::Error),
    /// The server did not produce a response within `REQUEST_TIMEOUT`.
    Timeout,
    /// The server returned a JSON-RPC error object.
    JsonRpc { code: i64, message: String },
    /// The response channel closed before a response arrived (server
    /// process likely exited).
    ChannelClosed,
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::Io(e) => write!(f, "I/O error: {}", e),
            McpError::Timeout => write!(f, "request timed out"),
            McpError::JsonRpc { code, message } => {
                write!(f, "JSON-RPC error {}: {}", code, message)
            }
            McpError::ChannelClosed => write!(f, "response channel closed"),
        }
    }
}

/// Type alias for the in-flight request map used by the reader task.
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, McpError>>>>>;

/// A single MCP server connection.
///
/// Spawns the configured command, performs the `initialize`→`notifications/initialized`
/// handshake, discovers tools, and provides methods for calling tools and
/// reading resources.
struct McpClient {
    /// Server config (name, command, args).
    config: McpServerConfig,
    /// Write handle for the child's stdin. Protected by a Mutex so multiple
    /// tool-call tasks can send requests concurrently.
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    /// Next JSON-RPC request ID.
    next_id: Arc<Mutex<u64>>,
    /// In-flight requests keyed by JSON-RPC id. The reader task routes
    /// responses here. A Mutex is sufficient: critical sections are tiny
    /// (insert/remove a oneshot sender).
    pending: PendingMap,
    /// The server process handle (for cleanup).
    child: Arc<std::sync::Mutex<Option<Child>>>,
    /// Handle to the stderr-drain background task. Held so `Drop` can
    /// abort it promptly when the client is destroyed.
    stderr_drain: Option<tokio::task::JoinHandle<()>>,
    /// Handle to the stdout-reader dispatch task. Held so `Drop` can
    /// abort it promptly when the client is destroyed.
    reader_task: Option<tokio::task::JoinHandle<()>>,
}

impl McpClient {
    /// Spawn the server process and perform the MCP handshake.
    ///
    /// Returns `None` if the server cannot be spawned or the handshake fails.
    async fn connect(config: &McpServerConfig) -> Option<Self> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        for (k, v) in &config.env_vars {
            cmd.env(k, v);
        }
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
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;

        let stdin = Arc::new(Mutex::new(stdin));
        let next_id = Arc::new(Mutex::new(1_u64));
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let stderr_drain = spawn_stderr_drain(child.stderr.take());
        let reader_task = Self::spawn_reader_task(stdout, pending.clone(), config.name.clone());

        let client = Self {
            config: config.clone(),
            stdin,
            next_id,
            pending,
            child: Arc::new(std::sync::Mutex::new(Some(child))),
            stderr_drain: Some(stderr_drain),
            reader_task: Some(reader_task),
        };

        // MCP handshake: initialize → handle response
        let init_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "kirkforge",
                    "version": "0.1.0"
                }
            }
        });

        let resp = match tokio::time::timeout(STARTUP_TIMEOUT, client.send_request(&init_req)).await
        {
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

        // Send initialized notification (no response expected)
        let init_done = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        client.send_notification(&init_done).await;

        Some(client)
    }

    /// Send a JSON-RPC request and return the raw response Value, or
    /// an `McpError` if the request failed.
    ///
    /// Each request registers a oneshot receiver keyed by its JSON-RPC
    /// id before the request is written. A dedicated reader task routes
    /// inbound responses to the matching waiter. This avoids two
    /// concurrent requests clobbering each other's responses, and it lets
    /// a response arrive in any order.
    async fn send_request(&self, req: &serde_json::Value) -> Result<serde_json::Value, McpError> {
        let id = {
            let mut guard = self.next_id.lock().await;
            let id = *guard;
            *guard += 1;
            id
        };

        // Inject the id into the request object.
        let mut req_with_id = req.clone();
        if let Some(obj) = req_with_id.as_object_mut() {
            obj.insert("id".to_string(), serde_json::json!(id));
        }

        let line = serde_json::to_string(&req_with_id)
            .map_err(|e| McpError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        tracing::debug!(id = id, request = %line, "MCP request");

        // Register the response waiter before writing, so an
        // out-of-order or very fast response can still be routed.
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        // Write the request to the child's stdin with the same timeout
        // as the read side. A frozen server can block on a full stdin
        // pipe indefinitely, so this timeout is part of the request
        // budget.
        let write_fut = async {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            Ok(())
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, write_fut).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = self.pending.lock().await.remove(&id);
                return Err(McpError::Io(e));
            }
            Err(_) => {
                let _ = self.pending.lock().await.remove(&id);
                tracing::warn!(id = id, "MCP request write timed out");
                return Err(McpError::Timeout);
            }
        }

        // Await the routed response.
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                // Reader task closed the channel (server exited) before
                // sending a response.
                tracing::warn!(id = id, "MCP response channel closed");
                Err(McpError::ChannelClosed)
            }
            Err(_) => {
                // Response didn't arrive in time. Clean up the waiter.
                let _ = self.pending.lock().await.remove(&id);
                tracing::warn!(id = id, "MCP request timed out waiting for response");
                Err(McpError::Timeout)
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn send_notification(&self, notification: &serde_json::Value) {
        let line = match serde_json::to_string(notification) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize MCP notification");
                return;
            }
        };
        let write_fut = async {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
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

    /// Call `tools/list` and return the tool definitions.
    async fn list_tools(&self) -> Vec<McpToolDef> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "params": {}
        });
        let resp = match self.send_request(&req).await {
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

    /// Spawn a task that reads JSON-RPC responses from the server's
    /// stdout and routes each one to the matching in-flight request.
    ///
    /// A single reader avoids the previous race where two concurrent
    /// `send_request` calls competed for the same `Mutex<BufReader>`:
    /// whichever acquired the lock first could read and discard a
    /// response intended for the other call.
    fn spawn_reader_task(
        stdout: tokio::process::ChildStdout,
        pending: PendingMap,
        server_name: String,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf).await {
                    Ok(0) => {
                        tracing::debug!(server = %server_name, "MCP stdout closed");
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(server = %server_name, error = %e, "MCP stdout read error");
                        break;
                    }
                }
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(resp) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                    tracing::debug!(server = %server_name, line = %trimmed, "MCP non-JSON stdout line");
                    continue;
                };

                let Some(id) = resp.get("id").and_then(|i| i.as_u64()) else {
                    // Notifications have no id; ignore (or log at debug).
                    tracing::debug!(server = %server_name, response = %resp, "MCP notification");
                    continue;
                };

                Self::dispatch_response(id, resp, &pending, &server_name).await;
            }
        })
    }

    /// Route a single parsed JSON-RPC response to its waiter.
    ///
    /// Split out of the reader task so the error-handling and
    /// out-of-order routing logic can be unit-tested without a real
    /// child process.
    async fn dispatch_response(
        id: u64,
        resp: serde_json::Value,
        pending: &Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, McpError>>>>,
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
                id = id,
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
                tracing::debug!(id = id, "MCP response receiver dropped");
            }
        } else {
            tracing::debug!(server = %server_name, id = id, "MCP response for unknown or timed-out request");
        }
    }

    /// Call `tools/call` with the given tool name and arguments.
    async fn call_tool(&self, tool_name: &str, args: serde_json::Value) -> Option<String> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args,
            }
        });
        let resp = match self.send_request(&req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(tool = %tool_name, error = %e, "MCP tool call failed");
                return None;
            }
        };
        // Extract the content from the result
        let result = resp.get("result")?;
        // MCP spec: result.content is an array of content blocks
        if let Some(content_blocks) = result.get("content").and_then(|c| c.as_array()) {
            let text_parts: Vec<String> = content_blocks
                .iter()
                .filter_map(|block| {
                    block
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            if text_parts.is_empty() {
                Some(serde_json::to_string_pretty(&result).unwrap_or_default())
            } else {
                Some(text_parts.join(""))
            }
        } else {
            Some(serde_json::to_string_pretty(&result).unwrap_or_default())
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Abort the reader and stderr drain tasks first so they don't
        // keep references to the child's stdout/stderr after we kill it.
        if let Some(handle) = self.reader_task.take() {
            handle.abort();
        }
        if let Some(handle) = self.stderr_drain.take() {
            handle.abort();
        }

        // Attempt to kill the child process. A synchronous `Drop` cannot
        // `.await`, so we skip the graceful MCP `notifications/exit`
        // notification. We always call `start_kill()` synchronously; if a
        // Tokio runtime is present we also spawn a best-effort reap task.
        // If `Drop` runs outside a runtime we simply detach the `Child` —
        // the OS will reap it — rather than panicking.
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                // Kill the whole process group so MCP server descendants
                // (e.g. a Node subprocess spawned by `npx`) are not left
                // behind as orphans.
                kill_process_group(&mut child);
                if tokio::runtime::Handle::try_current().is_ok() {
                    std::mem::drop(spawn_child_reap(child));
                }
            }
        }
    }
}

/// Spawn a task that drains a child's stderr into tracing logs.
///
/// This prevents the server from deadlocking once its stderr pipe buffer
/// (typically 64 KB on Linux) fills up.
fn spawn_stderr_drain(stderr: Option<ChildStderr>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(stderr) = stderr else { return };
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if !buf.is_empty() {
                        let line = buf.trim_end_matches('\n').trim_end_matches('\r');
                        if !line.is_empty() {
                            tracing::debug!(target: "mcp_stderr", "{}", line);
                        }
                    }
                }
            }
        }
    })
}

/// Kill a child and reap it asynchronously, bounded by a short timeout.
fn spawn_child_reap(mut child: Child) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    })
}

/// A tool definition returned by an MCP server.
#[derive(Debug, Clone)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Manages a collection of MCP server clients.
///
/// Created at session startup from the `mcp_servers` config array.
/// Each server's tools are prefixed with `mcp/<server>/` to avoid
/// collisions with built-in tools.
pub struct McpClientManager {
    clients: Vec<(String, Arc<McpClient>)>,
    tools: HashMap<String, (usize, String)>, // full_name → (client_index, server_tool_name)
    tool_defs_cache: HashMap<String, McpToolDef>, // full_name → tool definition
}

impl McpClientManager {
    /// Connect to all configured MCP servers and discover their tools.
    pub async fn new(servers: &[McpServerConfig]) -> Self {
        let mut clients: Vec<(String, Arc<McpClient>)> = Vec::new();
        let mut tools: HashMap<String, (usize, String)> = HashMap::new();
        let mut tool_defs_cache: HashMap<String, McpToolDef> = HashMap::new();

        for config in servers.iter() {
            if let Some(client) = McpClient::connect(config).await {
                let client = Arc::new(client);
                let server_tools = client.list_tools().await;
                // Index by the slot this client will occupy in `clients`,
                // NOT the config position — a server that fails to connect
                // is never pushed, so config indices and `clients` indices
                // diverge once any earlier server fails.
                let client_idx = clients.len();
                for t in &server_tools {
                    let full_name = format!("mcp/{}/{}", config.name, t.name);
                    tools.insert(full_name.clone(), (client_idx, t.name.clone()));
                    tool_defs_cache.insert(
                        full_name.clone(),
                        McpToolDef {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: t.parameters.clone(),
                        },
                    );
                }
                clients.push((config.name.clone(), client));
                tracing::info!(
                    server = %config.name,
                    tool_count = server_tools.len(),
                    "MCP server connected"
                );
            } else {
                tracing::warn!(
                    server = %config.name,
                    "Failed to connect to MCP server"
                );
            }
        }

        Self {
            clients,
            tools,
            tool_defs_cache,
        }
    }

    /// Constructor for tests — no server processes needed.
    pub fn with_tools(defs: Vec<(String, String, serde_json::Value)>) -> Self {
        let mut tools = HashMap::new();
        let mut tool_defs_cache = HashMap::new();
        for (idx, (full_name, desc, params)) in defs.iter().enumerate() {
            tools.insert(full_name.clone(), (idx, String::new()));
            tool_defs_cache.insert(
                full_name.clone(),
                McpToolDef {
                    name: String::new(),
                    description: desc.clone(),
                    parameters: params.clone(),
                },
            );
        }
        Self {
            clients: vec![],
            tools,
            tool_defs_cache,
        }
    }

    /// Return cached tool definitions for creating Tool wrappers.
    /// Returns (full_name, description, parameters) for each discovered tool.
    pub fn tool_defs(&self) -> Vec<(String, String, serde_json::Value)> {
        self.tool_defs_cache
            .iter()
            .map(|(name, def)| {
                (
                    name.clone(),
                    def.description.clone(),
                    def.parameters.clone(),
                )
            })
            .collect()
    }

    /// Call an MCP tool by its full name (e.g., "mcp/gitnexus/context").
    pub async fn call_tool(&self, full_name: &str, args: serde_json::Value) -> Option<String> {
        let (client_idx, server_name) = self.tools.get(full_name)?;
        let (_server_cfg_name, client) = self.clients.get(*client_idx)?;
        client.call_tool(server_name, args).await
    }

    /// Return the number of connected servers.
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    /// Return the number of tools across all servers.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Check whether a tool with the given full name exists.
    pub fn has_tool(&self, full_name: &str) -> bool {
        self.tools.contains_key(full_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(name: &str, command: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: vec![],
            env_vars: HashMap::new(),
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
        let servers = vec![make_config("test", "/nonexistent/command/xyzzy")];
        let mgr = McpClientManager::new(&servers).await;
        assert_eq!(mgr.server_count(), 0); // Failed to connect
        assert_eq!(mgr.tool_count(), 0);
    }

    #[test]
    fn test_has_tool() {
        let mgr = McpClientManager {
            clients: vec![],
            tools: {
                let mut m = HashMap::new();
                m.insert("mcp/test/echo".to_string(), (0_usize, "echo".to_string()));
                m
            },
            tool_defs_cache: HashMap::new(),
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
        pending.lock().await.insert(7, tx);

        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "error": { "code": -32601, "message": "Method not found" }
        });
        McpClient::dispatch_response(7, resp, &pending, "test").await;

        let err = rx
            .await
            .expect("waiter should receive a result")
            .expect_err("should be an error");
        let msg = format!("{}", err);
        assert!(msg.contains("JSON-RPC error"), "got: {}", msg);
        assert!(msg.contains("Method not found"), "got: {}", msg);
    }

    /// A successful JSON-RPC response is forwarded as `Ok(Value)` to
    /// the matching waiter.
    #[tokio::test]
    async fn test_dispatch_response_routes_success() {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(42, tx);

        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": { "tools": [] }
        });
        McpClient::dispatch_response(42, resp.clone(), &pending, "test").await;

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
        McpClient::dispatch_response(99, resp, &pending, "test").await;
    }

    /// `McpError` renders a human-readable message so operators can
    /// diagnose failing MCP servers.
    #[test]
    fn test_mcp_error_display() {
        let e = McpError::JsonRpc {
            code: -32601,
            message: "Method not found".to_string(),
        };
        let s = format!("{}", e);
        assert!(s.contains("-32601"), "got: {}", s);
        assert!(s.contains("Method not found"), "got: {}", s);
    }
}
