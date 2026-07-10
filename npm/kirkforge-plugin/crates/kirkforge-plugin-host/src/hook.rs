//! Plugin-defined lifecycle hook wrapper.
//!
//! v1 hooks are shell scripts invoked with a minimal, explicit environment
//! plus any extra variables the host chooses to expose for the event. The
//! host process environment is never inherited, so secrets are not leaked
//! to plugin code.
//!
//! Exit codes follow the Kimi-style fail-open convention:
//!
//! - `0` → allow
//! - `2` → deny (meaningful for pre-tool hooks)
//! - any other non-zero / timeout / crash → allow, but log a warning

use crate::env::build_plugin_env;
use crate::SandboxPolicy;
use kirkforge_plugin::{Capability, TrustTier};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A plugin hook that can be invoked.
#[derive(Debug, Clone)]
pub struct PluginHook {
    pub event: String,
    pub command: PathBuf,
    pub plugin_root: PathBuf,
    pub plugin_name: String,
    /// Trust tier the plugin was loaded under.
    pub effective_trust: TrustTier,
    /// Minimum trust tier required to run this hook.
    required_trust: TrustTier,
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
    #[error("hook blocked: required trust '{required}' exceeds effective trust '{effective}'")]
    TrustViolation {
        required: TrustTier,
        effective: TrustTier,
    },
    #[error("hook failed to execute: {0}")]
    Io(#[from] std::io::Error),
}

impl PluginHook {
    /// Build a `PluginHook` from a hook capability.
    pub fn from_capability(
        cap: &Capability,
        plugin_root: &Path,
        plugin_name: &str,
        effective_trust: TrustTier,
    ) -> Option<Self> {
        match cap {
            Capability::Hook { event, command } => Some(Self {
                event: event.clone(),
                command: command.clone(),
                plugin_root: plugin_root.to_path_buf(),
                plugin_name: plugin_name.into(),
                effective_trust,
                required_trust: SandboxPolicy::required_tier(cap),
            }),
            _ => None,
        }
    }

    /// Run the hook script with the given extra environment.
    ///
    /// The subprocess receives a minimal safe environment plus any variables
    /// in `extra_env`. The host process environment is never inherited.
    pub fn run(&self, extra_env: &HashMap<String, String>) -> Result<HookVerdict, HookError> {
        if !self.effective_trust.permits(self.required_trust) {
            return Err(HookError::TrustViolation {
                required: self.required_trust,
                effective: self.effective_trust,
            });
        }

        let cmd_path = self.plugin_root.join(&self.command);
        if !cmd_path.exists() {
            return Err(HookError::NotFound(cmd_path));
        }

        let env = build_plugin_env(&self.plugin_root, &self.plugin_name, extra_env);

        let mut attempts = 0;
        let status = loop {
            match Command::new(&cmd_path)
                .env_clear()
                .envs(&env)
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
        make_hook_with_trust(name, body, TrustTier::Shell)
    }

    fn make_hook_with_trust(
        name: &str,
        body: &str,
        effective_trust: TrustTier,
    ) -> (tempfile::TempDir, PluginHook) {
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
            plugin_name: "test-plugin".into(),
            effective_trust,
            required_trust: SandboxPolicy::required_tier(&Capability::Hook {
                event: "pre-tool-bash".into(),
                command: root.clone(),
            }),
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

    #[test]
    fn insufficient_trust_blocks_hook() {
        let (_tmp, hook) = make_hook_with_trust("blocked.sh", "exit 0", TrustTier::ReadOnly);
        let err = hook.run(&HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("blocked"));
    }
}
