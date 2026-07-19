//! Plugin loader functions: resolve the plugins directory, build the trust
//! policy from config, and load plugins into a `PluginRegistry`.
//!
//! This is the "plugin loader hub" that couples to config, access, the plugin
//! host crate, and the `PluginToolWrapper` defined in [`super::wrapper`].

use crate::shared::{Config, SharedConfig};
use crate::tools::Tool;
use kirkforge_plugin::{Capability, Plugin};
use kirkforge_plugin_host::{PluginRegistry, TrustPolicy};
use std::path::PathBuf;
use std::sync::Arc;

use super::wrapper::PluginToolWrapper;

/// Default plugins directory: `~/.local/share/kirkforge/plugins/`.
pub fn plugins_dir() -> PathBuf {
    crate::session::data_dir()
        .map(|d| d.join("plugins"))
        .unwrap_or_else(|_| PathBuf::from(".local/share/kirkforge/plugins"))
}

/// Build the host trust policy from the current config snapshot.
pub fn trust_policy_from_config(cfg: &Config) -> TrustPolicy {
    TrustPolicy {
        max: cfg.max_plugin_trust,
        reject_on_excess: cfg.reject_on_excess_plugin_trust,
        verify_signatures: cfg.plugin_signature_validation,
        signature_key_path: cfg.plugin_public_key_path.as_ref().map(PathBuf::from),
    }
}

/// Load enabled workspace plugin sources into an existing registry.
///
/// Workspace plugins are declared in `cfg.plugin_sources` and toggled via
/// `cfg.enabled_plugins`. They load with the same trust policy as data-dir
/// plugins. Warnings are returned for missing directories or rejected trust
/// tiers; the plugin itself is not added to the registry if it fails to load.
pub fn load_workspace_plugins(registry: &mut PluginRegistry, cfg: &Config) -> Vec<String> {
    let policy = trust_policy_from_config(cfg);
    let mut warnings = Vec::new();

    for name in &cfg.enabled_plugins {
        let Some(path) = cfg.plugin_sources.get(name) else {
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
