//! Plugin-defined verifier registration.
//!
//! v1 plugin verifiers are shell scripts declared in a plugin manifest's
//! `[[capabilities]]` section with `type = "verifier"`.
//! `register_plugin_verifiers_into_bus` wires each `Capability::Verifier`
//! into the unified `VerifierBus`. The bus runs plugin verifiers via the
//! same env-cleared subprocess path as the host `PluginVerifier` and
//! collects structured `VerdictEntry`s; the executor queries the bus after
//! file-modifying tool calls and injects error verdicts into the
//! conversation.
//!
//! WO 47.14: the legacy event-driven `PluginVerifierAdapter` (which bridged
//! the same plugin verifiers onto the async `Verifier` trait in
//! `VerifierSlots`) is deleted. Until then, plugin verifiers were registered
//! into BOTH systems and ran twice per file-modifying tool call. The bus
//! path is the sole integration path — `BusVerifier` is the surviving trait
//! of the WO 47.14 unification.
//!
//! The verifier receives the following environment variables:
//!
//! - `KF_VERIFIER_NAME`   — the verifier's declared name
//! - `KF_CHANGED_FILES`   — newline-separated list of changed files
//!
//! `ceiling:` env-var contract (bucketlist 3.30, resolved by WO 47.14):
//! the deleted event-driven path additionally passed `KF_EVENT_KIND` +
//! `KF_EVENT_JSON` (the full serialized `BusEvent`) and also fired on
//! read/bash events. Plugin scripts depending on those vars or on
//! non-file-event coverage must read `KF_CHANGED_FILES` instead; restoring
//! event visibility requires extending `VerifyContext` with the event
//! payload (tracked in WO 47.14 remaining work).
//!
//! Exit code `0` means pass; any non-zero exit code fails, with stderr as
<<<<<<< HEAD
//! the failure message. The plugin-host `PluginVerifier` already implements
//! this convention; this adapter just converts between the executor's
//! async `Verifier` trait and the synchronous plugin verifier.

use super::{Verdict, VerificationError, Verifier};
use crate::session::verifier::types::BusEvent;
use kf_plugin_host::{PluginVerifier, VerifierVerdict};
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
                    line: None,
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
                    line: None,
                });
            }
        };

        match verdict {
            Ok(VerifierVerdict::Pass) => Verdict::Clean,
            Ok(VerifierVerdict::Fail { message }) => Verdict::Unfixable(VerificationError {
                description: format!("plugin verifier {}: {}", self.inner.name, message),
                file: None,
                details: message,
                line: None,
            }),
            Err(e) => Verdict::Unfixable(VerificationError {
                description: format!("plugin verifier {}: execution failed", self.inner.name),
                file: None,
                details: e.to_string(),
                line: None,
            }),
        }
    }
}

/// Build verifier adapters from every active plugin verifier capability.
///
/// Returns a vector so the caller can register each adapter into the
/// executor's `VerifierSlots` with its declared priority.
pub fn verifiers_from_registry(
    registry: &kf_plugin_host::PluginRegistry,
) -> Vec<Arc<dyn Verifier>> {
    use kf_plugin_host::Plugin;
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
||||||| 15ad6877
//! the failure message. The plugin-host `PluginVerifier` already implements
//! this convention; this adapter just converts between the executor's
//! async `Verifier` trait and the synchronous plugin verifier.

use super::{Verdict, VerificationError, Verifier};
use crate::session::verifier::types::BusEvent;
use kf_plugin_host::{PluginVerifier, VerifierVerdict};
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
                    line: None,
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
                    line: None,
                });
            }
        };

        match verdict {
            Ok(VerifierVerdict::Pass) => Verdict::Clean,
            Ok(VerifierVerdict::Fail { message }) => Verdict::Unfixable(VerificationError {
                description: format!("plugin verifier {}: {}", self.inner.name, message),
                file: None,
                details: message,
                line: None,
            }),
            Err(e) => Verdict::Unfixable(VerificationError {
                description: format!("plugin verifier {}: execution failed", self.inner.name),
                file: None,
                details: e.to_string(),
                line: None,
            }),
        }
    }
}

/// Build verifier adapters from every active plugin verifier capability.
///
/// Returns a vector so the caller can register each adapter into the
/// executor's `VerifierSlots` with its declared priority.
pub fn verifiers_from_registry(
    registry: &kf_plugin_host::PluginRegistry,
) -> Vec<Arc<dyn Verifier>> {
    use kf_plugin_sdk::Plugin;
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
=======
//! the failure message. The plugin-host `PluginVerifier` implements this
//! convention; the bus-side `PluginBusVerifier` (bus.rs) is the adapter.
>>>>>>> wo/wo47.14

fn as_verifier_parts(cap: &kf_plugin_host::Capability) -> Option<(String, u8, std::path::PathBuf)> {
    match cap {
        kf_plugin_host::Capability::Verifier {
            name,
            priority,
            command: Some(command),
        } => Some((name.clone(), *priority, command.clone())),
        _ => None,
    }
}

/// Register every active plugin's verifier capabilities into the unified
/// `VerifierBus` (ADR-028 / ADR-043). For each `Capability::Verifier` with a
/// command, the resolved `plugin_root.join(command)` path is handed to
/// `bus.add_plugin_verifier`. Returns the number of verifiers registered.
///
/// WO 47.14: this is the sole plugin-verifier integration path — the legacy
/// `verifiers_from_registry` + `PluginVerifierAdapter` event-driven
/// registration is deleted.
pub fn register_plugin_verifiers_into_bus(
    registry: &kf_plugin_host::PluginRegistry,
    bus: &mut crate::session::verifier::bus::VerifierBus,
) -> usize {
    use kf_plugin_host::Plugin;
    let mut count = 0;
    for hosted in registry.active_plugins() {
        let plugin = &hosted.plugin;
        let plugin_root = plugin.root().to_path_buf();
        for cap in plugin.verifiers() {
            if let Some((name, priority, command)) = as_verifier_parts(&cap) {
                bus.add_plugin_verifier(name, priority, plugin_root.clone(), command);
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_verifier_parts_returns_none_for_non_verifier_capability() {
        let cap = kf_plugin_sdk::Capability::Skill {
            trigger: "/x".into(),
            prompt: "do x".into(),
            skill_file: None,
            model_hint: None,
        };
        assert!(as_verifier_parts(&cap).is_none());
    }

    #[test]
    fn as_verifier_parts_returns_none_for_verifier_without_command() {
        let cap = kf_plugin_sdk::Capability::Verifier {
            name: "no-cmd".into(),
            priority: 1,
            command: None,
        };
        assert!(as_verifier_parts(&cap).is_none());
    }

    #[test]
    fn as_verifier_parts_extracts_fields_when_command_present() {
        let cap = kf_plugin_sdk::Capability::Verifier {
            name: "fmt".into(),
            priority: 3,
            command: Some(std::path::PathBuf::from("bin/fmt.sh")),
        };
        let (name, priority, command) =
            as_verifier_parts(&cap).expect("verifier capability with command should yield parts");
        assert_eq!(name, "fmt");
        assert_eq!(priority, 3);
        assert_eq!(command, std::path::PathBuf::from("bin/fmt.sh"));
    }

<<<<<<< HEAD
        let mut registry = PluginRegistry::new();
        let warnings = registry
            .load_from_dir(
                &plugins_dir,
                TrustPolicy::up_to(kf_plugin_host::TrustTier::Shell),
            )
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let verifiers = verifiers_from_registry(&registry);
        assert_eq!(verifiers.len(), 1);
        assert_eq!(verifiers[0].name(), "demo-v");
        assert_eq!(verifiers[0].priority(), 7);
||||||| 15ad6877
        let mut registry = PluginRegistry::new();
        let warnings = registry
            .load_from_dir(
                &plugins_dir,
                TrustPolicy::up_to(kf_plugin_sdk::TrustTier::Shell),
            )
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let verifiers = verifiers_from_registry(&registry);
        assert_eq!(verifiers.len(), 1);
        assert_eq!(verifiers[0].name(), "demo-v");
        assert_eq!(verifiers[0].priority(), 7);
=======
    #[test]
    fn register_plugin_verifiers_into_bus_returns_zero_for_empty_registry() {
        let registry = kf_plugin_host::PluginRegistry::new();
        let mut bus = crate::session::verifier::bus::VerifierBus::new();
        let count = register_plugin_verifiers_into_bus(&registry, &mut bus);
        assert_eq!(count, 0);
        assert_eq!(bus.verifier_count(), 0);
>>>>>>> wo/wo47.14
    }

    #[cfg(unix)]
    #[test]
    fn register_plugin_verifiers_into_bus_wires_each_capability() {
        use crate::session::verifier::bus::{Severity, VerifierBus, VerifierSource, VerifyContext};

        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let plugin_dir = plugins_dir.join("demo");
        let plugin_bin_dir = plugin_dir.join("bin");
        std::fs::create_dir_all(&plugin_bin_dir).unwrap();

        let check = plugin_bin_dir.join("check.sh");
        std::fs::write(&check, "#!/bin/sh\necho 'nope' >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&check).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&check, perms).unwrap();

        std::fs::write(
            plugin_dir.join("kf-code.toml"),
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

        let mut registry = kf_plugin_host::PluginRegistry::new();
        let warnings = registry
            .load_from_dir(
                &plugins_dir,
<<<<<<< HEAD
                TrustPolicy::up_to(kf_plugin_host::TrustTier::Shell),
||||||| 15ad6877
                TrustPolicy::up_to(kf_plugin_sdk::TrustTier::Shell),
=======
                kf_plugin_host::TrustPolicy::up_to(kf_plugin_sdk::TrustTier::Shell),
>>>>>>> wo/wo47.14
            )
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let mut bus = VerifierBus::new();
        let n = register_plugin_verifiers_into_bus(&registry, &mut bus);
        assert_eq!(n, 1, "one plugin verifier should register");

        let ctx = VerifyContext {
            sandbox_dir: std::path::PathBuf::from("/tmp/test"),
            changed_files: vec![std::path::PathBuf::from("src/lib.rs")],
        };
        bus.run(&ctx);
        assert_eq!(bus.verdicts().len(), 1);
        let v = &bus.verdicts()[0];
        assert_eq!(v.source, VerifierSource::Plugin("demo-v".into()));
        assert_eq!(v.severity, Severity::Error);
        assert!(v.message.contains("nope"));
    }
<<<<<<< HEAD

    #[test]
    fn as_verifier_parts_returns_none_for_non_verifier_capability() {
        let cap = kf_plugin_host::Capability::Skill {
            trigger: "/x".into(),
            prompt: "do x".into(),
            skill_file: None,
            model_hint: None,
        };
        assert!(as_verifier_parts(&cap).is_none());
    }

    #[test]
    fn as_verifier_parts_returns_none_for_verifier_without_command() {
        let cap = kf_plugin_host::Capability::Verifier {
            name: "no-cmd".into(),
            priority: 1,
            command: None,
        };
        assert!(as_verifier_parts(&cap).is_none());
    }

    #[test]
    fn as_verifier_parts_extracts_fields_when_command_present() {
        let cap = kf_plugin_host::Capability::Verifier {
            name: "fmt".into(),
            priority: 3,
            command: Some(PathBuf::from("bin/fmt.sh")),
        };
        let (name, priority, command) =
            as_verifier_parts(&cap).expect("verifier capability with command should yield parts");
        assert_eq!(name, "fmt");
        assert_eq!(priority, 3);
        assert_eq!(command, PathBuf::from("bin/fmt.sh"));
    }

    #[test]
    fn verifiers_from_registry_returns_empty_for_empty_registry() {
        let registry = PluginRegistry::new();
        let verifiers = verifiers_from_registry(&registry);
        assert!(verifiers.is_empty());
    }

    #[test]
    fn register_plugin_verifiers_into_bus_returns_zero_for_empty_registry() {
        let registry = PluginRegistry::new();
        let mut bus = crate::session::verifier::bus::VerifierBus::new();
        let count = register_plugin_verifiers_into_bus(&registry, &mut bus);
        assert_eq!(count, 0);
        assert_eq!(bus.verifier_count(), 0);
    }

    #[test]
    fn plugin_verifier_adapter_priority_round_trips() {
        let pv = PluginVerifier {
            name: "p".into(),
            command: PathBuf::from("c.sh"),
            plugin_root: PathBuf::from("/tmp"),
        };
        for prio in [0u8, 1, 5, 254, 255] {
            let adapter = PluginVerifierAdapter::new(pv.clone(), prio);
            assert_eq!(adapter.priority(), prio);
            assert_eq!(adapter.name(), "p");
        }
    }
||||||| 15ad6877

    #[test]
    fn as_verifier_parts_returns_none_for_non_verifier_capability() {
        let cap = kf_plugin_sdk::Capability::Skill {
            trigger: "/x".into(),
            prompt: "do x".into(),
            skill_file: None,
            model_hint: None,
        };
        assert!(as_verifier_parts(&cap).is_none());
    }

    #[test]
    fn as_verifier_parts_returns_none_for_verifier_without_command() {
        let cap = kf_plugin_sdk::Capability::Verifier {
            name: "no-cmd".into(),
            priority: 1,
            command: None,
        };
        assert!(as_verifier_parts(&cap).is_none());
    }

    #[test]
    fn as_verifier_parts_extracts_fields_when_command_present() {
        let cap = kf_plugin_sdk::Capability::Verifier {
            name: "fmt".into(),
            priority: 3,
            command: Some(PathBuf::from("bin/fmt.sh")),
        };
        let (name, priority, command) =
            as_verifier_parts(&cap).expect("verifier capability with command should yield parts");
        assert_eq!(name, "fmt");
        assert_eq!(priority, 3);
        assert_eq!(command, PathBuf::from("bin/fmt.sh"));
    }

    #[test]
    fn verifiers_from_registry_returns_empty_for_empty_registry() {
        let registry = PluginRegistry::new();
        let verifiers = verifiers_from_registry(&registry);
        assert!(verifiers.is_empty());
    }

    #[test]
    fn register_plugin_verifiers_into_bus_returns_zero_for_empty_registry() {
        let registry = PluginRegistry::new();
        let mut bus = crate::session::verifier::bus::VerifierBus::new();
        let count = register_plugin_verifiers_into_bus(&registry, &mut bus);
        assert_eq!(count, 0);
        assert_eq!(bus.verifier_count(), 0);
    }

    #[test]
    fn plugin_verifier_adapter_priority_round_trips() {
        let pv = PluginVerifier {
            name: "p".into(),
            command: PathBuf::from("c.sh"),
            plugin_root: PathBuf::from("/tmp"),
        };
        for prio in [0u8, 1, 5, 254, 255] {
            let adapter = PluginVerifierAdapter::new(pv.clone(), prio);
            assert_eq!(adapter.priority(), prio);
            assert_eq!(adapter.name(), "p");
        }
    }
=======
>>>>>>> wo/wo47.14
}
