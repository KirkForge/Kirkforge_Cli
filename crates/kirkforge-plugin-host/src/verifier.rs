//! Plugin-defined verifier wrapper.
//!
//! v1 verifiers are shell scripts invoked with environment variables describing
//! the event being verified. A zero exit code means the check passed; any
//! non-zero exit code fails, with stderr as the failure message.

use crate::env::curated_env;
use kirkforge_plugin::Capability;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A plugin verifier that can be invoked.
#[derive(Debug, Clone)]
pub struct PluginVerifier {
    pub name: String,
    pub command: PathBuf,
    pub plugin_root: PathBuf,
}

/// Outcome of a verifier invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierVerdict {
    Pass,
    Fail { message: String },
}

/// Errors that can occur when running a verifier.
#[derive(Debug, thiserror::Error)]
pub enum VerifierError {
    #[error("verifier command not found: {0}")]
    NotFound(PathBuf),
    #[error("verifier failed to execute: {0}")]
    Io(#[from] std::io::Error),
}

impl PluginVerifier {
    /// Build a `PluginVerifier` from a verifier capability.
    pub fn from_capability(cap: &Capability, plugin_root: &Path) -> Option<Self> {
        match cap {
            Capability::Verifier { name, command, .. } => {
                let command = command.clone()?;
                if !crate::paths::is_command_within_root(plugin_root, &command) {
                    return None;
                }
                Some(Self {
                    name: name.clone(),
                    command,
                    plugin_root: plugin_root.to_path_buf(),
                })
            }
            _ => None,
        }
    }

    /// Run the verifier script with the given environment.
    pub fn run(&self, env: &HashMap<String, String>) -> Result<VerifierVerdict, VerifierError> {
        let cmd_path = self.plugin_root.join(&self.command);
        if !cmd_path.exists() {
            return Err(VerifierError::NotFound(cmd_path));
        }

        let mut attempts = 0;
        let output = loop {
            match Command::new(&cmd_path)
                .env_clear()
                .envs(curated_env(env))
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

        if output.status.success() {
            Ok(VerifierVerdict::Pass)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = if stderr.trim().is_empty() {
                format!("exited with {:?}", output.status.code())
            } else {
                stderr.trim().to_string()
            };
            Ok(VerifierVerdict::Fail { message })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_verifier(name: &str, body: &str) -> (tempfile::TempDir, PluginVerifier) {
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
        let verifier = PluginVerifier {
            name: "test".into(),
            command,
            plugin_root: root,
        };
        (tmp, verifier)
    }

    #[test]
    fn exit_zero_passes() {
        let (_tmp, v) = make_verifier("pass.sh", "exit 0");
        assert_eq!(v.run(&HashMap::new()).unwrap(), VerifierVerdict::Pass);
    }

    #[test]
    fn non_zero_fails_with_stderr() {
        let (_tmp, v) = make_verifier("fail.sh", "echo 'bad' >&2\nexit 1");
        assert_eq!(
            v.run(&HashMap::new()).unwrap(),
            VerifierVerdict::Fail {
                message: "bad".into()
            }
        );
    }
}
