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
    /// The full tool name (e.g., "mcp/context-server/context").
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

    #[test]
    fn test_all_mcp_tools_empty_manager_yields_no_tools() {
        let mgr = Arc::new(McpClientManager::with_tools(vec![]));
        let tools = all_mcp_tools(mgr);
        assert!(tools.is_empty());
    }

    #[test]
    fn test_all_mcp_tools_preserves_multiple_tools() {
        let defs = vec![
            (
                "mcp/srv/a".to_string(),
                "Tool A".to_string(),
                serde_json::json!({"type": "object"}),
            ),
            (
                "mcp/srv/b".to_string(),
                "Tool B".to_string(),
                serde_json::json!({"type": "object"}),
            ),
            (
                "mcp/srv/c".to_string(),
                "Tool C".to_string(),
                serde_json::json!({"type": "object"}),
            ),
        ];
        let mgr = Arc::new(McpClientManager::with_tools(defs));
        let tools = all_mcp_tools(mgr);
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(names.contains(&"mcp/srv/a"));
        assert!(names.contains(&"mcp/srv/b"));
        assert!(names.contains(&"mcp/srv/c"));
    }

    #[test]
    fn test_wrapper_def_carries_parameters() {
        let params = serde_json::json!({
            "type": "object",
            "properties": {
                "x": {"type": "number"},
                "y": {"type": "string"},
            },
            "required": ["x"],
        });
        let mgr = Arc::new(McpClientManager::with_tools(vec![(
            "mcp/test/params".to_string(),
            "Params check".to_string(),
            params.clone(),
        )]));
        let tools = all_mcp_tools(mgr);
        assert_eq!(tools.len(), 1);
        let def = tools[0].def();
        assert_eq!(def.name, "mcp/test/params");
        assert_eq!(def.description, "Params check");
        assert_eq!(def.parameters, params);
    }

    #[tokio::test]
    async fn test_wrapper_run_forwards_to_manager_call_tool() {
        let mgr = Arc::new(McpClientManager::with_tools(vec![(
            "mcp/test/forward".to_string(),
            "Forward".to_string(),
            serde_json::json!({"type": "object"}),
        )]));
        let tools = all_mcp_tools(mgr.clone());
        assert_eq!(tools.len(), 1);
        let ctx = crate::tools::ToolContext::new();
        let outcome = tools[0].run(&ctx, serde_json::json!({"x": 1})).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(_)),
            "no live server → expected Failure, got {outcome:?}"
        );
    }
}
