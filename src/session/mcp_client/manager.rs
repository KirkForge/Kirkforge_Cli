//! Manager for a collection of MCP server clients. Extracted from the
//! parent module so the single-connection `McpClient` lifecycle and the
//! multi-server manager each have their own file. Fields are `pub(super)`
//! so the parent module's tests can construct a manager directly.

use crate::shared::{McpServerConfig, ToolError, ToolOutcome};
use std::collections::HashMap;
use std::sync::Arc;

use super::McpClient;

/// A tool definition returned by an MCP server.
#[derive(Debug, Clone)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A slot in the manager that can be replaced on reconnect.
type ClientSlot = Arc<tokio::sync::RwLock<Arc<McpClient>>>;

/// Manages a collection of MCP server clients.
///
/// Created at session startup from the `mcp_servers` config array.
/// Each server's tools are prefixed with `mcp/<server>/` to avoid
/// collisions with built-in tools.
pub struct McpClientManager {
    /// Original configs, kept so a crashed client can be restarted.
    pub(super) configs: Vec<McpServerConfig>,
    /// Connected clients. The index matches the `clients` index stored in
    /// `tools`. A client can be replaced when it dies.
    pub(super) clients: Vec<ClientSlot>,
    pub(super) tools: HashMap<String, (usize, String)>, // full_name → (client_index, server_tool_name)
    pub(super) tool_defs_cache: HashMap<String, McpToolDef>, // full_name → tool definition
    /// Human-readable warnings collected while connecting servers.
    pub(super) warnings: Vec<String>,
}

impl McpClientManager {
    /// Connect to all configured MCP servers and discover their tools.
    pub async fn new(servers: &[McpServerConfig]) -> Self {
        let mut clients: Vec<ClientSlot> = Vec::new();
        let mut tools: HashMap<String, (usize, String)> = HashMap::new();
        let mut tool_defs_cache: HashMap<String, McpToolDef> = HashMap::new();
        let mut configs: Vec<McpServerConfig> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

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
                clients.push(Arc::new(tokio::sync::RwLock::new(client)));
                configs.push(config.clone());
                tracing::info!(
                    server = %config.name,
                    tool_count = server_tools.len(),
                    "MCP server connected"
                );
            } else {
                let msg = format!("Failed to connect to MCP server '{}'", config.name);
                tracing::warn!(server = %config.name, "{}", msg);
                warnings.push(msg);
            }
        }

        if !servers.is_empty() && tools.is_empty() {
            warnings.push(
                "No MCP tools discovered; configured MCP servers are unavailable or exposed no tools"
                    .to_string(),
            );
        }

        Self {
            configs,
            clients,
            tools,
            tool_defs_cache,
            warnings,
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
            configs: vec![],
            clients: vec![],
            tools,
            tool_defs_cache,
            warnings: vec![],
        }
    }

    /// Return startup warnings so callers can surface them to the user.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
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

    /// Call an MCP tool by its full name (e.g., "mcp/context-server/context").
    pub async fn call_tool(&self, full_name: &str, args: serde_json::Value) -> ToolOutcome {
        let (client_idx, server_name) = match self.tools.get(full_name) {
            Some(pair) => pair,
            None => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("unknown MCP tool '{full_name}'"),
                });
            }
        };
        let slot = match self.clients.get(*client_idx) {
            Some(entry) => entry,
            None => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!(
                        "MCP tool '{full_name}' references a server slot that no longer exists"
                    ),
                });
            }
        };
        let config = match self.configs.get(*client_idx) {
            Some(c) => c,
            None => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!(
                        "MCP tool '{full_name}' references a server config that no longer exists"
                    ),
                });
            }
        };

        // Fast path: client is alive.
        {
            let client = slot.read().await;
            if client.is_alive() {
                return client.call_tool(server_name, args).await;
            }
        }

        // Client died; try to reconnect once. Take the write lock before
        // connecting so concurrent callers don't race to reconnect and
        // overwrite each other's successful clients with failing ones.
        let mut guard = slot.write().await;
        if guard.is_alive() {
            return guard.call_tool(server_name, args).await;
        }
        let new_client = match McpClient::connect(config).await {
            Some(c) => Arc::new(c),
            None => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("MCP server '{}' is unavailable", config.name),
                });
            }
        };
        let result = new_client.call_tool(server_name, args).await;
        *guard = new_client;
        result
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
