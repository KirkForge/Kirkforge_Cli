//! Plugin-defined verifier wrapper.
//!
//! v1 verifiers are shell scripts invoked with a minimal, explicit environment
//! plus any extra variables the host chooses to expose. The host process
//! environment is never inherited, so secrets are not leaked to plugin code.
//! A zero exit code means the check passed; any non-zero exit code fails, with
//! stderr as the failure message.

use crate::env::build_plugin_env;
use crate::SandboxPolicy;
use kirkforge_plugin::{Capability, TrustTier};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A plugin verifier that can be invoked.
#[derive(Debug, Clone)]
pub struct PluginVerifier {
    pub name: String,
    pub command: PathBuf,
    pub plugin_root: PathBuf,
    pub plugin_name: String,
    /// Trust tier the plugin was loaded under.
    pub effective_trust: TrustTier,
    /// Minimum trust tier required to run this verifier.
    required_trust: TrustTier,
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
    #[error("verifier blocked: required trust '{required}' exceeds effective trust '{effective}'")]
    TrustViolation {
        required: TrustTier,
        effective: TrustTier,
    },
    #[error("verifier failed to execute: {0}")]
    Io(#[from] std::io::Error),
}

impl PluginVerifier {
    /// Build a `PluginVerifier` from a verifier capability.
    pub fn from_capability(
        cap: &Capability,
        plugin_root: &Path,
        plugin_name: &str,
        effective_trust: TrustTier,
    ) -> Option<Self> {
        match cap {
            Capability::Verifier { name, command, .. } => {
                let command = command.clone()?;
                Some(Self {
                    name: name.clone(),
                    command,
                    plugin_root: plugin_root.to_path_buf(),
                    plugin_name: plugin_name.into(),
                    effective_trust,
                    required_trust: SandboxPolicy::required_tier(cap),
                })
            }
            _ => None,
        }
    }

    /// Run the verifier script with the given extra environment.
    ///
    /// The subprocess receives a minimal safe environment plus any variables
    /// in `extra_env`. The host process environment is never inherited.
    pub fn run(
        &self,
        extra_env: &HashMap<String, String>,
    ) -> Result<VerifierVerdict, VerifierError> {
        if !self.effective_trust.permits(self.required_trust) {
            return Err(VerifierError::TrustViolation {
                required: self.required_trust,
                effective: self.effective_trust,
            });
        }

        let cmd_path = self.plugin_root.join(&self.command);
        if !cmd_path.exists() {
            return Err(VerifierError::NotFound(cmd_path));
        }

        let env = build_plugin_env(&self.plugin_root, &self.plugin_name, extra_env);

        let mut attempts = 0;
        let output = loop {
            match Command::new(&cmd_path)
                .env_clear()
                .envs(&env)
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
        make_verifier_with_trust(name, body, TrustTier::ReadOnly)
    }

    fn make_verifier_with_trust(
        name: &str,
        body: &str,
        effective_trust: TrustTier,
    ) -> (tempfile::TempDir, PluginVerifier) {
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
            plugin_root: root.clone(),
            plugin_name: "test-plugin".into(),
            effective_trust,
            required_trust: SandboxPolicy::required_tier(&Capability::Verifier {
                name: "test".into(),
                priority: 0,
                command: Some(root.clone()),
            }),
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

    #[test]
    fn verifier_never_blocked_by_readonly_default() {
        // Verifiers require ReadOnly, so ReadOnly effective trust is sufficient.
        let (_tmp, v) = make_verifier_with_trust("readonly.sh", "exit 0", TrustTier::ReadOnly);
        assert_eq!(v.run(&HashMap::new()).unwrap(), VerifierVerdict::Pass);
    }
}
