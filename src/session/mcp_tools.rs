//! Tool wrappers for MCP server tools.
//!
//! Each MCP tool is wrapped in `McpToolWrapper`, which implements the
//! `Tool` trait. Tool names are prefixed with `mcp/<server>/` to avoid
//! collisions with built-in tools. Because `ToolDef` requires `&'static str`
//! for names, the wrapper structs leak the tool metadata — safe because
//! they are created once at session startup and live until the process exits.
//!
//! # Usage
//!
//! The `all_mcp_tools()` function creates `Vec<Arc<dyn Tool>>` from a
//! `McpClientManager`, intended to be appended to the built-in tool list
//! in `main.rs`.

use crate::session::mcp_client::McpClientManager;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::sync::Arc;

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
        // Leak the strings to make them 'static (safe: session-lifetime objects)
        let name: &'static str = Box::leak(full_name.clone().into_boxed_str());
        let desc: &'static str = Box::leak(description.into_boxed_str());
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
        match self.manager.call_tool(&self.full_name, args).await {
            Some(content) => ToolOutcome::Success { content },
            None => ToolOutcome::Failure(ToolError::Internal {
                message: format!(
                    "MCP tool '{}' failed: no response from server",
                    self.full_name
                ),
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
