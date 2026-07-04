//! Tool wrappers for KirkForge plugins.
//!
//! Plugin tools are loaded from `~/.local/share/kirkforge/plugins` via the
//! `PluginRegistry`. Each plugin tool is wrapped to implement the executor's
//! `Tool` trait, similar to `McpToolWrapper`. Plugin tool scripts are invoked
//! synchronously in a blocking task so the async executor stays responsive.

use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use kirkforge_plugin::{Capability, Plugin};
use kirkforge_plugin_host::PluginRegistry;
use std::path::PathBuf;
use std::sync::Arc;

/// A `Tool` trait implementation that forwards calls to a v1 plugin tool script.
pub struct PluginToolWrapper {
    def: ToolDef,
    plugin_root: PathBuf,
    command: PathBuf,
}

impl PluginToolWrapper {
    /// Create a new wrapper for a single plugin tool.
    pub fn new(
        name: String,
        description: String,
        schema: serde_json::Value,
        plugin_root: PathBuf,
        command: PathBuf,
    ) -> Self {
        // ToolDef requires 'static strings; leak session-lifetime metadata.
        let name: &'static str = Box::leak(name.into_boxed_str());
        let desc: &'static str = Box::leak(description.into_boxed_str());
        Self {
            def: ToolDef {
                name,
                description: desc,
                parameters: schema,
            },
            plugin_root,
            command,
        }
    }
}

#[async_trait::async_trait]
impl Tool for PluginToolWrapper {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let tool = kirkforge_plugin_host::PluginTool {
            name: self.def.name.to_string(),
            description: self.def.description.to_string(),
            schema: self.def.parameters.clone(),
            command: self.command.clone(),
            plugin_root: self.plugin_root.clone(),
        };

        // v1 plugin tools run arbitrary shell commands; keep them off
        // the async runtime threads.
        let result = tokio::task::spawn_blocking({
            let tool = tool.clone();
            move || tool.execute(args)
        })
        .await;

        // Mention the env-var name so it appears in the crate's public
        // surface even if only used inside the spawned closure.
        let _ = kirkforge_plugin_host::KIRKFORGE_TOOL_ARGS;

        match result {
            Ok(Ok(content)) => ToolOutcome::Success { content },
            Ok(Err(e)) => ToolOutcome::Failure(ToolError::Execution {
                message: format!("plugin tool failed: {e}"),
                exit_code: None,
                stderr: String::new(),
            }),
            Err(e) => ToolOutcome::Failure(ToolError::Internal {
                message: format!("plugin tool panicked: {e}"),
            }),
        }
    }
}

/// Create `Tool` implementations for all active plugin tools in `registry`.
pub fn all_plugin_tools(registry: &PluginRegistry) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    for hosted in registry.active_plugins() {
        let root = hosted.plugin.root().to_path_buf();
        for cap in hosted.plugin.tools() {
            if let Capability::Tool {
                name,
                description,
                schema,
                command: Some(cmd),
            } = cap
            {
                let wrapper = PluginToolWrapper::new(name, description, schema, root.clone(), cmd);
                tools.push(Arc::new(wrapper));
            }
        }
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirkforge_plugin::TrustTier;
    use kirkforge_plugin_host::TrustPolicy;

    #[test]
    fn wrapper_for_plugin_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kirkforge.toml"),
            r#"
name = "demo"
version = "0.1.0"
description = "demo"
trust = "shell"

[[capabilities]]
type = "tool"
name = "demo/greet"
description = "Greet someone"
command = "greet.sh"
"#,
        )
        .unwrap();
        std::fs::write(plugin_dir.join("greet.sh"), "#!/bin/sh\nprintf 'hello'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(plugin_dir.join("greet.sh"))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(plugin_dir.join("greet.sh"), perms).unwrap();
        }

        let mut reg = PluginRegistry::new();
        reg.load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();

        let tools = all_plugin_tools(&reg);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].def().name, "demo/greet");
    }
}
