//! Tool wrappers for MCP server tools.
//!
//! Each MCP tool is wrapped in `McpToolWrapper`, which implements the
//! `Tool` trait. Tool names are prefixed with `mcp/<server>/` to avoid
//! collisions with built-in tools. Because `ToolDef` requires `&'static str`
//! for names, the wrapper structs intern the tool metadata via
//! `shared::intern_static_str` — leaking at most once per distinct name so
//! that rebuilding wrappers (e.g. on `/reload plugins`) does not accumulate.
//!
//! # Usage
//!
//! The `all_mcp_tools()` function creates `Vec<Arc<dyn Tool>>` from a
//! `McpClientManager`, intended to be appended to the built-in tool list
//! in `main.rs`.

use crate::session::mcp_client::McpClientManager;
use crate::shared::{intern_static_str, ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::sync::Arc;
use std::time::Duration;

/// A Tool trait implementation that forwards calls to an MCP server.
///
/// Stores an `Arc<McpClientManager>` and the full tool name. The `run()`
/// method calls `manager.call_tool()` with the server-side name.
pub struct McpToolWrapper {
    /// The full tool name (e.g., "mcp/gitnexus/context").
    full_name: String,
    /// The tool definition (with leaked static strings).
    def: ToolDef,
    /// Shared manager for calling tools.
    manager: Arc<McpClientManager>,
}

impl McpToolWrapper {
    /// Create a new wrapper for a single MCP tool.
    ///
    /// The caller should use `all_mcp_tools()` for creating these in batch.
    pub fn new(
        full_name: String,
        description: String,
        parameters: serde_json::Value,
        manager: Arc<McpClientManager>,
    ) -> Self {
        // Intern (not leak-per-call) so /reload plugins rebuilding these wrappers
        // does not accumulate fresh allocations. See `intern_static_str`.
        let name: &'static str = intern_static_str(&full_name);
        let desc: &'static str = intern_static_str(&description);
        Self {
            full_name,
            def: ToolDef {
                name,
                description: desc,
                parameters,
            },
            manager,
        }
    }
}

#[async_trait::async_trait]
impl Tool for McpToolWrapper {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        // Defensive outer timeout in case `call_tool` gets stuck in a
        // reconnect loop. The manager has its own per-request timeout; this
        // catches any slow path above it.
        const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
        match tokio::time::timeout(TOOL_TIMEOUT, self.manager.call_tool(&self.full_name, args))
            .await
        {
            Ok(outcome) => outcome,
            Err(_) => ToolOutcome::Failure(ToolError::Timeout {
                after_secs: TOOL_TIMEOUT.as_secs(),
            }),
        }
    }
}

/// Create Tool implementations for all MCP tools discovered by the manager.
///
/// Returns a Vec of `Arc<dyn Tool>` that can be appended to the built-in
/// tool list before passing to the Executor.
pub fn all_mcp_tools(manager: Arc<McpClientManager>) -> Vec<Arc<dyn Tool>> {
    // We need to re-request tool defs from the manager. The manager should
    // cache these. For now we'll rely on the manager exposing them.
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    for (full_name, desc, params) in manager.tool_defs() {
        let wrapper = McpToolWrapper::new(full_name.clone(), desc, params, manager.clone());
        tools.push(Arc::new(wrapper));
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrapper_creation() {
        // We can't easily test with a real McpClientManager since it
        // requires spawning processes, but we can verify the wrapper
        // structure compiles and the naming works.
        let mgr = Arc::new(McpClientManager::with_tools(vec![(
            "mcp/test/echo".to_string(),
            "Echo back the input".to_string(),
            serde_json::json!({"type": "object", "properties": {"message": {"type": "string"}}}),
        )]));

        let tools = all_mcp_tools(mgr);
        assert_eq!(tools.len(), 1);
        let def = tools[0].def();
        assert_eq!(def.name, "mcp/test/echo");
        assert_eq!(def.description, "Echo back the input");
    }
}
