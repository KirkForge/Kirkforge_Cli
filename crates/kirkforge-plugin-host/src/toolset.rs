//! Unified toolset abstraction.
//!
//! A `Toolset` is a collection of tools that can be queried by name and
//! executed. KirkForge-Cli will compose multiple toolsets (built-in,
//! plugin, MCP) into a single view. This crate provides the trait and a
//! plugin-backed implementation.

use crate::tool::{PluginTool, KIRKFORGE_TOOL_ARGS};
use crate::PluginRegistry;
use kirkforge_plugin::{Capability, Plugin};

/// Tool metadata independent of source.
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
    /// Source label, e.g. `plugin:my-plugin` or `mcp:server/tool`.
    pub source: String,
}

/// A collection of tools that can be listed and invoked.
pub trait Toolset: Send + Sync {
    /// Source label for tools in this set.
    fn source(&self) -> &str;
    /// Names of all tools provided by this set.
    fn names(&self) -> Vec<String>;
    /// Look up tool metadata by name.
    fn info(&self, name: &str) -> Option<ToolInfo>;
    /// Execute a tool call and return a JSON-serializable string.
    fn execute(&self, name: &str, args: serde_json::Value) -> anyhow::Result<String>;
}

/// Toolset backed by a `PluginRegistry`.
pub struct PluginToolset<'a> {
    registry: &'a PluginRegistry,
}

impl<'a> PluginToolset<'a> {
    pub fn new(registry: &'a PluginRegistry) -> Self {
        Self { registry }
    }
}

impl Toolset for PluginToolset<'_> {
    fn source(&self) -> &str {
        "plugins"
    }

    fn names(&self) -> Vec<String> {
        self.registry
            .active_plugins()
            .iter()
            .flat_map(|hosted| {
                hosted
                    .plugin
                    .tools()
                    .into_iter()
                    .filter_map(|cap| match cap {
                        Capability::Tool { name, .. } => Some(name),
                        _ => None,
                    })
            })
            .collect()
    }

    fn info(&self, name: &str) -> Option<ToolInfo> {
        let (manifest, plugin) = self.registry.tool_by_name(name)?;
        let cap = plugin.tools().into_iter().find(|cap| match cap {
            Capability::Tool { name: n, .. } => n == name,
            _ => false,
        })?;
        match cap {
            Capability::Tool {
                name,
                description,
                schema,
                ..
            } => Some(ToolInfo {
                name,
                description,
                schema,
                source: format!("plugin:{}", manifest.name),
            }),
            _ => None,
        }
    }

    fn execute(&self, name: &str, args: serde_json::Value) -> anyhow::Result<String> {
        let (_, plugin) = self
            .registry
            .tool_by_name(name)
            .ok_or_else(|| anyhow::anyhow!("unknown plugin tool: {name}"))?;
        let cap = plugin
            .tools()
            .into_iter()
            .find(|cap| match cap {
                Capability::Tool { name: n, .. } => n == name,
                _ => false,
            })
            .ok_or_else(|| anyhow::anyhow!("plugin tool {name} disappeared"))?;
        let tool = PluginTool::from_capability(&cap, plugin.root())
            .ok_or_else(|| anyhow::anyhow!("plugin tool {name} has no command"))?;

        let result = tool.execute(args)?;
        // Ensure callers can see the env var name we use.
        let _ = KIRKFORGE_TOOL_ARGS;
        Ok(result)
    }
}

/// Composite toolset that tries multiple inner toolsets in order.
pub struct CompositeToolset {
    toolsets: Vec<Box<dyn Toolset>>,
}

impl CompositeToolset {
    pub fn new(toolsets: Vec<Box<dyn Toolset>>) -> Self {
        Self { toolsets }
    }
}

impl Toolset for CompositeToolset {
    fn source(&self) -> &str {
        "composite"
    }

    fn names(&self) -> Vec<String> {
        self.toolsets.iter().flat_map(|t| t.names()).collect()
    }

    fn info(&self, name: &str) -> Option<ToolInfo> {
        self.toolsets.iter().find_map(|t| t.info(name))
    }

    fn execute(&self, name: &str, args: serde_json::Value) -> anyhow::Result<String> {
        for toolset in &self.toolsets {
            if toolset.names().contains(&name.to_string()) {
                return toolset.execute(name, args);
            }
        }
        anyhow::bail!("unknown tool: {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PluginRegistry, TrustPolicy};
    use kirkforge_plugin::TrustTier;

    #[test]
    fn plugin_toolset_lists_and_executes_tool() {
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
        std::fs::write(
            plugin_dir.join("greet.sh"),
            r#"#!/bin/sh
printf 'hello %s' "$KIRKFORGE_TOOL_ARGS"
"#,
        )
        .unwrap();
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

        let toolset = PluginToolset::new(&reg);
        assert!(toolset.names().contains(&"demo/greet".to_string()));
        let info = toolset.info("demo/greet").unwrap();
        assert_eq!(info.source, "plugin:demo");

        let out = toolset
            .execute("demo/greet", serde_json::json!({"who": "world"}))
            .unwrap();
        assert!(out.contains("world"));
    }
}
