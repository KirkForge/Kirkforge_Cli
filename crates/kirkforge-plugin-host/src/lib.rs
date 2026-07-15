//! Runtime host for KirkForge plugins.
//!
//! The host owns the plugin registry, enforces trust tiers, and provides
//! lookup helpers for skills, tools, hooks, and verifiers declared by
//! loaded plugins.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod compat;
mod env;
mod hook;
mod paths;
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

                    let (hosted, policy_warnings) = apply_policy(plugin, &policy);
                    warnings.extend(policy_warnings);
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

        let (hosted, policy_warnings) = apply_policy(plugin, &policy);
        if let Some(ref reason) = hosted.rejection {
            anyhow::bail!("{}: {}", hosted.plugin.manifest().name, reason);
        }

        let name = hosted.plugin.manifest().name.clone();
        // Remove any existing plugin with the same name before loading the new one.
        self.remove(&name);
        let mut warnings = policy_warnings;
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

/// Locate the `minisign` binary in `PATH`.
///
/// Returns an absolute path so that the verifier does not spawn an
/// unqualified command that could be hijacked by a malicious current
/// directory. The search mirrors how `Command` resolves names, minus the
/// current-directory fallback.
fn minisign_binary(path_env: Option<&std::ffi::OsStr>) -> Result<std::path::PathBuf, String> {
    let path_env = path_env.ok_or_else(|| "PATH not set; cannot locate minisign".to_string())?;
    for dir in std::env::split_paths(path_env) {
        let candidate = {
            #[allow(unused_mut)]
            let mut c = dir.join("minisign");
            #[cfg(windows)]
            {
                c.set_extension("exe");
            }
            c
        };
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("minisign binary not found in PATH; install minisign to verify plugin signatures".into())
}

/// Verify a plugin's detached minisign signature.
///
/// The signature file must be named `.kirkforge.sig` inside the plugin
/// directory and must sign the manifest file `kirkforge.toml`. The
/// configured public key is passed to `minisign -V`.
///
/// Before spawning `minisign` we confirm the binary exists in `PATH` and
/// canonicalize the manifest path so relative roots and symlinks cannot
/// cause the signature to be checked against an unintended file.
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

    let minisign = minisign_binary(std::env::var_os("PATH").as_deref())?;

    let manifest_path = plugin_root
        .join("kirkforge.toml")
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize manifest path: {e}"))?;

    let output = std::process::Command::new(&minisign)
        .arg("-V")
        .arg("-m")
        .arg(&manifest_path)
        .arg("-x")
        .arg(&sig_path)
        .arg("-p")
        .arg(key_path)
        .output()
        .map_err(|e| format!("failed to run minisign ({}): {e}", minisign.display()))?;

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
/// capabilities filtered down to those permitted by the effective trust tier and
/// to command paths that stay inside the plugin root. Returns any warnings
/// produced while filtering.
fn apply_policy(plugin: LoadedPlugin, policy: &TrustPolicy) -> (HostedPlugin, Vec<String>) {
    let mut warnings = Vec::new();
    if policy.reject_on_excess && !policy.max.permits(plugin.manifest.trust) {
        let hosted = HostedPlugin {
            effective_trust: plugin.manifest.trust,
            rejection: Some(format!(
                "trust tier '{}' exceeds host maximum '{}'",
                plugin.manifest.trust, policy.max
            )),
            plugin,
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
            plugin_dir.join("kirkforge.toml"),
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
        assert_eq!(reg.active_count(), 1);
        assert!(
            reg.tool_by_name("bad/escape").is_none(),
            "escaped tool should be dropped"
        );
        assert!(
            reg.tool_by_name("bad/ok").is_some(),
            "valid tool should be kept"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("resolves outside plugin root")),
            "expected command-escape warning, got: {warnings:?}"
        );
    }

    #[test]
    fn load_one_warns_when_capability_command_escapes_root() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("bad");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kirkforge.toml"),
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
        let (_name, warnings) = reg
            .load_one(&plugin_dir, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("resolves outside plugin root")),
            "expected command-escape warning, got: {warnings:?}"
        );
        assert!(reg.tool_by_name("bad/escape").is_none());
    }

    #[test]
    fn registry_drops_tool_with_missing_command_file() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("missing");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kirkforge.toml"),
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
            plugin_dir.join("kirkforge.toml"),
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

    #[cfg(unix)]
    mod signature_tests {
        use super::*;
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Mutex;

        static PATH_LOCK: Mutex<()> = Mutex::new(());

        struct PathEnvGuard {
            previous: Option<std::ffi::OsString>,
        }

        impl PathEnvGuard {
            fn new(value: std::ffi::OsString) -> Self {
                let previous = std::env::var_os("PATH");
                std::env::set_var("PATH", value);
                Self { previous }
            }
        }

        impl Drop for PathEnvGuard {
            fn drop(&mut self) {
                if let Some(ref p) = self.previous {
                    std::env::set_var("PATH", p);
                } else {
                    std::env::remove_var("PATH");
                }
            }
        }

        fn make_fake_minisign(bin_dir: &Path) -> std::path::PathBuf {
            let fake = bin_dir.join("minisign");
            // Records the command-line arguments to the path supplied by the
            // test via `MINISIGN_RECORD_ARGS`, then exits successfully.
            let script = b"#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$MINISIGN_RECORD_ARGS\"\nexit 0\n";
            std::fs::write(&fake, script).unwrap();
            let mut perms = std::fs::metadata(&fake).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake, perms).unwrap();
            fake
        }

        #[test]
        fn minisign_binary_resolves_in_path() {
            let _lock = PATH_LOCK.lock().unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let bin_dir = tmp.path().join("bin");
            std::fs::create_dir(&bin_dir).unwrap();
            let fake = bin_dir.join("minisign");
            std::fs::write(&fake, "").unwrap();

            let found = minisign_binary(Some(bin_dir.as_os_str())).unwrap();
            assert_eq!(found, fake);
        }

        #[test]
        fn minisign_binary_not_found_returns_clear_error() {
            let err = minisign_binary(Some(OsStr::new("")))
                .unwrap_err()
                .to_lowercase();
            assert!(err.contains("minisign binary not found"), "{err}");
        }

        #[test]
        fn verify_plugin_signature_canonicalizes_manifest_path() {
            let _lock = PATH_LOCK.lock().unwrap();
            let tmp = tempfile::tempdir().unwrap();

            // Real plugin directory containing the manifest and signature.
            let real_root = tmp.path().join("real");
            std::fs::create_dir(&real_root).unwrap();
            std::fs::write(real_root.join("kirkforge.toml"), "name = \"test\"\n").unwrap();
            std::fs::write(real_root.join(".kirkforge.sig"), "sig").unwrap();

            // A symlink pointing at the real directory; the verifier must
            // canonicalize the manifest path so it resolves the real file.
            let link_root = tmp.path().join("link");
            std::os::unix::fs::symlink(&real_root, &link_root).unwrap();

            let key_path = tmp.path().join("key.pub");
            std::fs::write(&key_path, "key").unwrap();

            let bin_dir = tmp.path().join("bin");
            std::fs::create_dir(&bin_dir).unwrap();
            let args_file = tmp.path().join("args.txt");
            make_fake_minisign(&bin_dir);

            let _guard = PathEnvGuard::new(bin_dir.into_os_string());
            std::env::set_var("MINISIGN_RECORD_ARGS", &args_file);

            verify_plugin_signature(&link_root, Some(&key_path)).unwrap();

            let args = std::fs::read_to_string(&args_file).unwrap();
            let manifest_arg = real_root.join("kirkforge.toml").canonicalize().unwrap();
            assert!(
                args.lines()
                    .any(|line| line == manifest_arg.to_str().unwrap()),
                "expected canonical manifest path {manifest_arg:?} in args:\n{args}"
            );
        }

        #[test]
        fn verify_plugin_signature_reports_missing_minisign() {
            let _lock = PATH_LOCK.lock().unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let plugin_dir = tmp.path().join("plugin");
            std::fs::create_dir(&plugin_dir).unwrap();
            std::fs::write(plugin_dir.join("kirkforge.toml"), "name = \"test\"\n").unwrap();
            std::fs::write(plugin_dir.join(".kirkforge.sig"), "sig").unwrap();
            let key_path = tmp.path().join("key.pub");
            std::fs::write(&key_path, "key").unwrap();

            // PATH points at an empty directory, so minisign cannot be found.
            let empty_path = tmp.path().join("empty");
            std::fs::create_dir(&empty_path).unwrap();
            let _guard = PathEnvGuard::new(empty_path.into_os_string());

            let err = verify_plugin_signature(&plugin_dir, Some(&key_path))
                .unwrap_err()
                .to_lowercase();
            assert!(err.contains("minisign binary not found"), "{err}");
        }
    }
}
