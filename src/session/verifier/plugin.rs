//! Plugin-defined verifier adapter.
//!
//! v1 plugin verifiers are shell scripts declared in a plugin manifest's
//! `[[capabilities]]` section with `type = "verifier"`. The adapter bridges
//! the plugin-host `PluginVerifier` onto the executor's `Verifier` trait so
//! plugin verifiers participate in the same priority-based truth model as
//! built-in verifiers.
//!
//! The verifier receives the event being checked as environment variables:
//!
//! - `KF_VERIFIER_NAME`   — the verifier's declared name
//! - `KF_EVENT_KIND`      — event kind label (e.g. "file_write")
//! - `KF_EVENT_JSON`      — full `BusEvent` serialized to JSON
//!
//! Exit code `0` means pass; any non-zero exit code fails, with stderr as
//! the failure message. The plugin-host `PluginVerifier` already implements
//! this convention; this adapter just converts between the executor's
//! async `Verifier` trait and the synchronous plugin verifier.

use super::{Verdict, VerificationError, Verifier};
use crate::session::event_bus::BusEvent;
use kirkforge_plugin_host::{PluginVerifier, VerifierVerdict};
use std::collections::HashMap;
use std::sync::Arc;

/// Adapter that runs a plugin verifier inside the executor's verifier slots.
#[derive(Debug, Clone)]
pub struct PluginVerifierAdapter {
    inner: PluginVerifier,
    priority: u8,
}

impl PluginVerifierAdapter {
    /// Wrap a plugin verifier with a priority.
    pub fn new(inner: PluginVerifier, priority: u8) -> Self {
        Self { inner, priority }
    }
}

#[async_trait::async_trait]
impl Verifier for PluginVerifierAdapter {
    fn name(&self) -> &str {
        &self.inner.name
    }

    fn priority(&self) -> u8 {
        self.priority
    }

    async fn verify(&self, event: &BusEvent) -> Verdict {
        let mut env = HashMap::new();
        env.insert("KF_VERIFIER_NAME".to_string(), self.inner.name.clone());
        env.insert("KF_EVENT_KIND".to_string(), event.kind().to_string());
        match serde_json::to_string(event) {
            Ok(json) => env.insert("KF_EVENT_JSON".to_string(), json),
            Err(e) => {
                return Verdict::Unfixable(VerificationError {
                    description: format!(
                        "plugin verifier {}: failed to serialize event",
                        self.inner.name
                    ),
                    file: None,
                    details: e.to_string(),
                });
            }
        };

        let inner = self.inner.clone();
        let verdict = match tokio::task::spawn_blocking(move || inner.run(&env)).await {
            Ok(result) => result,
            Err(e) => {
                return Verdict::Unfixable(VerificationError {
                    description: format!("plugin verifier {}: task panicked", self.inner.name),
                    file: None,
                    details: e.to_string(),
                });
            }
        };

        match verdict {
            Ok(VerifierVerdict::Pass) => Verdict::Clean,
            Ok(VerifierVerdict::Fail { message }) => Verdict::Unfixable(VerificationError {
                description: format!("plugin verifier {}: {}", self.inner.name, message),
                file: None,
                details: message,
            }),
            Err(e) => Verdict::Unfixable(VerificationError {
                description: format!("plugin verifier {}: execution failed", self.inner.name),
                file: None,
                details: e.to_string(),
            }),
        }
    }
}

/// Build verifier adapters from every active plugin verifier capability.
///
/// Returns a vector so the caller can register each adapter into the
/// executor's `VerifierSlots` with its declared priority.
pub fn verifiers_from_registry(
    registry: &kirkforge_plugin_host::PluginRegistry,
) -> Vec<Arc<dyn Verifier>> {
    use kirkforge_plugin::Plugin;
    let mut out: Vec<Arc<dyn Verifier>> = Vec::new();
    for hosted in registry.active_plugins() {
        let plugin = &hosted.plugin;
        for cap in plugin.verifiers() {
            if let Some((name, priority, command)) = as_verifier_parts(&cap) {
                let pv = PluginVerifier {
                    name: name.clone(),
                    command: command.clone(),
                    plugin_root: plugin.root().to_path_buf(),
                };
                out.push(Arc::new(PluginVerifierAdapter::new(pv, priority)));
            }
        }
    }
    out
}

fn as_verifier_parts(
    cap: &kirkforge_plugin::Capability,
) -> Option<(String, u8, std::path::PathBuf)> {
    match cap {
        kirkforge_plugin::Capability::Verifier {
            name,
            priority,
            command: Some(command),
        } => Some((name.clone(), *priority, command.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event_bus::FileReadEvent;
    use kirkforge_plugin_host::{PluginRegistry, TrustPolicy};
    use std::path::PathBuf;

    #[test]
    fn adapter_name_uses_plugin_name() {
        let pv = PluginVerifier {
            name: "demo".into(),
            command: PathBuf::from("bin/check.sh"),
            plugin_root: PathBuf::from("/tmp"),
        };
        let adapter = PluginVerifierAdapter::new(pv, 5);
        assert_eq!(adapter.name(), "demo");
        assert_eq!(adapter.priority(), 5);
    }

    #[tokio::test]
    async fn passing_plugin_verifier_returns_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let script = root.join("pass.sh");
        #[cfg(unix)]
        {
            std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&script, "exit 0\n").unwrap();
        }

        let pv = PluginVerifier {
            name: "pass".into(),
            command: PathBuf::from("pass.sh"),
            plugin_root: root,
        };
        let adapter = PluginVerifierAdapter::new(pv, 1);
        let event = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("src/lib.rs"),
            size_bytes: 100,
            truncated: false,
        });
        let verdict = adapter.verify(&event).await;
        assert!(matches!(verdict, Verdict::Clean));
    }

    #[tokio::test]
    async fn failing_plugin_verifier_returns_unfixable_with_stderr() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let script = root.join("fail.sh");
        #[cfg(unix)]
        {
            std::fs::write(&script, "#!/bin/sh\necho 'bad pattern' >&2\nexit 1\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&script, "bad pattern\n").unwrap();
        }

        let pv = PluginVerifier {
            name: "fail".into(),
            command: PathBuf::from("fail.sh"),
            plugin_root: root,
        };
        let adapter = PluginVerifierAdapter::new(pv, 1);
        let event = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("src/lib.rs"),
            size_bytes: 100,
            truncated: false,
        });
        let verdict = adapter.verify(&event).await;
        match verdict {
            Verdict::Unfixable(err) => {
                assert!(err.description.contains("bad pattern"));
            }
            other => panic!("expected Unfixable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failing_plugin_verifier_includes_env_vars_in_stderr() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let script = root.join("env-check.sh");
        #[cfg(unix)]
        {
            std::fs::write(
                &script,
                "#!/bin/sh\necho \"$KF_VERIFIER_NAME $KF_EVENT_KIND $(echo \"$KF_EVENT_JSON\" | head -c 200)\" >&2\nexit 1\n",
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&script, "echo %KF_VERIFIER_NAME% %KF_EVENT_KIND%\nexit 1\n").unwrap();
        }

        let pv = PluginVerifier {
            name: "env-check".into(),
            command: PathBuf::from("env-check.sh"),
            plugin_root: root,
        };
        let adapter = PluginVerifierAdapter::new(pv, 1);
        let event = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("src/lib.rs"),
            size_bytes: 100,
            truncated: false,
        });
        let verdict = adapter.verify(&event).await;
        match verdict {
            Verdict::Unfixable(err) => {
                assert!(
                    err.details.contains("env-check"),
                    "details: {}",
                    err.details
                );
                assert!(
                    err.details.contains("file_read") || err.details.contains("FileRead"),
                    "details: {}",
                    err.details
                );
                assert!(
                    err.details.contains("src/lib.rs"),
                    "event JSON substring missing: {}",
                    err.details
                );
            }
            other => panic!("expected Unfixable, got {other:?}"),
        }
    }

    #[test]
    fn verifiers_from_registry_builds_adapters() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let plugin_dir = plugins_dir.join("demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kirkforge.toml"),
            r#"
name = "demo-verifier"
version = "0.1.0"
description = "demo"
trust = "shell"

[[capabilities]]
type = "verifier"
name = "demo-v"
priority = 7
command = "bin/check.sh"
"#,
        )
        .unwrap();

        let mut registry = PluginRegistry::new();
        let warnings = registry
            .load_from_dir(
                &plugins_dir,
                TrustPolicy::up_to(kirkforge_plugin::TrustTier::Shell),
            )
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let verifiers = verifiers_from_registry(&registry);
        assert_eq!(verifiers.len(), 1);
        assert_eq!(verifiers[0].name(), "demo-v");
        assert_eq!(verifiers[0].priority(), 7);
    }
}
