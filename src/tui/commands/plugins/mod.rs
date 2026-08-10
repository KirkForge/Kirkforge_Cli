//! `/plugins` slash-command family — runtime plugin mount/unmount.
//!
//! Subcommands:
//! - `list` — show active, blocked, and available plugin directories.
//! - `enable <name>` — load a plugin directory from `data_dir/plugins/<name>`.
//! - `disable <name>` — unload a named plugin and remove its skills.
//! - `reload` — full rescan of the plugins directory.
//! - `trust <name> <tier>` — session-only re-enable with a specific trust tier.

use crate::shared::{read_shared_config, write_shared_config};
use crate::tui::app::AppState;
use kf_plugin_host::{PluginRegistry, TrustPolicy};
use kf_plugin_sdk::TrustTier;
use std::path::PathBuf;
use tokio::sync::mpsc;

mod sources;
#[cfg(test)]
use sources::resolve_source_path;
use sources::{add_source, list_sources, remove_source, setup_plugin_sources};

/// Operation requested by `/plugins ...`.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginsOp {
    List,
    Enable { name: String },
    Disable { name: String },
    Toggle { name: String },
    Reload,
    Trust { name: String, tier: String },
    Setup,
    Sources,
    Add { name: String, path: String },
    Remove { name: String },
}

/// Parse `/plugins` arguments into an operation.
pub fn parse(args: &str) -> Result<PluginsOp, String> {
    let mut tokens = args.split_whitespace();
    let cmd = tokens.next().unwrap_or("list");

    match cmd {
        "list" | "" => Ok(PluginsOp::List),
        "enable" => {
            let name = tokens
                .next()
                .ok_or("Usage: /plugins enable <name>")?
                .to_string();
            Ok(PluginsOp::Enable { name })
        }
        "disable" => {
            let name = tokens
                .next()
                .ok_or("Usage: /plugins disable <name>")?
                .to_string();
            Ok(PluginsOp::Disable { name })
        }
        "toggle" => {
            let name = tokens
                .next()
                .ok_or("Usage: /plugins toggle <name>")?
                .to_string();
            Ok(PluginsOp::Toggle { name })
        }
        "reload" => Ok(PluginsOp::Reload),
        "setup" => Ok(PluginsOp::Setup),
        "sources" => Ok(PluginsOp::Sources),
        "add" => {
            let name = tokens
                .next()
                .ok_or("Usage: /plugins add <name> <path>")?
                .to_string();
            let path = tokens
                .next()
                .ok_or("Usage: /plugins add <name> <path>")?
                .to_string();
            Ok(PluginsOp::Add { name, path })
        }
        "remove" => {
            let name = tokens
                .next()
                .ok_or("Usage: /plugins remove <name>")?
                .to_string();
            Ok(PluginsOp::Remove { name })
        }
        "trust" => {
            let name = tokens
                .next()
                .ok_or("Usage: /plugins trust <name> <tier>")?
                .to_string();
            let tier = tokens
                .next()
                .ok_or("Usage: /plugins trust <name> <tier>")?
                .to_string();
            Ok(PluginsOp::Trust { name, tier })
        }
        _ => Err(format!(
            "Unknown /plugins subcommand '{cmd}'. Usage: /plugins list | enable <name> | disable <name> | toggle <name> | reload | trust <name> <tier> | setup | sources | add <name> <path> | remove <name>",
        )),
    }
}

/// Handle `/plugins` slash commands.
pub async fn handle_plugins_command(
    args: &str,
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    match parse(args) {
        Ok(PluginsOp::List) => list_plugins(state),
        Ok(PluginsOp::Enable { name }) => enable_plugin(&name, state, plugin_reload_tx).await,
        Ok(PluginsOp::Disable { name }) => disable_plugin(&name, state, plugin_reload_tx),
        Ok(PluginsOp::Toggle { name }) => toggle_plugin(&name, state, plugin_reload_tx).await,
        Ok(PluginsOp::Reload) => reload_plugins(state, plugin_reload_tx).await,
        Ok(PluginsOp::Trust { name, tier }) => {
            trust_plugin(&name, &tier, state, plugin_reload_tx).await
        }
        Ok(PluginsOp::Setup) => setup_plugin_sources(state, plugin_reload_tx).await,
        Ok(PluginsOp::Sources) => list_sources(state),
        Ok(PluginsOp::Add { name, path }) => {
            add_source(&name, &path, state, plugin_reload_tx).await
        }
        Ok(PluginsOp::Remove { name }) => remove_source(&name, state, plugin_reload_tx),
        Err(e) => e,
    }
}

/// `list` — show active, blocked, and available plugin directories.
fn list_plugins(state: &AppState) -> String {
    let active_names = active_plugin_names(&state.provider.plugin_registry);
    let warnings = blocked_warnings(state);

    let mut lines = Vec::new();

    let active = state.provider.plugin_registry.active_plugins();
    if active.is_empty() {
        lines.push("Active plugins: none".to_string());
    } else {
        lines.push(format!("Active plugins ({}):", active.len()));
        for hosted in active {
            let name = &hosted.plugin.manifest.name;
            let manifest_trust = hosted.plugin.manifest.trust;
            let effective = hosted.effective_trust;
            // WO 11.3: show effective trust when it differs from manifest.
            let trust_label = if effective != manifest_trust {
                format!("{manifest_trust} (effective: {effective})",)
            } else {
                format!("{effective}")
            };
            // WO 11.3: show filtered-capability count when a downgrade
            // removed capabilities. `original_capability_count` is the
            // manifest's original count; the current manifest.capabilities
            // is the surviving set (filter_capabilities mutates it).
            let surviving = hosted.plugin.manifest.capabilities.len();
            let filtered = hosted.original_capability_count.saturating_sub(surviving);
            let filtered_label = if filtered > 0 {
                format!(" [filtered: {filtered} capabilities hidden by trust tier]")
            } else {
                String::new()
            };
            lines.push(format!("  - {name} ({trust_label}){filtered_label}"));
        }
    }

    if warnings.is_empty() {
        lines.push("Blocked plugins: none".to_string());
    } else {
        lines.push(format!("Blocked plugins ({}):", warnings.len()));
        for warning in &warnings {
            lines.push(format!("  - {warning}"));
        }
    }

    match available_plugin_dirs(&active_names) {
        Ok(dirs) if dirs.is_empty() => lines.push("Available plugin directories: none".to_string()),
        Ok(dirs) => {
            lines.push(format!("Available plugin directories ({}):", dirs.len()));
            for dir in dirs {
                lines.push(format!("  - {dir}"));
            }
        }
        Err(e) => lines.push(format!("Available plugin directories: {e}")),
    }

    let cfg = read_shared_config(&state.services.config);
    let disabled: std::collections::HashSet<&String> = cfg.tools.disabled_plugins.iter().collect();

    if cfg.tools.plugin_sources.is_empty() {
        lines.push("Workspace plugin sources: none (use /plugins add <name> <path>)".to_string());
    } else {
        lines.push(format!(
            "Workspace plugin sources ({}):",
            cfg.tools.plugin_sources.len()
        ));
        let enabled: std::collections::HashSet<&String> =
            cfg.tools.enabled_plugins.iter().collect();
        for (name, path) in &cfg.tools.plugin_sources {
            let enabled = enabled.contains(name);
            let is_compiled = crate::session::plugin_tools::folded_feature_enabled(name);
            let is_folded = crate::session::plugin_tools::is_folded(name);
            let feature_gate = crate::session::plugin_tools::folded_feature(name);
            let is_disabled = disabled.contains(name);

            let source_label = if is_compiled {
                "compiled-in"
            } else if is_folded {
                "external (feature off)"
            } else {
                "external"
            };

            let feature_label = if let Some(feat) = feature_gate {
                if is_compiled {
                    format!("[{feat}: on]")
                } else {
                    format!("[{feat}: off]")
                }
            } else {
                String::new()
            };

            let status = if is_disabled {
                "disabled"
            } else if enabled {
                if is_compiled {
                    "on (compiled-in)"
                } else if active_names.contains(name) {
                    "on"
                } else {
                    "on (not loaded)"
                }
            } else {
                "off"
            };

            let line = if feature_label.is_empty() {
                format!(
                    "  - {name} -> {} [{status}] ({source_label})",
                    path.display()
                )
            } else {
                format!(
                    "  - {name} -> {} [{status}] ({source_label}) {feature_label}",
                    path.display()
                )
            };
            lines.push(line);
        }
    }

    if !disabled.is_empty() {
        let mut names: Vec<&&String> = disabled.iter().collect();
        names.sort();
        lines.push(format!("Runtime disabled plugins ({}):", disabled.len()));
        for name in names {
            lines.push(format!("  - {name}"));
        }
    }

    lines.join("\n")
}

/// `enable <name>` — load a plugin directory and register its skills.
async fn enable_plugin(
    name: &str,
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    let cfg = read_shared_config(&state.services.config).clone();
    let dir = plugin_dir(name);
    let policy = TrustPolicy::up_to(cfg.tools.max_plugin_trust)
        .with_reject_on_excess(cfg.tools.reject_on_excess_plugin_trust);

    let (loaded_name, load_warnings) = match state.provider.plugin_registry.load_one(&dir, policy) {
        Ok(r) => r,
        Err(e) => return format!("❌ Failed to enable plugin '{name}': {e}"),
    };
    for w in load_warnings {
        tracing::warn!(warning = %w, "plugin load warning");
    }

    // Replace any stale skills from a previous load of the same plugin.
    state.services.skill_registry.remove_plugin(&loaded_name);

    let skills_added = if let Some((manifest, plugin)) = state
        .provider
        .plugin_registry
        .find_active_by_name(&loaded_name)
    {
        state.services.skill_registry.add_plugin(manifest, plugin)
    } else {
        0
    };

    state.provider.plugin_status =
        plugin_status_summary(&state.provider.plugin_registry, &blocked_warnings(state));
    crate::send_or_warn!(
        plugin_reload_tx.send(state.provider.plugin_registry.clone()),
        "plugin registry receiver dropped; executor may have exited"
    );

    let hosted = state
        .provider
        .plugin_registry
        .active_plugins()
        .into_iter()
        .find(|p| p.plugin.manifest.name == loaded_name);
    let trust = hosted
        .map(|p| p.effective_trust.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    format!("🔌 Enabled plugin '{loaded_name}' ({trust}) with {skills_added} skill(s).")
}

/// `disable <name>` — unload a plugin and remove its skills.
fn disable_plugin(
    name: &str,
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    if state
        .provider
        .plugin_registry
        .find_active_by_name(name)
        .is_none()
    {
        return format!("❌ Plugin '{name}' is not active.");
    }

    state.services.skill_registry.remove_plugin(name);
    state.provider.plugin_registry.remove(name);

    state.provider.plugin_status =
        plugin_status_summary(&state.provider.plugin_registry, &blocked_warnings(state));
    crate::send_or_warn!(
        plugin_reload_tx.send(state.provider.plugin_registry.clone()),
        "plugin registry receiver dropped; executor may have exited"
    );

    format!("🔌 Disabled plugin '{name}'.")
}

/// `reload` — full rescan of the plugins directory.
async fn reload_plugins(
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    let cfg = read_shared_config(&state.services.config).clone();
    let before = state.provider.plugin_registry.active_count();

    let (registry, warnings) = match crate::session::plugin_tools::load_plugin_registry(&cfg) {
        Ok(r) => r,
        Err(e) => return format!("❌ Plugin reload failed: {e}"),
    };

    state.provider.plugin_registry = registry;

    // Rebuild the skill registry from scratch so it matches the fresh registry.
    state.services.skill_registry.clear();
    state
        .services
        .skill_registry
        .set_max_plugin_trust(cfg.tools.max_plugin_trust);
    if let Err(e) = state.services.skill_registry.scan_and_load(&cfg) {
        tracing::warn!(error = %e, "skill rescan during /plugins reload failed");
    }
    for skill in crate::session::skills::builtin_skills() {
        state.services.skill_registry.register(skill);
    }

    let after = state.provider.plugin_registry.active_count();
    state.provider.plugin_status = state.services.skill_registry.plugin_status_summary();

    crate::send_or_warn!(
        plugin_reload_tx.send(state.provider.plugin_registry.clone()),
        "plugin registry receiver dropped; executor may have exited"
    );

    if warnings.is_empty() {
        format!("🔌 Reloaded plugins: {before} active before, {after} active now.")
    } else {
        format!(
            "🔌 Reloaded plugins: {before} active before, {after} active now. Warnings: {}",
            warnings.join("; ")
        )
    }
}

/// `trust <name> <tier>` — session-only re-enable with a specific tier.
async fn trust_plugin(
    name: &str,
    tier_str: &str,
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    let tier = match parse_tier(tier_str) {
        Ok(t) => t,
        Err(e) => return format!("❌ {e}"),
    };

    // Remove the current load (if any) so we can re-apply the trust policy.
    state.services.skill_registry.remove_plugin(name);
    state.provider.plugin_registry.remove(name);

    let dir = plugin_dir(name);
    let policy = TrustPolicy::up_to(tier);

    let (loaded_name, load_warnings) = match state.provider.plugin_registry.load_one(&dir, policy) {
        Ok(r) => r,
        Err(e) => return format!("❌ Failed to set trust tier for '{name}': {e}"),
    };
    for w in load_warnings {
        tracing::warn!(warning = %w, "plugin load warning");
    }

    let skills_added = if let Some((manifest, plugin)) = state
        .provider
        .plugin_registry
        .find_active_by_name(&loaded_name)
    {
        state.services.skill_registry.add_plugin(manifest, plugin)
    } else {
        0
    };

    state.provider.plugin_status =
        plugin_status_summary(&state.provider.plugin_registry, &blocked_warnings(state));
    crate::send_or_warn!(
        plugin_reload_tx.send(state.provider.plugin_registry.clone()),
        "plugin registry receiver dropped; executor may have exited"
    );

    format!("🔌 Set trust tier for plugin '{loaded_name}' to {tier} ({skills_added} skill(s)).")
}

/// Resolve `data_dir/plugins/<name>`.
fn plugin_dir(name: &str) -> PathBuf {
    crate::session::plugin_tools::plugins_dir().join(name)
}

/// Collect names of all active plugins.
fn active_plugin_names(registry: &PluginRegistry) -> std::collections::HashSet<String> {
    registry
        .active_plugins()
        .into_iter()
        .map(|p| p.plugin.manifest.name.clone())
        .collect()
}

/// Plugin warnings that are not stale because the plugin is now active.
fn blocked_warnings(state: &AppState) -> Vec<String> {
    let active = active_plugin_names(&state.provider.plugin_registry);
    state
        .services
        .skill_registry
        .plugin_warnings()
        .iter()
        .filter(|w| {
            // Drop warnings for plugins that have since been enabled manually.
            // Warnings are either "name: reason" or "path: reason", so we
            // compare against the last path component of the prefix.
            let subject = w.split(':').next().unwrap_or(w);
            let subject_name = subject.split('/').next_back().unwrap_or(subject);
            !active.iter().any(|name| name == subject_name)
        })
        .cloned()
        .collect()
}

/// List plugin directories under `data_dir/plugins` that are not currently active.
fn available_plugin_dirs(
    active_names: &std::collections::HashSet<String>,
) -> anyhow::Result<Vec<String>> {
    let base = crate::session::plugin_tools::plugins_dir();
    if !base.exists() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    for entry in std::fs::read_dir(&base)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if active_names.contains(name) {
            continue;
        }
        names.push(name.to_string());
    }
    names.sort();
    Ok(names)
}

/// Parse a trust tier string. The host crate does not expose a `TryFrom`
/// for `TrustTier`, so we map the canonical kebab-case names locally.
fn parse_tier(s: &str) -> Result<TrustTier, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "read-only" | "readonly" | "read_only" => Ok(TrustTier::ReadOnly),
        "shell" => Ok(TrustTier::Shell),
        "network" => Ok(TrustTier::Network),
        "unsafe" => Ok(TrustTier::Unsafe),
        _ => Err(format!(
            "unknown trust tier '{s}'; use read-only, shell, network, or unsafe"
        )),
    }
}

/// Compact status summary like the skill registry's, but driven from the
/// executor-facing `PluginRegistry` and the current warning set.
fn plugin_status_summary(registry: &PluginRegistry, warnings: &[String]) -> Option<String> {
    let active = registry.active_plugins();
    if active.is_empty() && warnings.is_empty() {
        return None;
    }

    let mut read_only = 0usize;
    let mut shell = 0usize;
    let mut network = 0usize;
    let mut unsafe_ = 0usize;

    for hosted in active {
        match hosted.effective_trust {
            TrustTier::ReadOnly => read_only += 1,
            TrustTier::Shell => shell += 1,
            TrustTier::Network => network += 1,
            TrustTier::Unsafe => unsafe_ += 1,
        }
    }

    let mut parts = Vec::new();
    if read_only > 0 {
        parts.push(format!("🔒{read_only}"));
    }
    if shell > 0 {
        parts.push(format!("⚡{shell}"));
    }
    if network > 0 {
        parts.push(format!("🌐{network}"));
    }
    if unsafe_ > 0 {
        parts.push(format!("☠️{unsafe_}"));
    }

    let blocked = warnings.len();
    if blocked > 0 {
        parts.push(format!("☠️{blocked} blocked"));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// `toggle <name>` — persistently enable/disable a plugin at runtime.
///
/// For workspace plugin sources (those in `plugin_sources`), this toggles
/// the `enabled_plugins` list AND the `disabled_plugins` set so the change
/// takes effect both at the loader level and at the runtime filter level.
///
/// For compiled-in plugins (stratum, budget), this toggles
/// `disabled_plugins` only — the feature flag controls compilation, but
/// the runtime toggle controls whether the compiled code is active.
async fn toggle_plugin(
    name: &str,
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    let is_workspace_source = {
        let cfg = read_shared_config(&state.services.config);
        cfg.tools.plugin_sources.contains_key(name)
    };

    let was_disabled;
    {
        let mut cfg = write_shared_config(&state.services.config);

        was_disabled = cfg.tools.disabled_plugins.contains(name);
        if was_disabled {
            cfg.tools.disabled_plugins.remove(name);
        } else {
            cfg.tools.disabled_plugins.insert(name.to_string());
        }

        if is_workspace_source {
            let was_enabled = cfg.tools.enabled_plugins.iter().any(|n| n == name);
            if was_enabled {
                cfg.tools.enabled_plugins.retain(|n| n != name);
            } else {
                cfg.tools.enabled_plugins.push(name.to_string());
            }
        } else if !was_disabled {
            // Disabling a non-workspace plugin: nothing else to do,
            // `disabled_plugins` already contains the name.
        } else {
            // Enabling a non-workspace plugin: ensure it's not in disabled_plugins.
            // (Already handled above by removing from disabled_plugins.)
        }

        if let Err(e) = crate::session::config::save_config(&cfg) {
            return format!("❌ Failed to save config while toggling '{name}': {e}");
        }
    }

    let result = reload_plugins(state, plugin_reload_tx).await;

    let status = if was_disabled { "enabled" } else { "disabled" };
    let restart_notice = if crate::session::plugin_tools::folded_feature_enabled(name) {
        " (takes effect on next launch for compiled-in tools)"
    } else {
        ""
    };
    format!("🔌 Plugin '{name}' is now {status}.{restart_notice} {result}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::app_state;

    /// Sets `KF_CODE_DATA_DIR` to `dir` for the lifetime of the guard.
    /// Uses the crate-wide `test_data_dir_lock()` so every test that mutates
    /// the data directory is serialized against every other such test.
    struct TempDataDir {
        prev: Option<std::ffi::OsString>,
        _guard: tokio::sync::MutexGuard<'static, ()>,
    }

    impl TempDataDir {
        async fn new(dir: &std::path::Path) -> Self {
            let guard = crate::session::test_data_dir_lock().lock().await;
            let prev = std::env::var_os("KF_CODE_DATA_DIR");
            std::env::set_var("KF_CODE_DATA_DIR", dir.as_os_str());
            Self {
                prev,
                _guard: guard,
            }
        }
    }

    impl Drop for TempDataDir {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("KF_CODE_DATA_DIR", v),
                None => std::env::remove_var("KF_CODE_DATA_DIR"),
            }
        }
    }

    fn dummy_reload_tx() -> mpsc::UnboundedSender<PluginRegistry> {
        let (tx, _rx) = mpsc::unbounded_channel();
        tx
    }

    #[test]
    fn parse_list() {
        assert_eq!(parse("").unwrap(), PluginsOp::List);
        assert_eq!(parse("list").unwrap(), PluginsOp::List);
        assert_eq!(parse("  list  ").unwrap(), PluginsOp::List);
    }

    #[test]
    fn parse_enable_disable_reload() {
        assert_eq!(
            parse("enable foo").unwrap(),
            PluginsOp::Enable {
                name: "foo".to_string()
            }
        );
        assert_eq!(
            parse("disable bar").unwrap(),
            PluginsOp::Disable {
                name: "bar".to_string()
            }
        );
        assert_eq!(parse("reload").unwrap(), PluginsOp::Reload);
    }

    #[test]
    fn parse_trust() {
        assert_eq!(
            parse("trust demo shell").unwrap(),
            PluginsOp::Trust {
                name: "demo".to_string(),
                tier: "shell".to_string()
            }
        );
    }

    #[test]
    fn parse_rejects_unknown_subcommand() {
        let err = parse("frobnicate").unwrap_err();
        assert!(err.contains("Unknown /plugins subcommand"));
    }

    #[test]
    fn parse_rejects_missing_arguments() {
        assert!(parse("enable").unwrap_err().contains("Usage:"));
        assert!(parse("disable").unwrap_err().contains("Usage:"));
        assert!(parse("trust").unwrap_err().contains("Usage:"));
        assert!(parse("trust demo").unwrap_err().contains("Usage:"));
    }

    #[test]
    fn parse_tier_accepts_aliases() {
        assert_eq!(parse_tier("read-only").unwrap(), TrustTier::ReadOnly);
        assert_eq!(parse_tier("readonly").unwrap(), TrustTier::ReadOnly);
        assert_eq!(parse_tier("read_only").unwrap(), TrustTier::ReadOnly);
        assert_eq!(parse_tier("shell").unwrap(), TrustTier::Shell);
        assert_eq!(parse_tier("network").unwrap(), TrustTier::Network);
        assert_eq!(parse_tier("unsafe").unwrap(), TrustTier::Unsafe);
    }

    #[test]
    fn parse_tier_rejects_unknown() {
        assert!(parse_tier("superuser")
            .unwrap_err()
            .contains("unknown trust tier"));
    }

    #[test]
    fn plugin_status_summary_empty_returns_none() {
        let registry = PluginRegistry::new();
        assert!(plugin_status_summary(&registry, &[]).is_none());
    }

    #[test]
    fn active_plugin_names_collects_all_active() {
        let registry = PluginRegistry::new();
        let names = active_plugin_names(&registry);
        assert!(names.is_empty());
    }

    #[tokio::test]
    async fn list_plugins_shows_empty_directories() {
        let temp = tempfile::tempdir().unwrap();
        let _env = TempDataDir::new(temp.path()).await;
        let mut state = app_state();
        let tx = dummy_reload_tx();

        let msg = handle_plugins_command("list", &mut state, &tx).await;
        assert!(msg.contains("Active plugins: none"));
        assert!(msg.contains("Blocked plugins: none"));
        assert!(msg.contains("Available plugin directories: none"));
    }

    #[tokio::test]
    async fn disable_inactive_plugin_returns_error() {
        let temp = tempfile::tempdir().unwrap();
        let _env = TempDataDir::new(temp.path()).await;
        let mut state = app_state();
        let tx = dummy_reload_tx();

        let msg = handle_plugins_command("disable not-loaded", &mut state, &tx).await;
        assert!(msg.contains("not active"));
    }

    #[tokio::test]
    async fn enable_then_disable_plugin_updates_registry_and_skills() {
        let temp = tempfile::tempdir().unwrap();
        let plugins_dir = temp.path().join("plugins");
        let plugin_dir = plugins_dir.join("demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "demo"
version = "0.1.0"
description = "Demo plugin"
trust = "read-only"

[[capabilities]]
type = "skill"
trigger = "/demo"
prompt = "Demo skill"
"#,
        )
        .unwrap();

        let _env = TempDataDir::new(temp.path()).await;
        let mut state = app_state();
        let tx = dummy_reload_tx();

        let enable_msg = handle_plugins_command("enable demo", &mut state, &tx).await;
        assert!(
            enable_msg.contains("Enabled plugin 'demo'"),
            "unexpected enable message: {enable_msg}"
        );
        assert!(state
            .provider
            .plugin_registry
            .find_active_by_name("demo")
            .is_some());
        assert!(state
            .services
            .skill_registry
            .get_by_trigger("/demo")
            .is_some());
        assert_eq!(state.provider.plugin_status, Some("🔒1".to_string()));

        let disable_msg = handle_plugins_command("disable demo", &mut state, &tx).await;
        assert!(
            disable_msg.contains("Disabled plugin 'demo'"),
            "unexpected disable message: {disable_msg}"
        );
        assert!(state
            .provider
            .plugin_registry
            .find_active_by_name("demo")
            .is_none());
        assert!(state
            .services
            .skill_registry
            .get_by_trigger("/demo")
            .is_none());
    }

    #[tokio::test]
    async fn trust_reloads_plugin_with_specific_tier() {
        let temp = tempfile::tempdir().unwrap();
        let plugins_dir = temp.path().join("plugins");
        let plugin_dir = plugins_dir.join("demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "demo"
version = "0.1.0"
description = "Demo plugin"
trust = "shell"

[[capabilities]]
type = "skill"
trigger = "/demo"
prompt = "Demo skill"
"#,
        )
        .unwrap();

        let _env = TempDataDir::new(temp.path()).await;
        let mut state = app_state();
        // Clamp the host maximum to ReadOnly so the shell plugin is rejected
        // by default, then verify that `/plugins trust` overrides it for the
        // current session.
        {
            let mut cfg = state
                .services
                .config
                .write()
                .unwrap_or_else(|e| e.into_inner());
            cfg.tools.max_plugin_trust = TrustTier::ReadOnly;
        }
        let tx = dummy_reload_tx();

        let enable_msg = handle_plugins_command("enable demo", &mut state, &tx).await;
        assert!(
            enable_msg.contains("trust tier 'shell' exceeds host maximum 'read-only'"),
            "expected enable to be blocked: {enable_msg}"
        );

        let msg = handle_plugins_command("trust demo shell", &mut state, &tx).await;
        assert!(
            msg.contains("Set trust tier for plugin 'demo' to shell"),
            "{msg}"
        );
        let hosted = state
            .provider
            .plugin_registry
            .active_plugins()
            .into_iter()
            .find(|p| p.plugin.manifest.name == "demo")
            .unwrap();
        assert_eq!(hosted.plugin.manifest.trust, TrustTier::Shell);
        assert_eq!(hosted.effective_trust, TrustTier::Shell);
    }

    #[test]
    fn parse_workspace_source_commands() {
        assert_eq!(
            parse("toggle foo").unwrap(),
            PluginsOp::Toggle {
                name: "foo".to_string()
            }
        );
        assert_eq!(parse("setup").unwrap(), PluginsOp::Setup);
        assert_eq!(parse("sources").unwrap(), PluginsOp::Sources);
        assert_eq!(
            parse("add foo /path/to/foo").unwrap(),
            PluginsOp::Add {
                name: "foo".to_string(),
                path: "/path/to/foo".to_string()
            }
        );
        assert_eq!(
            parse("remove foo").unwrap(),
            PluginsOp::Remove {
                name: "foo".to_string()
            }
        );
    }

    #[test]
    fn parse_rejects_missing_workspace_arguments() {
        assert!(parse("toggle").unwrap_err().contains("Usage:"));
        assert!(parse("add").unwrap_err().contains("Usage:"));
        assert!(parse("add foo").unwrap_err().contains("Usage:"));
        assert!(parse("remove").unwrap_err().contains("Usage:"));
    }

    #[test]
    fn resolve_source_path_keeps_absolute_paths() {
        let abs = std::env::temp_dir().join("demo");
        let p = resolve_source_path(&abs.to_string_lossy());
        assert_eq!(p, abs);
    }

    #[test]
    fn resolve_source_path_joins_relative_to_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let p = resolve_source_path("./demo");
        assert_eq!(p, cwd.join("demo"));
    }

    #[tokio::test]
    async fn list_shows_effective_trust_and_filtered_count_on_downgrade() {
        let temp = tempfile::tempdir().unwrap();
        let plugins_dir = temp.path().join("plugins");
        let plugin_dir = plugins_dir.join("downgraded");
        let tools_dir = plugin_dir.join("tools");
        let hooks_dir = plugin_dir.join("hooks");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "downgraded"
version = "0.1.0"
description = "downgrade test"
trust = "shell"

[[capabilities]]
type = "skill"
trigger = "/demo"
prompt = "hi"

[[capabilities]]
type = "tool"
name = "downgraded/tool"
description = "shell tool"
command = "tools/tool.sh"

[[capabilities]]
type = "hook"
event = "post-turn"
command = "hooks/post-turn.sh"
"#,
        )
        .unwrap();
        std::fs::write(tools_dir.join("tool.sh"), "#!/bin/sh\nprintf ok").unwrap();
        std::fs::write(hooks_dir.join("post-turn.sh"), "#!/bin/sh\nexit 0").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in [tools_dir.join("tool.sh"), hooks_dir.join("post-turn.sh")] {
                let mut perms = std::fs::metadata(&f).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&f, perms).unwrap();
            }
        }

        let _env = TempDataDir::new(temp.path()).await;
        let mut state = app_state();
        // Clamp host max to ReadOnly + reject_on_excess=false so the shell
        // plugin is downgraded (not rejected) and its tool+hook filtered.
        {
            let mut cfg = state
                .services
                .config
                .write()
                .unwrap_or_else(|e| e.into_inner());
            cfg.tools.max_plugin_trust = TrustTier::ReadOnly;
            cfg.tools.reject_on_excess_plugin_trust = false;
        }
        let tx = dummy_reload_tx();
        let msg = handle_plugins_command("enable downgraded", &mut state, &tx).await;
        assert!(msg.contains("Enabled plugin 'downgraded'"), "{msg}");

        let list_msg = handle_plugins_command("list", &mut state, &tx).await;
        assert!(
            list_msg.contains("shell (effective: read-only)"),
            "expected effective trust annotation, got: {list_msg}"
        );
        assert!(
            list_msg.contains("filtered: 2 capabilities hidden by trust tier"),
            "expected filtered count of 2, got: {list_msg}"
        );
    }
}
