//! Plugin-defined lifecycle hook wrapper.
//!
//! v1 hooks are shell scripts invoked with the same environment variables as
//! built-in hooks. Exit codes follow the Kimi-style fail-open convention:
//!
//! - `0` → allow
//! - `2` → deny (meaningful for pre-tool hooks)
//! - any other non-zero / timeout / crash → allow, but log a warning

use kirkforge_plugin::Capability;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A plugin hook that can be invoked.
#[derive(Debug, Clone)]
pub struct PluginHook {
    pub event: String,
    pub command: PathBuf,
    pub plugin_root: PathBuf,
}

/// Outcome of a hook invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookVerdict {
    Allow,
    Deny,
}

/// Errors that can occur when running a hook.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("hook command not found: {0}")]
    NotFound(PathBuf),
    #[error("hook failed to execute: {0}")]
    Io(#[from] std::io::Error),
}

impl PluginHook {
    /// Build a `PluginHook` from a hook capability.
    pub fn from_capability(cap: &Capability, plugin_root: &Path) -> Option<Self> {
        match cap {
            Capability::Hook { event, command } => {
                if !crate::paths::is_command_within_root(plugin_root, command) {
                    return None;
                }
                Some(Self {
                    event: event.clone(),
                    command: command.clone(),
                    plugin_root: plugin_root.to_path_buf(),
                })
            }
            _ => None,
        }
    }

    /// Run the hook script with the given environment.
    pub fn run(&self, env: &HashMap<String, String>) -> Result<HookVerdict, HookError> {
        let cmd_path = self.plugin_root.join(&self.command);
        if !cmd_path.exists() {
            return Err(HookError::NotFound(cmd_path));
        }

        let mut attempts = 0;
        let status = loop {
            match Command::new(&cmd_path)
                .envs(env)
                .current_dir(&self.plugin_root)
                .status()
            {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 3 => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    attempts += 1;
                    continue;
                }
                other => break other?,
            }
        };

        Ok(match status.code() {
            Some(0) => HookVerdict::Allow,
            Some(2) => HookVerdict::Deny,
            code => {
                tracing::warn!(
                    event = %self.event,
                    command = %self.command.display(),
                    exit_code = ?code,
                    "plugin hook exited non-zero; fail-open allowing"
                );
                HookVerdict::Allow
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_hook(name: &str, body: &str) -> (tempfile::TempDir, PluginHook) {
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
        let hook = PluginHook {
            event: "pre-tool-bash".into(),
            command,
            plugin_root: root.clone(),
        };
        (tmp, hook)
    }

    #[test]
    fn exit_zero_allows() {
        let (_tmp, hook) = make_hook("allow.sh", "exit 0");
        assert_eq!(hook.run(&HashMap::new()).unwrap(), HookVerdict::Allow);
    }

    #[test]
    fn exit_two_denies() {
        let (_tmp, hook) = make_hook("deny.sh", "exit 2");
        assert_eq!(hook.run(&HashMap::new()).unwrap(), HookVerdict::Deny);
    }

    #[test]
    fn other_exit_allows_with_warning() {
        let (_tmp, hook) = make_hook("warn.sh", "exit 1");
        assert_eq!(hook.run(&HashMap::new()).unwrap(), HookVerdict::Allow);
    }
}
