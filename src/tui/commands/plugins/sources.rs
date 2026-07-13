//! Workspace plugin-source management for `/plugins`.
//!
//! Extracted from `mod.rs`: the `setup`, `sources`, `add`, and `remove`
//! subcommands plus path resolution. These read/write the shared config
//! and drive a registry reload; `write_shared_config` stays in the parent
//! because `toggle_plugin` shares it.

use super::{
    active_plugin_names, blocked_warnings, plugin_status_summary, reload_plugins,
    write_shared_config,
};
use crate::shared::read_shared_config;
use crate::tui::app::AppState;
use kirkforge_plugin_host::PluginRegistry;
use std::path::PathBuf;
use tokio::sync::mpsc;

/// `setup` — show a quick-start message for workspace plugin sources.
pub(super) async fn setup_plugin_sources(
    state: &AppState,
    _plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    let cfg = read_shared_config(&state.config);
    let mut lines = vec![
        "Workspace plugin source setup:".to_string(),
        "  /plugins add <name> <path>   — register a plugin directory".to_string(),
        "  /plugins remove <name>       — unregister a source".to_string(),
        "  /plugins toggle <name>        — enable or disable a source (persists)".to_string(),
        "  /plugins sources             — list configured sources".to_string(),
        "  /plugins reload              — rescan and apply current config".to_string(),
        String::new(),
    ];
    if cfg.plugin_sources.is_empty() {
        lines.push("No workspace sources configured yet.".to_string());
    } else {
        lines.push(format!(
            "Configured sources ({}):",
            cfg.plugin_sources.len()
        ));
        for (name, path) in &cfg.plugin_sources {
            let enabled = if cfg.enabled_plugins.iter().any(|n| n == name) {
                "on"
            } else {
                "off"
            };
            lines.push(format!("  - {name} -> {} [{enabled}]", path.display()));
        }
    }
    lines.join("\n")
}

/// `sources` — list configured workspace plugin sources and their enabled state.
pub(super) fn list_sources(state: &AppState) -> String {
    let cfg = read_shared_config(&state.config);
    if cfg.plugin_sources.is_empty() {
        return "No workspace plugin sources configured. Use /plugins add <name> <path>."
            .to_string();
    }

    let active = active_plugin_names(&state.plugin_registry);
    let mut lines = Vec::new();
    lines.push(format!(
        "Workspace plugin sources ({}):",
        cfg.plugin_sources.len()
    ));
    for (name, path) in &cfg.plugin_sources {
        let enabled = cfg.enabled_plugins.iter().any(|n| n == name);
        let status = match (enabled, active.contains(name)) {
            (true, true) => "enabled, active",
            (true, false) => "enabled, inactive",
            (false, _) => "disabled",
        };
        lines.push(format!("  - {name} -> {} [{status}]", path.display()));
    }
    lines.join("\n")
}

/// `add <name> <path>` — register a workspace plugin source.
pub(super) async fn add_source(
    name: &str,
    path: &str,
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    let resolved = resolve_source_path(path);
    if !resolved.exists() {
        return format!(
            "❌ Plugin source path does not exist: {resolved}",
            resolved = resolved.display()
        );
    }
    if !resolved.is_dir() {
        return format!(
            "❌ Plugin source path is not a directory: {resolved}",
            resolved = resolved.display()
        );
    }

    {
        let mut cfg = write_shared_config(&state.config);
        cfg.plugin_sources
            .insert(name.to_string(), resolved.clone());
        if let Err(e) = crate::session::config::save_config(&cfg) {
            return format!("❌ Failed to save config while adding source '{name}': {e}");
        }
    }

    reload_plugins(state, plugin_reload_tx).await
}

/// `remove <name>` — unregister a workspace plugin source.
pub(super) fn remove_source(
    name: &str,
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    {
        let mut cfg = write_shared_config(&state.config);
        if cfg.plugin_sources.remove(name).is_none() {
            return format!("❌ No workspace plugin source named '{name}'.");
        }
        cfg.enabled_plugins.retain(|n| n != name);
        if let Err(e) = crate::session::config::save_config(&cfg) {
            return format!("❌ Failed to save config while removing source '{name}': {e}");
        }
    }

    // Unload the plugin if it is currently active; the registry will not be
    // re-loaded from this source on the next /plugins reload either.
    state.skill_registry.remove_plugin(name);
    state.plugin_registry.remove(name);
    state.plugin_status = plugin_status_summary(&state.plugin_registry, &blocked_warnings(state));
    crate::send_or_warn!(
        plugin_reload_tx.send(state.plugin_registry.clone()),
        "plugin registry receiver dropped; executor may have exited"
    );

    format!("🔌 Removed workspace plugin source '{name}'.")
}

/// Resolve a workspace source path relative to the current directory.
pub(super) fn resolve_source_path(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(p)
    } else {
        p
    }
}
