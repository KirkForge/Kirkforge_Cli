use crate::session::mcp_client::McpClientManager;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::path::Path;
use std::sync::Arc;

pub fn validate_mcp_uri(uri: &str, workspace: &Path) -> Result<(), String> {
    let scheme = uri.split("://").next().unwrap_or("");
    if scheme == "data" {
        return Err(format!("data: URI '{uri}' is not allowed (injection risk)"));
    }
    if let Some(path) = uri.strip_prefix("file://") {
        let resolved = Path::new(path);
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize workspace: {e}"))?;
        match resolved.canonicalize() {
            Ok(canonical_path) => {
                if !canonical_path.starts_with(&canonical_workspace) {
                    return Err(format!("file URI '{uri}' resolves outside the workspace"));
                }
            }
            Err(_) => {
                let absolute = if resolved.is_absolute() {
                    resolved.to_path_buf()
                } else {
                    workspace.join(resolved)
                };
                if !absolute.starts_with(&canonical_workspace) {
                    return Err(format!("file URI '{uri}' resolves outside the workspace"));
                }
            }
        }
    }
    Ok(())
}

pub struct McpResourceTool {
    manager: Arc<McpClientManager>,
}

impl McpResourceTool {
    pub fn new(manager: Arc<McpClientManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl Tool for McpResourceTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "mcp_resource",
            description:
                "List or read resources from connected MCP servers. Use action=list to discover available resources, action=read to fetch a specific resource by URI.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "read"],
                        "description": "list: discover resources; read: fetch a resource by URI"
                    },
                    "server": {
                        "type": "string",
                        "description": "Optional MCP server name to filter by"
                    },
                    "uri": {
                        "type": "string",
                        "description": "Resource URI (required for action=read)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let action = match args.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => {
                return ToolOutcome::Failure(ToolError::InvalidArgs {
                    message: "Missing 'action' argument".into(),
                });
            }
        };
        let server_filter = args.get("server").and_then(|s| s.as_str());

        match action {
            "list" => {
                let resources = self.manager.list_resources().await;
                if resources.is_empty() {
                    return ToolOutcome::Success {
                        content: "No resources found on any connected MCP server.".into(),
                    };
                }
                let mut lines = Vec::new();
                for (server, resource) in &resources {
                    if let Some(filter) = server_filter {
                        if server != filter {
                            continue;
                        }
                    }
                    lines.push(format!(
                        "Server: {} | URI: {} | Name: {}{}",
                        server,
                        resource.uri,
                        resource.name,
                        resource
                            .description
                            .is_empty()
                            .then(String::new)
                            .unwrap_or_else(|| format!(" | {}", resource.description))
                    ));
                }
                if lines.is_empty() {
                    ToolOutcome::Success {
                        content: format!(
                            "No resources found{}.",
                            server_filter
                                .map(|s| format!(" on server '{s}'"))
                                .unwrap_or_default()
                        ),
                    }
                } else {
                    ToolOutcome::Success {
                        content: lines.join("\n"),
                    }
                }
            }
            "read" => {
                let uri = match args.get("uri").and_then(|u| u.as_str()) {
                    Some(u) => u,
                    None => {
                        return ToolOutcome::Failure(ToolError::InvalidArgs {
                            message: "Missing 'uri' argument for action=read".into(),
                        });
                    }
                };
                let workspace = std::env::current_dir().unwrap_or_default();
                if let Err(e) = validate_mcp_uri(uri, &workspace) {
                    return ToolOutcome::Failure(ToolError::InvalidArgs { message: e });
                }
                match self.manager.read_resource(uri).await {
                    Ok(val) => ToolOutcome::Success {
                        content: serde_json::to_string_pretty(&val).unwrap_or_default(),
                    },
                    Err(e) => ToolOutcome::Failure(ToolError::Internal { message: e }),
                }
            }
            other => ToolOutcome::Failure(ToolError::InvalidArgs {
                message: format!("Unknown action '{other}', expected 'list' or 'read'"),
            }),
        }
    }
}

pub struct McpPromptTool {
    manager: Arc<McpClientManager>,
}

impl McpPromptTool {
    pub fn new(manager: Arc<McpClientManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl Tool for McpPromptTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "mcp_prompt",
            description:
                "List or get prompt templates from connected MCP servers. Use action=list to discover prompts, action=get to retrieve a specific prompt by name.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "get"],
                        "description": "list: discover prompts; get: retrieve a prompt by name"
                    },
                    "server": {
                        "type": "string",
                        "description": "Optional MCP server name to filter by"
                    },
                    "name": {
                        "type": "string",
                        "description": "Prompt name (required for action=get)"
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Optional JSON object of prompt arguments (for action=get)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let action = match args.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => {
                return ToolOutcome::Failure(ToolError::InvalidArgs {
                    message: "Missing 'action' argument".into(),
                });
            }
        };
        let server_filter = args.get("server").and_then(|s| s.as_str());

        match action {
            "list" => {
                let prompts = self.manager.list_prompts().await;
                if prompts.is_empty() {
                    return ToolOutcome::Success {
                        content: "No prompts found on any connected MCP server.".into(),
                    };
                }
                let mut lines = Vec::new();
                for (server, prompt) in &prompts {
                    if let Some(filter) = server_filter {
                        if server != filter {
                            continue;
                        }
                    }
                    let arg_names: Vec<String> = prompt
                        .arguments
                        .iter()
                        .map(|a| {
                            format!("{}{}", a.name, if a.required { " (required)" } else { "" })
                        })
                        .collect();
                    lines.push(format!(
                        "Server: {} | Name: {} | Args: [{}]{}",
                        server,
                        prompt.name,
                        if arg_names.is_empty() {
                            "none".to_string()
                        } else {
                            arg_names.join(", ")
                        },
                        prompt
                            .description
                            .is_empty()
                            .then(String::new)
                            .unwrap_or_else(|| format!(" | {}", prompt.description))
                    ));
                }
                if lines.is_empty() {
                    ToolOutcome::Success {
                        content: format!(
                            "No prompts found{}.",
                            server_filter
                                .map(|s| format!(" on server '{s}'"))
                                .unwrap_or_default()
                        ),
                    }
                } else {
                    ToolOutcome::Success {
                        content: lines.join("\n"),
                    }
                }
            }
            "get" => {
                let name = match args.get("name").and_then(|n| n.as_str()) {
                    Some(n) => n,
                    None => {
                        return ToolOutcome::Failure(ToolError::InvalidArgs {
                            message: "Missing 'name' argument for action=get".into(),
                        });
                    }
                };
                let prompt_args = args.get("arguments").cloned();
                match self.manager.get_prompt(name, prompt_args).await {
                    Ok(val) => ToolOutcome::Success {
                        content: serde_json::to_string_pretty(&val).unwrap_or_default(),
                    },
                    Err(e) => ToolOutcome::Failure(ToolError::Internal { message: e }),
                }
            }
            other => ToolOutcome::Failure(ToolError::InvalidArgs {
                message: format!("Unknown action '{other}', expected 'list' or 'get'"),
            }),
        }
    }
}

pub fn all_mcp_resource_tools(manager: Arc<McpClientManager>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(McpResourceTool::new(manager.clone())),
        Arc::new(McpPromptTool::new(manager)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_manager() -> Arc<McpClientManager> {
        Arc::new(McpClientManager::with_tools(vec![]))
    }

    #[test]
    fn resource_tool_def_has_correct_name_and_schema() {
        let tool = McpResourceTool::new(empty_manager());
        let def = tool.def();
        assert_eq!(def.name, "mcp_resource");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[test]
    fn prompt_tool_def_has_correct_name_and_schema() {
        let tool = McpPromptTool::new(empty_manager());
        let def = tool.def();
        assert_eq!(def.name, "mcp_prompt");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[tokio::test]
    async fn resource_list_empty_manager() {
        let tool = McpResourceTool::new(empty_manager());
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"action": "list"}))
            .await;
        let ToolOutcome::Success { content } = outcome else {
            panic!("expected Success, got {outcome:?}");
        };
        assert!(content.contains("No resources"));
    }

    #[tokio::test]
    async fn resource_read_missing_uri() {
        let tool = McpResourceTool::new(empty_manager());
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"action": "read"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "expected InvalidArgs, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn resource_read_no_servers() {
        let tool = McpResourceTool::new(empty_manager());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"action": "read", "uri": "test://x"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { .. })),
            "expected Internal failure (no servers), got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn resource_unknown_action() {
        let tool = McpResourceTool::new(empty_manager());
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"action": "delete"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "expected InvalidArgs, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn prompt_list_empty_manager() {
        let tool = McpPromptTool::new(empty_manager());
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"action": "list"}))
            .await;
        let ToolOutcome::Success { content } = outcome else {
            panic!("expected Success, got {outcome:?}");
        };
        assert!(content.contains("No prompts"));
    }

    #[tokio::test]
    async fn prompt_get_missing_name() {
        let tool = McpPromptTool::new(empty_manager());
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"action": "get"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "expected InvalidArgs, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn prompt_get_no_servers() {
        let tool = McpPromptTool::new(empty_manager());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"action": "get", "name": "test"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { .. })),
            "expected Internal failure (no servers), got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn prompt_unknown_action() {
        let tool = McpPromptTool::new(empty_manager());
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"action": "delete"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "expected InvalidArgs, got {outcome:?}"
        );
    }

    #[test]
    fn all_mcp_resource_tools_returns_two_tools() {
        let tools = all_mcp_resource_tools(empty_manager());
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(names.contains(&"mcp_resource"));
        assert!(names.contains(&"mcp_prompt"));
    }

    #[test]
    fn validate_mcp_uri_allows_https_uri() {
        let workspace = Path::new("/tmp/workspace");
        assert!(validate_mcp_uri("https://example.com/resource", workspace).is_ok());
    }

    #[test]
    fn validate_mcp_uri_allows_workspace_internal_file_uri() {
        let workspace = std::env::current_dir().unwrap();
        let uri = format!("file://{}", workspace.join("src").display());
        assert!(validate_mcp_uri(&uri, &workspace).is_ok());
    }

    #[test]
    fn validate_mcp_uri_blocks_workspace_external_file_uri() {
        let workspace = std::env::current_dir().unwrap();
        let uri = "file:///etc/passwd";
        let result = validate_mcp_uri(uri, &workspace);
        assert!(result.is_err(), "expected block for external file URI");
        assert!(
            result.unwrap_err().contains("outside the workspace"),
            "error should mention outside the workspace"
        );
    }

    #[test]
    fn validate_mcp_uri_allows_custom_scheme() {
        let workspace = Path::new("/tmp/workspace");
        assert!(validate_mcp_uri("custom://resource", workspace).is_ok());
        assert!(validate_mcp_uri("mcp://server/resource", workspace).is_ok());
    }

    #[test]
    fn validate_mcp_uri_blocks_data_uri() {
        let workspace = Path::new("/tmp/workspace");
        let err =
            validate_mcp_uri("data:text/html,<script>alert(1)</script>", workspace).unwrap_err();
        assert!(err.contains("not allowed"));
    }
}
