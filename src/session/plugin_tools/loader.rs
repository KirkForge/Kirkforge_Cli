//! Plugin loader functions: resolve the plugins directory, build the trust
//! policy from config, and load plugins into a `PluginRegistry`.
//!
//! This is the "plugin loader hub" that couples to config, access, the plugin
//! host crate, and the `PluginToolWrapper` defined in [`super::wrapper`].
//!
//! ## Two-path dispatch (ADR-050)
//!
//! Folded plugins (Stratum, Plugin3, Draw, Video) can run as either:
//! - **Compiled-in** (feature on): tools register as direct Rust calls in
//!   `main/mod.rs`; the shell plugin dir is skipped here.
//! - **External** (feature off): the shell plugin dir loads here as
//!   `PluginToolWrapper` shell-outs (graceful degradation).
//!
//! The `enabled_plugins` config is the single toggle for both paths.

use crate::shared::{Config, SharedConfig};
use crate::tools::Tool;
use kirkforge_plugin::{Capability, Plugin};
use kirkforge_plugin_host::{PluginRegistry, TrustPolicy};
use std::path::PathBuf;
use std::sync::Arc;

use super::wrapper::PluginToolWrapper;

/// Names of plugins that have been folded into core behind feature flags.
///
/// When the corresponding feature is enabled, these are served by compiled-in
/// Rust code and their shell plugin dirs are skipped during filesystem loading.
/// When the feature is disabled, the shell plugin dir is loaded as fallback.
const FOLDED_PLUGINS: &[(&str, &str)] = &[
    ("stratum", "stratum"),
    ("kirkforge-plugin3", "budget"),
    ("kirkforge-draw", "draw"),
    ("kirkforge-video", "video"),
];

/// Check if a plugin name is folded and whether its feature is compiled in.
pub fn folded_feature_enabled(name: &str) -> bool {
    match name {
        #[cfg(feature = "stratum")]
        "stratum" => true,
        #[cfg(feature = "budget")]
        "kirkforge-plugin3" => true,
        #[cfg(feature = "draw")]
        "kirkforge-draw" => true,
        #[cfg(feature = "video")]
        "kirkforge-video" => true,
        _ => false,
    }
}

/// Check if a plugin name is one of the folded plugins (regardless of feature).
pub fn is_folded(name: &str) -> bool {
    FOLDED_PLUGINS.iter().any(|(n, _)| *n == name)
}

/// Get the feature name for a folded plugin, if any.
pub fn folded_feature(name: &str) -> Option<&'static str> {
    FOLDED_PLUGINS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, f)| *f)
}

/// Default plugins directory: `~/.local/share/kirkforge/plugins/`.
pub fn plugins_dir() -> PathBuf {
    crate::session::data_dir()
        .map(|d| d.join("plugins"))
        .unwrap_or_else(|_| PathBuf::from(".local/share/kirkforge/plugins"))
}

/// Build the host trust policy from the current config snapshot.
pub fn trust_policy_from_config(cfg: &Config) -> TrustPolicy {
    TrustPolicy {
        max: cfg.tools.max_plugin_trust,
        reject_on_excess: cfg.tools.reject_on_excess_plugin_trust,
        verify_signatures: cfg.tools.plugin_signature_validation,
        signature_key_path: cfg.tools.plugin_public_key_path.as_ref().map(PathBuf::from),
    }
}

/// Load enabled workspace plugin sources into an existing registry.
///
/// Workspace plugins are declared in `cfg.tools.plugin_sources` and toggled via
/// `cfg.tools.enabled_plugins`. They load with the same trust policy as data-dir
/// plugins. Warnings are returned for missing directories or rejected trust
/// tiers; the plugin itself is not added to the registry if it fails to load.
pub fn load_workspace_plugins(registry: &mut PluginRegistry, cfg: &Config) -> Vec<String> {
    let policy = trust_policy_from_config(cfg);
    let mut warnings = Vec::new();

    for name in &cfg.tools.enabled_plugins {
        // Folded plugins with feature ON are served by compiled-in Rust code
        // (registered in main/mod.rs). Skip the shell-plugin dir so the two
        // paths don't double-register the same tool names. When the feature is
        // OFF, fall through to the shell-plugin path (graceful degradation).
        if folded_feature_enabled(name) {
            tracing::debug!(
                plugin = %name,
                "folded plugin feature is on — skipping shell-plugin load (compiled-in)"
            );
            continue;
        }

        let Some(path) = cfg.tools.plugin_sources.get(name) else {
            warnings.push(format!("{name}: enabled but no plugin_source configured"));
            continue;
        };
        let resolved = if path.is_absolute() {
            path.clone()
        } else {
            match std::env::current_dir() {
                Ok(cwd) => cwd.join(path),
                Err(e) => {
                    warnings.push(format!(
                        "{name}: cannot resolve relative plugin source {path}: {e}",
                        path = path.display()
                    ));
                    continue;
                }
            }
        };
        let resolved = if resolved.exists() {
            resolved
        } else {
            // Production install fallback: the compile-time workspace paths only
            // exist when running from the source tree. Installed releases ship
            // bundled plugins under the data directory (`~/.local/share/kirkforge/plugins`).
            plugins_dir().join(name)
        };
        if !resolved.exists() {
            warnings.push(format!(
                "{name}: plugin source directory does not exist: {resolved}",
                resolved = resolved.display()
            ));
            continue;
        }
        match registry.load_one(&resolved, policy.clone()) {
            Ok((_, plugin_warnings)) => warnings.extend(plugin_warnings),
            Err(e) => warnings.push(format!("{name}: {e}")),
        }
    }

    warnings
}

/// Load the plugin registry from the configured plugins directory and any
/// enabled workspace plugin sources.
///
/// Returns the registry together with any load warnings (e.g. rejected or
/// signature-invalid plugins, missing workspace sources).
pub fn load_plugin_registry(cfg: &Config) -> anyhow::Result<(PluginRegistry, Vec<String>)> {
    let dir = plugins_dir();
    let mut registry = PluginRegistry::new();
    let mut warnings = registry.load_from_dir(&dir, trust_policy_from_config(cfg))?;
    warnings.extend(load_workspace_plugins(&mut registry, cfg));
    Ok((registry, warnings))
}

/// Create `Tool` implementations for all active plugin tools in `registry`.
pub fn all_plugin_tools(
    registry: &PluginRegistry,
    shared_config: SharedConfig,
) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    for hosted in registry.active_plugins() {
        let root = hosted.plugin.root().to_path_buf();
        for cap in hosted.plugin.tools() {
            if let Capability::Tool {
                name,
                description,
                schema,
                command: Some(cmd),
            } = cap
            {
                let wrapper = PluginToolWrapper::new(
                    name,
                    description,
                    schema,
                    root.clone(),
                    cmd,
                    shared_config.clone(),
                );
                tools.push(Arc::new(wrapper));
            }
        }
    }

    tools
}
