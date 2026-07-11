//! Runtime host for KirkForge plugins.
//!
//! The host owns the plugin registry, enforces trust tiers, and provides
//! lookup helpers for skills, tools, hooks, and verifiers declared by
//! loaded plugins.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod compat;
mod hook;
mod sandbox;
mod tool;
mod toolset;
mod verifier;

pub use compat::{load_skill_dir, load_skills_dir};
pub use hook::{HookError, HookVerdict, PluginHook};
pub use sandbox::SandboxPolicy;
pub use tool::{PluginTool, ToolError, KIRKFORGE_TOOL_ARGS};
pub use toolset::{CompositeToolset, PluginToolset, ToolInfo, Toolset};
pub use verifier::{PluginVerifier, VerifierError, VerifierVerdict};

use kirkforge_plugin::{Capability, LoadedPlugin, Plugin, PluginManifest, TrustTier};
use std::collections::HashMap;
use std::path::Path;

/// Policy the host applies to all loaded plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustPolicy {
    /// Highest tier the host will allow. Plugins requesting more are
    /// either blocked or downgraded (configurable).
    pub max: TrustTier,
    /// If true, a plugin whose requested tier exceeds `max` is rejected.
    /// If false, its capabilities are capped to `max` (e.g. a `network`
    /// plugin loaded with `max = shell` keeps shell tools but loses
    /// network ones). For v1 we reject by default — least surprise.
    pub reject_on_excess: bool,
    /// If true, every loaded plugin directory must contain a
    /// `.kirkforge.sig` detached signature file that can be verified with
    /// `minisign`. Off by default.
    pub verify_signatures: bool,
    /// Path to the minisign public key used when `verify_signatures` is
    /// true. Verification is skipped entirely if this is `None`.
    pub signature_key_path: Option<std::path::PathBuf>,
}

impl Default for TrustPolicy {
    fn default() -> Self {
        Self {
            max: TrustTier::Shell,
            reject_on_excess: true,
            verify_signatures: false,
            signature_key_path: None,
        }
    }
}

impl TrustPolicy {
    /// Create a policy that allows up to `max` and rejects anything beyond.
    pub fn up_to(max: TrustTier) -> Self {
        Self {
            max,
            reject_on_excess: true,
            verify_signatures: false,
            signature_key_path: None,
        }
    }

    /// Enable or disable detached-minisign signature verification.
    pub fn with_verify_signatures(
        mut self,
        verify: bool,
        key_path: Option<std::path::PathBuf>,
    ) -> Self {
        self.verify_signatures = verify;
        self.signature_key_path = key_path;
        self
    }

    /// Set whether plugins whose trust exceeds `max` are rejected.
    pub fn with_reject_on_excess(mut self, reject: bool) -> Self {
        self.reject_on_excess = reject;
        self
    }
}

/// A plugin together with any trust-policy decision applied to it.
#[derive(Debug, Clone)]
pub struct HostedPlugin {
    pub plugin: LoadedPlugin,
    pub effective_trust: TrustTier,
    /// If `Some`, the plugin was rejected and should not be used.
    pub rejection: Option<String>,
}

impl HostedPlugin {
    /// True if the plugin is allowed to run.
    pub fn is_active(&self) -> bool {
        self.rejection.is_none()
    }
}

/// Registry of all loaded plugins.
#[derive(Debug, Default, Clone)]
pub struct PluginRegistry {
    plugins: Vec<HostedPlugin>,
    skills_by_trigger: HashMap<String, usize>,
    tools_by_name: HashMap<String, usize>,
    hooks_by_event: HashMap<String, Vec<usize>>,
    verifiers_by_name: HashMap<String, usize>,
}

impl PluginRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of active (non-rejected) plugins.
    pub fn active_count(&self) -> usize {
        self.plugins.iter().filter(|p| p.is_active()).count()
    }

    /// Load every plugin directory under `plugins_dir` and apply `policy`.
    ///
    /// A plugin directory must contain a `kirkforge.toml` file. Hidden
    /// directories are skipped.
    pub fn load_from_dir(
        &mut self,
        plugins_dir: &Path,
        policy: TrustPolicy,
    ) -> anyhow::Result<Vec<String>> {
        let mut warnings = Vec::new();
        if !plugins_dir.exists() {
            tracing::debug!(dir = %plugins_dir.display(), "plugins directory does not exist");
            return Ok(warnings);
        }

        let entries = std::fs::read_dir(plugins_dir).map_err(|e| {
            anyhow::anyhow!(
                "cannot read plugins directory {}: {}",
                plugins_dir.display(),
                e
            )
        })?;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('.'))
                .unwrap_or(false)
            {
                continue;
            }
            match LoadedPlugin::load(&path) {
                Ok(plugin) => {
                    if let Err(e) = plugin.manifest().validate_api_version() {
                        warnings.push(format!("{}: {}", path.display(), e));
                        continue;
                    }

                    if policy.verify_signatures {
                        if let Err(e) =
                            verify_plugin_signature(&path, policy.signature_key_path.as_deref())
                        {
                            warnings.push(format!(
                                "{}: signature verification failed: {}",
                                plugin.manifest().name,
                                e
                            ));
                            continue;
                        }
                    }

                    let hosted = apply_policy(plugin, &policy);
                    if let Some(ref reason) = hosted.rejection {
                        warnings.push(format!("{}: {}", hosted.plugin.manifest.name, reason));
                    } else {
                        warnings.extend(self.push_and_index(hosted));
                    }
                }
                Err(e) => {
                    warnings.push(format!("{}: failed to load plugin: {}", path.display(), e));
                }
            }
        }

        Ok(warnings)
    }

    /// Add a hosted plugin to the registry and index its capabilities.
    fn push_and_index(&mut self, hosted: HostedPlugin) -> Vec<String> {
        let idx = self.plugins.len();
        self.plugins.push(hosted);
        self.index_at(idx)
    }

    /// Index capabilities for the plugin at position `idx`. Returns warnings
    /// for duplicate capabilities that silently shadow an existing entry.
    fn index_at(&mut self, idx: usize) -> Vec<String> {
        let mut warnings = Vec::new();
        let Some(hosted) = self.plugins.get(idx) else {
            return warnings;
        };
        let manifest = hosted.plugin.manifest().clone();
        let plugin_name = &manifest.name;

        for cap in &manifest.capabilities {
            match cap {
                Capability::Skill { trigger, .. } => {
                    if let Some(prev) = self.skills_by_trigger.insert(trigger.clone(), idx) {
                        let prev_name = self.plugins[prev].plugin.manifest().name.clone();
                        warnings.push(format!(
                            "skill trigger '{trigger}' from plugin '{plugin_name}' shadows plugin '{prev_name}'"
                        ));
                    }
                }
                Capability::Tool { name, .. } => {
                    if let Some(prev) = self.tools_by_name.insert(name.clone(), idx) {
                        let prev_name = self.plugins[prev].plugin.manifest().name.clone();
                        warnings.push(format!(
                            "tool '{name}' from plugin '{plugin_name}' shadows plugin '{prev_name}'"
                        ));
                    }
                }
                Capability::Hook { event, .. } => {
                    self.hooks_by_event
                        .entry(event.clone())
                        .or_default()
                        .push(idx);
                }
                Capability::Verifier { name, .. } => {
                    if let Some(prev) = self.verifiers_by_name.insert(name.clone(), idx) {
                        let prev_name = self.plugins[prev].plugin.manifest().name.clone();
                        warnings.push(format!(
                            "verifier '{name}' from plugin '{plugin_name}' shadows plugin '{prev_name}'"
                        ));
                    }
                }
            }
        }
        warnings
    }

    /// Rebuild all capability index maps from the plugin vector.
    ///
    /// Used after `remove` because removing a plugin shifts indices of all
    /// later plugins, invalidating the existing maps.
    fn rebuild_indexes(&mut self) {
        self.skills_by_trigger.clear();
        self.tools_by_name.clear();
        self.hooks_by_event.clear();
        self.verifiers_by_name.clear();
        for idx in 0..self.plugins.len() {
            // Warnings from rebuild are not propagated because remove()
            // cannot return them; duplicates here indicate the same
            // capability remained after removing the previous owner.
            let _ = self.index_at(idx);
        }
    }

    /// Load a single plugin directory by path and apply `policy`.
    ///
    /// Returns the plugin name and any duplicate-capability warnings on
    /// success. If the plugin is rejected by the trust policy, returns the
    /// rejection reason as an error.
    pub fn load_one(
        &mut self,
        plugin_dir: &Path,
        policy: TrustPolicy,
    ) -> anyhow::Result<(String, Vec<String>)> {
        let plugin = LoadedPlugin::load(plugin_dir).map_err(|e| {
            anyhow::anyhow!("failed to load plugin from {}: {}", plugin_dir.display(), e)
        })?;

        plugin
            .manifest()
            .validate_api_version()
            .map_err(|e| anyhow::anyhow!("{}: {}", plugin_dir.display(), e))?;

        if policy.verify_signatures {
            verify_plugin_signature(plugin_dir, policy.signature_key_path.as_deref()).map_err(
                |e| {
                    anyhow::anyhow!(
                        "{}: signature verification failed: {}",
                        plugin.manifest().name,
                        e
                    )
                },
            )?;
        }

        let hosted = apply_policy(plugin, &policy);
        if let Some(ref reason) = hosted.rejection {
            anyhow::bail!("{}: {}", hosted.plugin.manifest().name, reason);
        }

        let name = hosted.plugin.manifest().name.clone();
        // Remove any existing plugin with the same name before loading the new one.
        self.remove(&name);
        let warnings = self.push_and_index(hosted);
        Ok((name, warnings))
    }

    /// Remove an active plugin by name.
    ///
    /// Returns true if a plugin was removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let len_before = self.plugins.len();
        self.plugins.retain(|p| p.plugin.manifest().name != name);
        if self.plugins.len() == len_before {
            return false;
        }
        self.rebuild_indexes();
        true
    }

    /// Find an active plugin by name.
    pub fn find_active_by_name(&self, name: &str) -> Option<(&PluginManifest, &dyn Plugin)> {
        let hosted = self
            .plugins
            .iter()
            .find(|p| p.plugin.manifest().name == name && p.is_active())?;
        Some((&hosted.plugin.manifest, &hosted.plugin as &dyn Plugin))
    }

    /// Find an active plugin by skill trigger.
    pub fn skill_by_trigger(&self, trigger: &str) -> Option<(&PluginManifest, &dyn Plugin)> {
        let &idx = self.skills_by_trigger.get(trigger)?;
        let hosted = self.plugins.get(idx)?;
        if !hosted.is_active() {
            return None;
        }
        Some((&hosted.plugin.manifest, &hosted.plugin as &dyn Plugin))
    }

    /// All active skill triggers.
    pub fn skill_triggers(&self) -> Vec<String> {
        self.skills_by_trigger.keys().cloned().collect()
    }

    /// All active plugins.
    pub fn active_plugins(&self) -> Vec<&HostedPlugin> {
        self.plugins.iter().filter(|p| p.is_active()).collect()
    }

    /// Find an active plugin that exposes a tool by name.
    pub fn tool_by_name(&self, name: &str) -> Option<(&PluginManifest, &dyn Plugin)> {
        let &idx = self.tools_by_name.get(name)?;
        let hosted = self.plugins.get(idx)?;
        if !hosted.is_active() {
            return None;
        }
        Some((&hosted.plugin.manifest, &hosted.plugin as &dyn Plugin))
    }

    /// Find all active plugins that expose a hook for `event`.
    pub fn hooks_for_event(&self, event: &str) -> Vec<(&PluginManifest, &dyn Plugin)> {
        let mut out = Vec::new();
        if let Some(idxs) = self.hooks_by_event.get(event) {
            for &idx in idxs {
                if let Some(hosted) = self.plugins.get(idx) {
                    if hosted.is_active() {
                        out.push((&hosted.plugin.manifest, &hosted.plugin as &dyn Plugin));
                    }
                }
            }
        }
        out
    }

    /// Find an active plugin verifier by name.
    pub fn verifier_by_name(&self, name: &str) -> Option<(&PluginManifest, &dyn Plugin)> {
        let &idx = self.verifiers_by_name.get(name)?;
        let hosted = self.plugins.get(idx)?;
        if !hosted.is_active() {
            return None;
        }
        Some((&hosted.plugin.manifest, &hosted.plugin as &dyn Plugin))
    }
}

/// Verify a plugin's detached minisign signature.
///
/// The signature file must be named `.kirkforge.sig` inside the plugin
/// directory and must sign the manifest file `kirkforge.toml`. The
/// configured public key is passed to `minisign -V`.
fn verify_plugin_signature(
    plugin_root: &std::path::Path,
    key_path: Option<&std::path::Path>,
) -> Result<(), String> {
    let sig_path = plugin_root.join(".kirkforge.sig");
    if !sig_path.exists() {
        return Err("missing required .kirkforge.sig signature file".into());
    }

    let key_path = key_path.ok_or_else(|| {
        "signature verification enabled but no plugin_public_key_path configured".to_string()
    })?;

    let manifest_path = plugin_root.join("kirkforge.toml");
    let output = std::process::Command::new("minisign")
        .arg("-V")
        .arg("-m")
        .arg(&manifest_path)
        .arg("-x")
        .arg(&sig_path)
        .arg("-p")
        .arg(key_path)
        .output()
        .map_err(|e| format!("failed to run minisign: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = if stderr.trim().is_empty() {
            format!("minisign exited with {:?}", output.status.code())
        } else {
            stderr.trim().to_string()
        };
        return Err(reason);
    }

    Ok(())
}

/// Apply the trust policy to a freshly loaded plugin.
///
/// Rejected plugins are returned without indexing. Accepted plugins have their
/// capabilities filtered down to those permitted by the effective trust tier.
fn apply_policy(plugin: LoadedPlugin, policy: &TrustPolicy) -> HostedPlugin {
    if policy.reject_on_excess && !policy.max.permits(plugin.manifest.trust) {
        return HostedPlugin {
            effective_trust: plugin.manifest.trust,
            rejection: Some(format!(
                "trust tier '{}' exceeds host maximum '{}'",
                plugin.manifest.trust, policy.max
            )),
            plugin,
        };
    }

    let effective = if policy.max.permits(plugin.manifest.trust) {
        plugin.manifest.trust
    } else {
        policy.max
    };

    let plugin = filter_capabilities_by_tier(plugin, effective);

    HostedPlugin {
        plugin,
        effective_trust: effective,
        rejection: None,
    }
}

/// Remove capabilities from a plugin that require more trust than the
/// effective tier allows.
fn filter_capabilities_by_tier(mut plugin: LoadedPlugin, tier: TrustTier) -> LoadedPlugin {
    let allowed = SandboxPolicy::filter(tier, &plugin.manifest.capabilities);

    plugin.skill_prompts.retain(|trigger, _| {
        allowed
            .iter()
            .any(|cap| matches!(cap, Capability::Skill { trigger: t, .. } if t == trigger))
    });
    plugin
        .hooks
        .retain(|cap| allowed.iter().any(|allowed| allowed == cap));
    plugin
        .verifiers
        .retain(|cap| allowed.iter().any(|allowed| allowed == cap));
    plugin
        .tools
        .retain(|cap| allowed.iter().any(|allowed| allowed == cap));
    plugin.manifest.capabilities = allowed;

    plugin
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirkforge_plugin::TrustTier;

    fn make_test_plugin_dir(root: &Path, trust: TrustTier) {
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::write(
            root.join("kirkforge.toml"),
            format!(
                r#"
name = "test-plugin"
version = "0.1.0"
description = "test"
trust = "{trust}"

[[capabilities]]
type = "skill"
trigger = "/hello"
prompt = "Say hello to {{args}}"

[[capabilities]]
type = "hook"
event = "post-turn"
command = "hooks/post-turn.sh"
"#,
            ),
        )
        .unwrap();
    }

    #[test]
    fn registry_loads_skill_and_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("hello");
        // Hook requires Shell, so the plugin must be at least Shell.
        make_test_plugin_dir(&plugin_dir, TrustTier::Shell);

        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reg.active_count(), 1);
        assert!(reg.skill_by_trigger("/hello").is_some());
        assert!(!reg.hooks_for_event("post-turn").is_empty());
    }

    #[test]
    fn registry_filters_capabilities_below_effective_trust() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("hello");
        make_test_plugin_dir(&plugin_dir, TrustTier::ReadOnly);

        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reg.active_count(), 1);
        // Skill is read-only, hook requires shell and is filtered away.
        assert!(reg.skill_by_trigger("/hello").is_some());
        assert!(reg.hooks_for_event("post-turn").is_empty());
    }

    #[test]
    fn registry_rejects_excessive_trust() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("risky");
        make_test_plugin_dir(&plugin_dir, TrustTier::Unsafe);

        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert_eq!(reg.active_count(), 0);
        assert!(warnings.iter().any(|w| w.contains("exceeds")));
    }

    #[test]
    fn registry_downgrades_excessive_trust_when_reject_on_excess_false() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("downgraded");
        make_test_plugin_dir(&plugin_dir, TrustTier::Network);

        let mut reg = PluginRegistry::new();
        let policy = TrustPolicy::up_to(TrustTier::Shell).with_reject_on_excess(false);
        let warnings = reg.load_from_dir(&plugins, policy).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        // Plugin stays active but its effective trust is capped at Shell.
        assert_eq!(reg.active_count(), 1);
        assert!(reg.skill_by_trigger("/hello").is_some());
        assert!(!reg.hooks_for_event("post-turn").is_empty());
    }

    #[test]
    fn registry_rejects_unsigned_plugin_when_signature_validation_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("unsigned");
        make_test_plugin_dir(&plugin_dir, TrustTier::Shell);

        let mut reg = PluginRegistry::new();
        let key_path = tmp.path().join("plugin.pub");
        std::fs::write(&key_path, "dummy-key").unwrap();
        let policy =
            TrustPolicy::up_to(TrustTier::Shell).with_verify_signatures(true, Some(key_path));
        let warnings = reg.load_from_dir(&plugins, policy).unwrap();
        assert_eq!(reg.active_count(), 0);
        assert!(
            warnings.iter().any(|w| w.contains("signature")),
            "warnings: {warnings:?}"
        );
    }

    #[test]
    fn load_one_loads_single_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("test-plugin");
        make_test_plugin_dir(&plugin_dir, TrustTier::Shell);

        let mut reg = PluginRegistry::new();
        let (name, _warnings) = reg
            .load_one(&plugin_dir, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert_eq!(name, "test-plugin");
        assert_eq!(reg.active_count(), 1);
        assert!(reg.skill_by_trigger("/hello").is_some());
        assert!(!reg.hooks_for_event("post-turn").is_empty());
    }

    #[test]
    fn load_one_rejects_excess_trust() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("risky");
        make_test_plugin_dir(&plugin_dir, TrustTier::Unsafe);

        let mut reg = PluginRegistry::new();
        let err = reg
            .load_one(&plugin_dir, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap_err();
        assert!(err.to_string().contains("exceeds"));
        assert_eq!(reg.active_count(), 0);
    }

    #[test]
    fn remove_deletes_plugin_and_updates_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("test-plugin");
        make_test_plugin_dir(&plugin_dir, TrustTier::Shell);

        let mut reg = PluginRegistry::new();
        reg.load_one(&plugin_dir, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert!(reg.remove("test-plugin"));
        assert_eq!(reg.active_count(), 0);
        assert!(reg.skill_by_trigger("/hello").is_none());
        assert!(reg.hooks_for_event("post-turn").is_empty());
    }

    #[test]
    fn remove_returns_false_when_missing() {
        let mut reg = PluginRegistry::new();
        assert!(!reg.remove("nonexistent"));
    }

    #[test]
    fn load_one_replaces_existing_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("test-plugin");
        make_test_plugin_dir(&plugin_dir, TrustTier::Shell);

        let mut reg = PluginRegistry::new();
        reg.load_one(&plugin_dir, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        reg.load_one(&plugin_dir, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert_eq!(reg.active_count(), 1);
        assert!(reg.skill_by_trigger("/hello").is_some());
    }
}
