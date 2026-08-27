//! Runtime host for KirkForge plugins.
//!
//! The host owns the plugin registry, enforces trust tiers, and provides
//! lookup helpers for skills, tools, hooks, and verifiers declared by
//! loaded plugins.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod compat;
pub mod env;
mod hook;
mod paths;
mod rlimits;
mod sandbox;
pub mod sdk;
mod tool;
mod toolset;
mod verifier;

pub use compat::{load_skill_dir, load_skills_dir};
pub use hook::{HookError, HookVerdict, PluginHook};
pub use sandbox::SandboxPolicy;
pub use tool::{PluginTool, ToolError, KF_CODE_TOOL_ARGS, KF_CODE_TOOL_ARGS_JSON};
pub use toolset::{CompositeToolset, PluginToolset, ToolInfo, Toolset};
pub use verifier::{PluginVerifier, VerifierError, VerifierVerdict};

// The SDK surface (folded from the former `kf-plugin-sdk` crate, WO 47.4)
// re-exported at the host root so consumers can migrate from the
// `kf_plugin_sdk::` alias to `kf_plugin_host::` mechanically.
pub use sdk::{
    ApiVersion, Capability, CapabilityKind, LoadedPlugin, ManifestError, Plugin, PluginManifest,
    ResourceLimits, TrustTier, ValidationError, KNOWN_EVENTS,
};
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
    /// `.kf-code.sig` detached signature file that can be verified with
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
    /// Original number of capabilities in the manifest before
    /// trust-tier filtering (WO 11.3). Used by `/plugins list` to
    /// show how many capabilities were hidden by the downgrade.
    pub original_capability_count: usize,
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
    /// A plugin directory must contain a `kf-code.toml` file. Hidden
    /// directories are skipped. After loading, plugins are sorted into
    /// topological order based on their `depends_on` fields so
    /// dependencies are indexed before dependents (WO 11.2, ADR-058).
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

        // Collect (plugin_dir, plugin) pairs first so we can topologically
        // sort before indexing (WO 11.2: dependencies load before dependents).
        let mut loaded: Vec<(std::path::PathBuf, LoadedPlugin)> = Vec::new();
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

                    // WO 15.2 / M10: reject invalid manifests. Both
                    // load_from_dir and load_one now return errors on
                    // validation failure, consistent behaviour. Before
                    // this, load_from_dir silently accepted bad names, bad
                    // semver, duplicate triggers, unknown hook events, and
                    // untrusted command paths on the production load path.
                    if let Err(errs) = plugin.manifest().validate() {
                        for e in errs {
                            warnings.push(format!("{}: {}", path.display(), e));
                        }
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

                    loaded.push((path, plugin));
                }
                Err(e) => {
                    warnings.push(format!("{}: failed to load plugin: {}", path.display(), e));
                }
            }
        }

        // Topological sort by depends_on (WO 11.2). Missing deps and
        // cycles are reported as warnings; the offending plugin is not
        // indexed.
        let names: std::collections::HashSet<&str> = loaded
            .iter()
            .map(|(_, p)| p.manifest().name.as_str())
            .collect();
        match topological_order(&loaded, &names) {
            Ok(order) => {
                for idx in order {
                    let (_path, plugin) = &loaded[idx];
                    let (hosted, policy_warnings) = apply_policy(plugin.clone(), &policy);
                    warnings.extend(policy_warnings);
                    if let Some(ref reason) = hosted.rejection {
                        warnings.push(format!("{}: {}", hosted.plugin.manifest.name, reason));
                    } else {
                        warnings.extend(self.push_and_index(hosted));
                    }
                }
            }
            Err(e) => {
                warnings.push(e);
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

        // Reject the plugin on manifest validation errors, matching
        // load_from_dir behaviour (M10). Previously load_one loaded the
        // plugin with warnings while load_from_dir skipped it — the
        // asymmetry meant an invalid manifest could be loaded one way but
        // not the other.
        if let Err(errs) = plugin.manifest().validate() {
            let messages = validation_warnings(&plugin, &errs);
            anyhow::bail!("{}", messages.join("; "));
        }
        let mut warnings: Vec<String> = Vec::new();

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

        let (hosted, policy_warnings) = apply_policy(plugin, &policy);
        if let Some(ref reason) = hosted.rejection {
            anyhow::bail!("{}: {}", hosted.plugin.manifest().name, reason);
        }

        let name = hosted.plugin.manifest().name.clone();
        // Remove any existing plugin with the same name before loading the new one.
        self.remove(&name);
        warnings.extend(policy_warnings);
        warnings.extend(self.push_and_index(hosted));
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

/// Topologically sort `loaded` plugins so dependencies come before
/// dependents (WO 11.2, ADR-058). Returns a vector of indices into
/// `loaded` in load order. Returns `Err(message)` if a dependency is
/// missing from `available_names` or a cycle is detected.
fn topological_order(
    loaded: &[(std::path::PathBuf, LoadedPlugin)],
    available_names: &std::collections::HashSet<&str>,
) -> Result<Vec<usize>, String> {
    use std::collections::{HashMap, HashSet};

    let name_to_idx: HashMap<&str, usize> = loaded
        .iter()
        .enumerate()
        .map(|(i, (_, p))| (p.manifest().name.as_str(), i))
        .collect();

    // Check for missing dependencies first.
    for (_, plugin) in loaded {
        for dep in &plugin.manifest().depends_on {
            if !available_names.contains(dep.as_str()) {
                return Err(format!(
                    "{}: depends on '{}' which is not loaded (enable '{}' \
                     or remove the depends_on entry)",
                    plugin.manifest().name,
                    dep,
                    dep
                ));
            }
        }
    }

    // DFS-based topological sort with cycle detection.
    let mut visited: HashSet<usize> = HashSet::new();
    let mut on_stack: HashSet<usize> = HashSet::new();
    let mut order: Vec<usize> = Vec::with_capacity(loaded.len());

    fn visit(
        node: usize,
        loaded: &[(std::path::PathBuf, LoadedPlugin)],
        name_to_idx: &HashMap<&str, usize>,
        visited: &mut HashSet<usize>,
        on_stack: &mut HashSet<usize>,
        order: &mut Vec<usize>,
    ) -> Result<(), String> {
        if visited.contains(&node) {
            return Ok(());
        }
        if on_stack.contains(&node) {
            let name = loaded[node].1.manifest().name.clone();
            return Err(format!("dependency cycle detected at '{name}'"));
        }
        on_stack.insert(node);
        let deps: Vec<usize> = loaded[node]
            .1
            .manifest()
            .depends_on
            .iter()
            .filter_map(|d| name_to_idx.get(d.as_str()).copied())
            .collect();
        for dep in deps {
            visit(dep, loaded, name_to_idx, visited, on_stack, order)?;
        }
        on_stack.remove(&node);
        visited.insert(node);
        order.push(node);
        Ok(())
    }

    let mut sorted_indices: Vec<usize> = (0..loaded.len()).collect();
    sorted_indices.sort_by_key(|&i| loaded[i].1.manifest().name.clone());
    for &start in &sorted_indices {
        if !visited.contains(&start) {
            visit(
                start,
                loaded,
                &name_to_idx,
                &mut visited,
                &mut on_stack,
                &mut order,
            )?;
        }
    }

    Ok(order)
}

/// Verify a plugin's detached minisign signature in-process (ADR-057).
///
/// The signature file must be named `.kf-code.sig` inside the plugin
/// directory and must sign the manifest file `kf-code.toml`. The
/// configured public key is loaded via the pure-Rust `minisign-verify`
/// crate — no `minisign` binary is required in `PATH`.
///
/// Error semantics (unchanged from the shell-out path):
/// - missing `.kf-code.sig` → error
/// - missing/unreadable public key → error
/// - malformed signature or key → error
/// - signature mismatch → error
/// - success → `Ok(())`
fn verify_plugin_signature(
    plugin_root: &std::path::Path,
    key_path: Option<&std::path::Path>,
) -> Result<(), String> {
    let sig_path = plugin_root.join(".kf-code.sig");
    if !sig_path.exists() {
        return Err("missing required .kf-code.sig signature file".into());
    }

    let key_path = key_path.ok_or_else(|| {
        "signature verification enabled but no plugin_public_key_path configured".to_string()
    })?;

    let public_key = minisign_verify::PublicKey::from_file(key_path).map_err(|e| {
        format!(
            "failed to load minisign public key {}: {e}",
            key_path.display()
        )
    })?;

    let signature = minisign_verify::Signature::from_file(&sig_path)
        .map_err(|e| format!("failed to load signature {}: {e}", sig_path.display()))?;

    let manifest_path = plugin_root.join("kf-code.toml");
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| format!("failed to read manifest {}: {e}", manifest_path.display()))?;

    // `verify` third arg = `allow_legacy`. Minisign's default non-prehashed
    // signatures verify with `false`; legacy (prehash-off) keys also verify.
    public_key
        .verify(&manifest_bytes, &signature, true)
        .map_err(|e| format!("signature verification failed: {e}"))
}

/// Apply the trust policy to a freshly loaded plugin.
///
/// Rejected plugins are returned without indexing. Accepted plugins have their
/// capabilities filtered down to those permitted by the effective trust tier and
/// to command paths that stay inside the plugin root. Returns any warnings
/// produced while filtering.
fn apply_policy(plugin: LoadedPlugin, policy: &TrustPolicy) -> (HostedPlugin, Vec<String>) {
    let mut warnings = Vec::new();
    let original_capability_count = plugin.manifest.capabilities.len();
    if policy.reject_on_excess && !policy.max.permits(plugin.manifest.trust) {
        let hosted = HostedPlugin {
            effective_trust: plugin.manifest.trust,
            rejection: Some(format!(
                "trust tier '{}' exceeds host maximum '{}'",
                plugin.manifest.trust, policy.max
            )),
            plugin,
            original_capability_count,
        };
        return (hosted, warnings);
    }

    let effective = if policy.max.permits(plugin.manifest.trust) {
        plugin.manifest.trust
    } else {
        policy.max
    };

    let plugin = filter_capabilities(plugin, effective, &mut warnings);

    let hosted = HostedPlugin {
        plugin,
        effective_trust: effective,
        rejection: None,
        original_capability_count,
    };
    (hosted, warnings)
}

/// Remove capabilities from a plugin that require more trust than the
/// effective tier allows, drop any capability whose command path would escape
/// the plugin root, and drop any capability whose command file does not exist.
///
/// Command paths are canonicalised so symlinks inside the plugin root that
/// point outside it are also rejected.
fn filter_capabilities(
    mut plugin: LoadedPlugin,
    tier: TrustTier,
    warnings: &mut Vec<String>,
) -> LoadedPlugin {
    let allowed = SandboxPolicy::filter(tier, &plugin.manifest.capabilities);
    let root = plugin.root.clone();
    let canonical_root = match std::fs::canonicalize(&root) {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!(
                "cannot canonicalise plugin root '{}': {e}; dropping all capabilities",
                root.display()
            ));
            plugin.skill_prompts.clear();
            plugin.hooks.clear();
            plugin.verifiers.clear();
            plugin.tools.clear();
            plugin.manifest.capabilities.clear();
            return plugin;
        }
    };

    let mut validated = Vec::with_capacity(allowed.len());
    for cap in allowed {
        if let Some(cmd) = paths::capability_command(&cap) {
            let abs = match root.join(cmd).canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    warnings.push(format!(
                        "{}: command path '{}' is not accessible: {e}; dropping capability",
                        capability_label(&cap),
                        cmd.display()
                    ));
                    continue;
                }
            };
            if !abs.starts_with(&canonical_root) {
                warnings.push(format!(
                    "{}: command path '{}' resolves outside plugin root; dropping capability",
                    capability_label(&cap),
                    cmd.display()
                ));
                continue;
            }
        }
        validated.push(cap);
    }

    plugin.skill_prompts.retain(|trigger, _| {
        validated
            .iter()
            .any(|cap| matches!(cap, Capability::Skill { trigger: t, .. } if t == trigger))
    });
    plugin
        .hooks
        .retain(|cap| validated.iter().any(|allowed| allowed == cap));
    plugin
        .verifiers
        .retain(|cap| validated.iter().any(|allowed| allowed == cap));
    plugin
        .tools
        .retain(|cap| validated.iter().any(|allowed| allowed == cap));
    plugin.manifest.capabilities = validated;

    plugin
}

/// Human-readable identifier for a capability, used in warnings.
fn capability_label(cap: &Capability) -> String {
    match cap {
        Capability::Skill { trigger, .. } => format!("skill '{trigger}'"),
        Capability::Tool { name, .. } => format!("tool '{name}'"),
        Capability::Hook { event, .. } => format!("hook '{event}'"),
        Capability::Verifier { name, .. } => format!("verifier '{name}'"),
    }
}

/// Format manifest-level validation errors as load warnings. Prefixes
/// each error with the plugin name so a multi-plugin load can attribute
/// issues to the right plugin.
fn validation_warnings(plugin: &LoadedPlugin, errors: &[ValidationError]) -> Vec<String> {
    let name = &plugin.manifest().name;
    errors
        .iter()
        .map(|e| format!("{name}: manifest validation: {e}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::TrustTier;

    fn make_test_plugin_dir(root: &Path, trust: TrustTier) {
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::write(
            root.join("kf-code.toml"),
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
        std::fs::write(root.join("hooks/post-turn.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(root.join("hooks/post-turn.sh"))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(root.join("hooks/post-turn.sh"), perms).unwrap();
        }
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
    fn registry_drops_capability_with_command_outside_root() {
        // WO 15.2: load_from_dir now calls validate() before indexing.
        // validate() rejects the `../evil.sh` command path (parent-dir
        // segment), so the whole plugin is skipped with a validation
        // warning. Both load_from_dir and load_one reject invalid
        // manifests (M10).
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("bad");
        std::fs::create_dir_all(plugin_dir.join("tools")).unwrap();
        std::fs::write(plugin_dir.join("tools/ok.sh"), "#!/bin/sh\nprintf ok\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(plugin_dir.join("tools/ok.sh"))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(plugin_dir.join("tools/ok.sh"), perms).unwrap();
        }
        // Create the file outside the plugin root so canonicalisation can
        // resolve the relative escape and confirm it leaves the root.
        std::fs::write(plugins.join("evil.sh"), "#!/bin/sh\n").unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "bad"
version = "0.1.0"
description = "bad"
trust = "shell"

[[capabilities]]
type = "tool"
name = "bad/escape"
description = "escapes plugin root"
command = "../evil.sh"

[[capabilities]]
type = "tool"
name = "bad/ok"
description = "stays inside plugin root"
command = "tools/ok.sh"
"#,
        )
        .unwrap();

        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert_eq!(
            reg.active_count(),
            0,
            "plugin with a `..` command path is skipped by validate()"
        );
        assert!(
            reg.tool_by_name("bad/escape").is_none(),
            "escaped tool must not be indexed"
        );
        assert!(
            reg.tool_by_name("bad/ok").is_none(),
            "whole plugin skipped, so valid tool is also absent"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("bad") && w.contains("command")),
            "expected a command validation warning, got: {warnings:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_one_rejects_capability_with_absolute_command() {
        // M10: validate() rejects absolute command paths; load_one now
        // returns Err instead of loading with warnings.
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("bad");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "bad"
version = "0.1.0"
description = "bad"
trust = "shell"

[[capabilities]]
type = "tool"
name = "bad/escape"
description = "escapes plugin root"
command = "/bin/sh"
"#,
        )
        .unwrap();

        let mut reg = PluginRegistry::new();
        let err = reg
            .load_one(&plugin_dir, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap_err();
        assert!(
            err.to_string().contains("manifest validation"),
            "expected manifest validation error, got: {err}"
        );
    }

    #[test]
    fn registry_drops_tool_with_missing_command_file() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("missing");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "missing"
version = "0.1.0"
description = "missing"
trust = "shell"

[[capabilities]]
type = "tool"
name = "missing/tool"
description = "missing command"
command = "tools/missing.sh"
"#,
        )
        .unwrap();

        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert_eq!(reg.active_count(), 1);
        assert!(
            reg.tool_by_name("missing/tool").is_none(),
            "missing tool command should be dropped"
        );
        assert!(
            warnings.iter().any(|w| w.contains("is not accessible")),
            "expected missing-file warning, got: {warnings:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn registry_drops_tool_with_symlink_escaping_root() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("bad-symlink");
        std::fs::create_dir_all(plugin_dir.join("tools")).unwrap();
        // A symlink inside the plugin root that points to a file outside it.
        symlink("/bin/sh", plugin_dir.join("tools/escape.sh")).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "bad-symlink"
version = "0.1.0"
description = "bad"
trust = "shell"

[[capabilities]]
type = "tool"
name = "bad/escape"
description = "escapes via symlink"
command = "tools/escape.sh"
"#,
        )
        .unwrap();

        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert_eq!(reg.active_count(), 1);
        assert!(
            reg.tool_by_name("bad/escape").is_none(),
            "symlink-escaped tool should be dropped"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("resolves outside plugin root")),
            "expected symlink-escape warning, got: {warnings:?}"
        );
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

    #[test]
    fn load_one_rejects_invalid_manifest() {
        // M10: load_one must reject invalid manifests, matching
        // load_from_dir. Previously load_one loaded the plugin with
        // warnings while load_from_dir skipped it.
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("bad-valid8");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "Bad_Name"
version = "not-semver"
description = "intentionally broken"
trust = "shell"

[[capabilities]]
type = "tool"
name = "x"
command = "/bin/sh"

[[capabilities]]
type = "skill"
trigger = "/dup"
prompt = "first"

[[capabilities]]
type = "skill"
trigger = "/dup"
prompt = "second"
"#,
        )
        .unwrap();

        let mut reg = PluginRegistry::new();
        let err = reg
            .load_one(&plugin_dir, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("manifest validation"),
            "expected manifest validation error, got: {msg}"
        );
        assert!(msg.contains("name"), "error: {msg}");
        assert!(msg.contains("version"), "error: {msg}");
        assert!(msg.contains("capabilities[0].command"), "error: {msg}");
        assert!(msg.contains("capabilities[2].trigger"), "error: {msg}");
    }

    #[test]
    fn load_from_dir_surfaces_invalid_manifest_and_skips_plugin() {
        // WO 15.2 / M10: load_from_dir must call validate() and skip the
        // plugin on error. Both load_from_dir and load_one reject invalid
        // manifests consistently. Before this fix the bulk-load path
        // silently accepted a bad name. Uses a bad name (`Bad Name!`) so
        // the only validation error is the name check; the path prefix
        // and the "name" path must both appear in the warnings.
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("bad-name");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "Bad Name!"
version = "0.1.0"
description = "bad name"
trust = "read-only"
"#,
        )
        .unwrap();

        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::ReadOnly))
            .unwrap();
        assert_eq!(reg.active_count(), 0, "invalid plugin should be skipped");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("bad-name") && w.contains("name")),
            "expected a name validation warning prefixed by the plugin path, got: {warnings:?}"
        );
    }

    mod signature_tests {
        use super::*;
        use crate::sdk::TrustTier;
        use minisign::KeyPair;
        use std::io::Write;

        /// Generate a real minisign keypair, sign `kf-code.toml` in
        /// `plugin_dir`, write `.kf-code.sig`, and return the public
        /// key file path. Uses the pure-Rust `minisign` crate
        /// (dev-dependency) so tests don't need the `minisign` binary.
        ///
        /// `keys_dir` is where the keypair is written — it MUST be
        /// outside the `plugins/` tree, otherwise `load_from_dir`
        /// treats the key directory as a plugin.
        fn sign_plugin(plugin_dir: &Path, keys_dir: &Path, manifest: &str) -> std::path::PathBuf {
            std::fs::create_dir_all(keys_dir).unwrap();
            let pk_path = keys_dir.join("plugin.pub");
            let sk_path = keys_dir.join("plugin.key");

            let KeyPair { pk, sk } = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
            std::fs::write(&pk_path, pk.to_box().unwrap().to_string()).unwrap();
            std::fs::write(&sk_path, sk.to_box(None).unwrap().to_string()).unwrap();

            std::fs::write(plugin_dir.join("kf-code.toml"), manifest).unwrap();
            let sig_box = minisign::sign(
                None,
                &sk,
                std::io::Cursor::new(manifest.as_bytes()),
                None,
                None,
            )
            .unwrap();
            let mut sig_file = std::fs::File::create(plugin_dir.join(".kf-code.sig")).unwrap();
            sig_file.write_all(sig_box.to_string().as_bytes()).unwrap();
            pk_path
        }

        fn valid_manifest() -> &'static str {
            r#"
name = "signed-plugin"
version = "0.1.0"
description = "signed"
trust = "shell"

[[capabilities]]
type = "skill"
trigger = "/hello"
prompt = "hi"
"#
        }

        #[test]
        fn verify_accepts_valid_signature() {
            let tmp = tempfile::tempdir().unwrap();
            let plugin_dir = tmp.path().join("plugin");
            std::fs::create_dir_all(&plugin_dir).unwrap();
            let pk_path = sign_plugin(&plugin_dir, &tmp.path().join("keys"), valid_manifest());

            verify_plugin_signature(&plugin_dir, Some(&pk_path))
                .expect("valid signature should verify in-process");
        }

        #[test]
        fn verify_rejects_missing_sig_file() {
            let tmp = tempfile::tempdir().unwrap();
            let plugin_dir = tmp.path().join("plugin");
            std::fs::create_dir_all(&plugin_dir).unwrap();
            std::fs::write(plugin_dir.join("kf-code.toml"), valid_manifest()).unwrap();
            let pk_path = tmp.path().join("key.pub");
            std::fs::write(&pk_path, "dummy").unwrap();

            let err = verify_plugin_signature(&plugin_dir, Some(&pk_path))
                .unwrap_err()
                .to_lowercase();
            assert!(err.contains("missing"), "{err}");
            assert!(err.contains(".kf-code.sig"), "{err}");
        }

        #[test]
        fn verify_rejects_missing_key_path() {
            let tmp = tempfile::tempdir().unwrap();
            let plugin_dir = tmp.path().join("plugin");
            std::fs::create_dir_all(&plugin_dir).unwrap();
            std::fs::write(plugin_dir.join("kf-code.toml"), valid_manifest()).unwrap();
            std::fs::write(plugin_dir.join(".kf-code.sig"), "garbage").unwrap();

            let err = verify_plugin_signature(&plugin_dir, None).unwrap_err();
            assert!(err.contains("no plugin_public_key_path"), "{err}");
        }

        #[test]
        fn verify_rejects_malformed_signature() {
            let tmp = tempfile::tempdir().unwrap();
            let plugin_dir = tmp.path().join("plugin");
            std::fs::create_dir_all(&plugin_dir).unwrap();
            let pk_path = sign_plugin(&plugin_dir, &tmp.path().join("keys"), valid_manifest());
            // Corrupt the signature file.
            std::fs::write(plugin_dir.join(".kf-code.sig"), "not a minisign sig\n").unwrap();

            let err = verify_plugin_signature(&plugin_dir, Some(&pk_path))
                .unwrap_err()
                .to_lowercase();
            assert!(err.contains("signature"), "{err}");
        }

        #[test]
        fn verify_rejects_wrong_key() {
            let tmp = tempfile::tempdir().unwrap();
            let plugin_dir = tmp.path().join("plugin");
            std::fs::create_dir_all(&plugin_dir).unwrap();
            let _real_pk = sign_plugin(&plugin_dir, &tmp.path().join("keys"), valid_manifest());

            // Generate a second, unrelated keypair and verify with that pk.
            let KeyPair { pk, sk: _ } = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
            let wrong_pk_path = tmp.path().join("wrong.pub");
            std::fs::write(&wrong_pk_path, pk.to_box().unwrap().to_string()).unwrap();

            let err = verify_plugin_signature(&plugin_dir, Some(&wrong_pk_path))
                .unwrap_err()
                .to_lowercase();
            assert!(err.contains("verification failed"), "{err}");
        }

        #[test]
        fn verify_rejects_tampered_manifest() {
            let tmp = tempfile::tempdir().unwrap();
            let plugin_dir = tmp.path().join("plugin");
            std::fs::create_dir_all(&plugin_dir).unwrap();
            let pk_path = sign_plugin(&plugin_dir, &tmp.path().join("keys"), valid_manifest());
            // Tamper with the manifest after signing.
            std::fs::write(
                plugin_dir.join("kf-code.toml"),
                "name = \"tampered\"\nversion = \"9.9.9\"\n",
            )
            .unwrap();

            let err = verify_plugin_signature(&plugin_dir, Some(&pk_path))
                .unwrap_err()
                .to_lowercase();
            assert!(err.contains("verification failed"), "{err}");
        }

        /// Full registry load with signature verification enabled and a
        /// valid signature succeeds; the plugin is active.
        #[test]
        fn registry_loads_signed_plugin_when_verification_enabled() {
            let tmp = tempfile::tempdir().unwrap();
            let plugins_dir = tmp.path().join("plugins");
            let plugin_dir = plugins_dir.join("signed");
            std::fs::create_dir_all(&plugin_dir).unwrap();
            let pk_path = sign_plugin(&plugin_dir, &tmp.path().join("keys"), valid_manifest());

            let mut reg = PluginRegistry::new();
            let policy =
                TrustPolicy::up_to(TrustTier::Shell).with_verify_signatures(true, Some(pk_path));
            let warnings = reg.load_from_dir(&plugins_dir, policy).unwrap();
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(reg.active_count(), 1);
            assert!(reg.skill_by_trigger("/hello").is_some());
        }
    }
}

#[cfg(test)]
mod load_order_tests {
    use super::*;
    use crate::sdk::TrustTier;

    fn make_plugin(root: &Path, name: &str, deps: &[&str]) {
        std::fs::create_dir_all(root).unwrap();
        let dep_str = if deps.is_empty() {
            String::new()
        } else {
            format!("\ndepends_on = {deps:?}")
        };
        std::fs::write(
            root.join("kf-code.toml"),
            format!(
                r#"
name = "{name}"
version = "0.1.0"
description = "test"
trust = "read-only"{dep_str}
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn load_order_empty_deps_preserves_name_order() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        make_plugin(&plugins.join("alpha"), "alpha", &[]);
        make_plugin(&plugins.join("beta"), "beta", &[]);
        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::ReadOnly))
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reg.active_count(), 2);
    }

    #[test]
    fn load_order_missing_dependency_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        make_plugin(&plugins.join("dependent"), "dependent", &["missing-dep"]);
        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::ReadOnly))
            .unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("depends on") && w.contains("missing-dep")),
            "expected missing-dep warning, got: {warnings:?}"
        );
        assert_eq!(reg.active_count(), 0);
    }

    #[test]
    fn load_order_cycle_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        make_plugin(&plugins.join("a"), "a", &["b"]);
        make_plugin(&plugins.join("b"), "b", &["a"]);
        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::ReadOnly))
            .unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("cycle")),
            "expected cycle warning, got: {warnings:?}"
        );
    }

    #[test]
    fn load_order_transitive_loads_all() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        make_plugin(&plugins.join("a"), "a", &[]);
        make_plugin(&plugins.join("b"), "b", &["a"]);
        make_plugin(&plugins.join("c"), "c", &["b"]);
        let mut reg = PluginRegistry::new();
        let warnings = reg
            .load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::ReadOnly))
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reg.active_count(), 3);
        // All three should be active; the topological order is internal
        // (a before b before c), but the test asserts they all loaded.
    }
}
