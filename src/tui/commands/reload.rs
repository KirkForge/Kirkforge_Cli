//! `/reload` slash-command handler.
//!
//! Re-reads `config.toml` (with environment-variable layering), updates
//! the TUI's shared config in place, and forwards a snapshot to the
//! executor so it rebuilds access-control structures. The executor emits
//! the user-visible confirmation token.
//!
//! `/reload plugins` re-scans the plugin directory and forwards a fresh
//! `PluginRegistry` to the executor so it can swap the plugin toolset,
//! hooks, and verifiers between turns.

use crate::shared::{read_shared_config, Config};
use crate::tui::app::AppState;
use kirkforge_plugin_host::PluginRegistry;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Handle `/reload` command.
pub async fn handle_reload_command(
    config_tx: &mpsc::UnboundedSender<Config>,
    state: &mut AppState,
) -> String {
    let fresh = crate::session::config::load_config();
    let before = read_shared_config(&state.config).clone();
    let diff_summary = crate::session::config::config_diff_summary(&before, &fresh);

    // Take the old Arc out of state so the lock operation borrows a
    // local, not `state.config`. If the lock is poisoned, the local is
    // dropped and `state.config` already points at a fresh, un-poisoned
    // lock. This prevents a poisoned lock from wedging the TUI.
    let old_arc = std::mem::replace(
        &mut state.config,
        Arc::new(std::sync::RwLock::new(fresh.clone())),
    );
    let write_ok = match old_arc.write() {
        Ok(mut cfg) => {
            *cfg = fresh.clone();
            true
        }
        Err(_) => false,
    };
    if write_ok {
        // On success, keep the updated old_arc so any other holders of
        // the same Arc see the new config; on poison, the replacement
        // above is the source of truth.
        state.config = old_arc;
    }

    // Forward the new snapshot to the executor. The executor owns the
    // access-control rebuild and emits the actual confirmation token.
    if config_tx.send(fresh).is_err() {
        return "❌ Config reloaded in TUI, but executor is not running.".into();
    }

    if diff_summary.is_empty() {
        "🔄 Reloaded config (no changes)".into()
    } else {
        format!("🔄 Reloaded config: {diff_summary}")
    }
}

/// Handle `/reload skills` command.
///
/// Re-scans registered skill paths and re-registers built-in skills on top.
/// Returns a short summary for the TUI chat panel.
pub fn handle_reload_skills_command(state: &mut AppState) -> String {
    let before = state.skill_registry.len();
    state.skill_registry.clear();
    state.skill_registry.set_max_plugin_trust(
        read_shared_config(&state.config).max_plugin_trust,
    );
    let scanned = state.skill_registry.scan_and_load().unwrap_or(0);
    for skill in crate::session::skills::builtin_skills() {
        state.skill_registry.register(skill);
    }
    let after = state.skill_registry.len();
    format!(
        "🧠 Reloaded skills: cleared {before}, rescanned {scanned}, now {after} registered."
    )
}

/// Handle `/reload plugins` command.
///
/// Re-scans the configured plugins directory using the current trust policy,
/// forwards the fresh registry to the executor over the plugin-reload
/// control channel, and returns a short summary for the TUI chat panel.
pub async fn handle_reload_plugins_command(
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
    state: &mut AppState,
) -> String {
    let cfg = read_shared_config(&state.config).clone();
    let (registry, warnings) = match crate::session::plugin_tools::load_plugin_registry(&cfg) {
        Ok(r) => r,
        Err(e) => return format!("❌ Plugin reload failed: {e}"),
    };

    // Refresh the skill/plugin status summary in the status bar.
    state.skill_registry.set_max_plugin_trust(cfg.max_plugin_trust);
    if let Err(e) = state.skill_registry.scan_and_load() {
        tracing::warn!(error = %e, "skill rescan during /reload plugins failed");
    }
    // Always re-register built-in skills on top.
    for skill in crate::session::skills::builtin_skills() {
        state.skill_registry.register(skill);
    }
    state.plugin_status = state.skill_registry.plugin_status_summary();

    if plugin_reload_tx.send(registry).is_err() {
        return "❌ Plugins re-scanned, but executor is not running.".into();
    }

    if warnings.is_empty() {
        "🔌 Plugin reload requested.".into()
    } else {
        format!(
            "🔌 Plugin reload requested. Warnings: {}",
            warnings.join("; ")
        )
    }
}
