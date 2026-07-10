//! Plugin-defined tool wrapper.
//!
//! v1 tools are shell scripts. The host serializes the tool arguments as JSON
//! in the `KIRKFORGE_TOOL_ARGS` environment variable and reads the tool result
//! from stdout. A non-zero exit code becomes an error using stderr as the
//! message.

use kirkforge_plugin::Capability;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variable used to pass tool arguments to a v1 plugin tool.
pub const KIRKFORGE_TOOL_ARGS: &str = "KIRKFORGE_TOOL_ARGS";
/// Alias used by plugin tool scripts that need `jq`-style JSON parsing.
pub const KIRKFORGE_TOOL_ARGS_JSON: &str = "KIRKFORGE_TOOL_ARGS_JSON";

/// A plugin tool that can be executed.
#[derive(Debug, Clone)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
    pub command: PathBuf,
    pub plugin_root: PathBuf,
}

/// Errors that can occur when running a plugin tool.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool command not found: {0}")]
    NotFound(PathBuf),
    #[error("tool failed to execute: {0}")]
    Io(#[from] std::io::Error),
}

impl PluginTool {
    /// Build a `PluginTool` from a tool capability.
    pub fn from_capability(cap: &Capability, plugin_root: &Path) -> Option<Self> {
        match cap {
            Capability::Tool {
                name,
                description,
                schema,
                command,
            } => {
                let command = command.clone()?;
                Some(Self {
                    name: name.clone(),
                    description: description.clone(),
                    schema: schema.clone(),
                    command,
                    plugin_root: plugin_root.to_path_buf(),
                })
            }
            _ => None,
        }
    }

    /// Execute the tool with the given JSON arguments.
    pub fn execute(&self, args: serde_json::Value) -> Result<String, ToolError> {
        let cmd_path = self.plugin_root.join(&self.command);
        if !cmd_path.exists() {
            return Err(ToolError::NotFound(cmd_path));
        }

        let mut attempts = 0;
        let output = loop {
            match Command::new(&cmd_path)
                .env(KIRKFORGE_TOOL_ARGS, args.to_string())
                .env(KIRKFORGE_TOOL_ARGS_JSON, args.to_string())
                .current_dir(&self.plugin_root)
                .output()
            {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 3 => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    attempts += 1;
                    continue;
                }
                other => break other?,
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = if stderr.trim().is_empty() {
                format!("tool exited with {:?}", output.status.code())
            } else {
                stderr.trim().to_string()
            };
            return Err(ToolError::Io(std::io::Error::other(message)));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool(name: &str, body: &str) -> (tempfile::TempDir, PluginTool) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let command = std::path::PathBuf::from(name);
        let script = format!("#!/bin/sh\n{body}");
        std::fs::write(root.join(&command), script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(root.join(&command))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(root.join(&command), perms).unwrap();
        }
        let tool = PluginTool {
            name: "test".into(),
            description: "test tool".into(),
            schema: serde_json::Value::Null,
            command,
            plugin_root: root,
        };
        (tmp, tool)
    }

    #[test]
    fn reads_stdout_as_result() {
        let (_tmp, tool) = make_tool("tool.sh", "echo hello");
        assert_eq!(tool.execute(serde_json::Value::Null).unwrap(), "hello");
    }

    #[test]
    fn receives_args_in_env() {
        let (_tmp, tool) = make_tool(
            "args.sh",
            "printf '%s' \"$KIRKFORGE_TOOL_ARGS\"; printf '%s' \"$KIRKFORGE_TOOL_ARGS_JSON\"",
        );
        let args = serde_json::json!({"n": 7});
        let out = tool.execute(args.clone()).unwrap();
        assert_eq!(out, format!("{args}{args}"));
    }

    #[test]
    fn non_zero_becomes_error() {
        let (_tmp, tool) = make_tool("fail.sh", "echo boom >&2\nexit 1");
        assert!(tool.execute(serde_json::Value::Null).is_err());
    }
}
