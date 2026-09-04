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
//! the failure message. The plugin-host `PluginVerifier` implements this
//! convention; the bus-side `PluginBusVerifier` (bus.rs) is the adapter.

fn as_verifier_parts(
    cap: &kf_plugin_host::sdk::Capability,
) -> Option<(String, u8, std::path::PathBuf)> {
    match cap {
        kf_plugin_host::sdk::Capability::Verifier {
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
    use kf_plugin_host::sdk::Plugin;
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
        let cap = kf_plugin_host::sdk::Capability::Skill {
            trigger: "/x".into(),
            prompt: "do x".into(),
            skill_file: None,
            model_hint: None,
        };
        assert!(as_verifier_parts(&cap).is_none());
    }

    #[test]
    fn as_verifier_parts_returns_none_for_verifier_without_command() {
        let cap = kf_plugin_host::sdk::Capability::Verifier {
            name: "no-cmd".into(),
            priority: 1,
            command: None,
        };
        assert!(as_verifier_parts(&cap).is_none());
    }

    #[test]
    fn as_verifier_parts_extracts_fields_when_command_present() {
        let cap = kf_plugin_host::sdk::Capability::Verifier {
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

    #[test]
    fn register_plugin_verifiers_into_bus_returns_zero_for_empty_registry() {
        let registry = kf_plugin_host::PluginRegistry::new();
        let mut bus = crate::session::verifier::bus::VerifierBus::new();
        let count = register_plugin_verifiers_into_bus(&registry, &mut bus);
        assert_eq!(count, 0);
        assert_eq!(bus.verifier_count(), 0);
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
                kf_plugin_host::TrustPolicy::up_to(kf_plugin_host::sdk::TrustTier::Shell),
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
}
