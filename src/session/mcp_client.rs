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

use crate::shared::McpServerConfig;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// A single MCP server connection.
///
/// Spawns the configured command, performs the `initialize`→`notifications/initialized`
/// handshake, discovers tools, and provides methods for calling tools and
/// reading resources.
struct McpClient {
    /// Server config (name, command, args).
    #[allow(dead_code)]
    config: McpServerConfig,
    /// Write handle for the child's stdin. Protected by a Mutex so multiple
    /// tool-call tasks can send requests concurrently.
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    /// Read handle for the child's stdout (line-delimited JSON-RPC).
    reader: Arc<tokio::sync::Mutex<BufReader<tokio::process::ChildStdout>>>,
    /// Next JSON-RPC request ID.
    next_id: Arc<Mutex<u64>>,
    /// The server process handle (for cleanup).
    #[allow(dead_code)]
    child: Arc<std::sync::Mutex<Option<Child>>>,
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

        let mut child = cmd.spawn().ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;

        let stdin = Arc::new(Mutex::new(stdin));
        let reader = Arc::new(Mutex::new(BufReader::new(stdout)));
        let next_id = Arc::new(Mutex::new(1_u64));

        let client = Self {
            config: config.clone(),
            stdin,
            reader,
            next_id,
            child: Arc::new(std::sync::Mutex::new(Some(child))),
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

        let resp = client.send_request(&init_req).await?;
        // Verify it's a valid response to initialize
        resp.get("result")?;

        // Send initialized notification (no response expected)
        let init_done = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        client.send_notification(&init_done).await;

        Some(client)
    }

    /// Send a JSON-RPC request and return the raw response Value.
    async fn send_request(&self, req: &serde_json::Value) -> Option<serde_json::Value> {
        let id = {
            let mut guard = self.next_id.lock().await;
            let id = *guard;
            *guard += 1;
            id
        };

        // We need the id in the request object
        let mut req_with_id = req.clone();
        if let Some(obj) = req_with_id.as_object_mut() {
            obj.insert("id".to_string(), serde_json::json!(id));
        }

        let line = serde_json::to_string(&req_with_id).ok()?;
        tracing::debug!(id = id, request = %line, "MCP request");

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await.ok()?;
            stdin.write_all(b"\n").await.ok()?;
            stdin.flush().await.ok()?;
        }

        // Read response lines until we find one matching our ID
        let mut reader = self.reader.lock().await;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).await.ok()?;
            if n == 0 {
                return None; // EOF
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(resp) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                continue;
            };
            // Check if this response is for us
            if resp.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return Some(resp);
            }
            // Otherwise it's a notification or a response for another request —
            // skip it (in practice MCP servers don't send unsolicited messages,
            // but we handle them gracefully).
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn send_notification(&self, notification: &serde_json::Value) {
        let line = serde_json::to_string(notification).unwrap_or_default();
        let mut stdin = self.stdin.lock().await;
        let _ = stdin.write_all(line.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.flush().await;
    }

    /// Call `tools/list` and return the tool definitions.
    async fn list_tools(&self) -> Vec<McpToolDef> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "params": {}
        });
        let Some(resp) = self.send_request(&req).await else {
            return vec![];
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
                let parameters = t.get("inputSchema").cloned().unwrap_or_else(|| {
                    serde_json::json!({"type": "object", "properties": {}})
                });
                Some(McpToolDef {
                    name,
                    description,
                    parameters,
                })
            })
            .collect()
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
        let resp = self.send_request(&req).await?;
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
        // Attempt to kill the child process
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
                // Don't wait — the process will be reaped by init if we die
            }
        }
    }
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

        for (idx, config) in servers.iter().enumerate() {
            if let Some(client) = McpClient::connect(config).await {
                let client = Arc::new(client);
                let server_tools = client.list_tools().await;
                for t in &server_tools {
                    let full_name = format!("mcp/{}/{}", config.name, t.name);
                    tools.insert(full_name.clone(), (idx, t.name.clone()));
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
    #[allow(dead_code)]
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
                (name.clone(), def.description.clone(), def.parameters.clone())
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
    #[allow(dead_code)]
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    /// Return the number of tools across all servers.
    #[allow(dead_code)]
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
}
