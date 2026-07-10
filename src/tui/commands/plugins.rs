//! `/plugins` slash-command family — runtime plugin mount/unmount.
//!
//! Subcommands:
//! - `list` — show active, blocked, and available plugin directories.
//! - `enable <name>` — load a plugin directory from `data_dir/plugins/<name>`.
//! - `disable <name>` — unload a named plugin and remove its skills.
//! - `reload` — full rescan of the plugins directory.
//! - `trust <name> <tier>` — session-only re-enable with a specific trust tier.

use crate::shared::{read_shared_config, SharedConfig};
use crate::tui::app::AppState;
use kirkforge_plugin::TrustTier;
use kirkforge_plugin_host::{PluginRegistry, TrustPolicy};
use std::path::PathBuf;
use tokio::sync::mpsc;

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
    let active_names = active_plugin_names(&state.plugin_registry);
    let warnings = blocked_warnings(state);

    let mut lines = Vec::new();

    let active = state.plugin_registry.active_plugins();
    if active.is_empty() {
        lines.push("Active plugins: none".to_string());
    } else {
        lines.push(format!("Active plugins ({}):", active.len()));
        for hosted in active {
            let name = &hosted.plugin.manifest.name;
            let trust = hosted.effective_trust;
            lines.push(format!("  - {name} ({trust})"));
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

    let cfg = read_shared_config(&state.config);
    if cfg.plugin_sources.is_empty() {
        lines.push("Workspace plugin sources: none (use /plugins add <name> <path>)".to_string());
    } else {
        lines.push(format!(
            "Workspace plugin sources ({}):",
            cfg.plugin_sources.len()
        ));
        let enabled: std::collections::HashSet<&String> = cfg.enabled_plugins.iter().collect();
        for (name, path) in &cfg.plugin_sources {
            let status = if enabled.contains(name) {
                if active_names.contains(name) {
                    "on ✓"
                } else {
                    "on (not loaded)"
                }
            } else {
                "off"
            };
            lines.push(format!("  - {name} -> {} [{status}]", path.display()));
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
    let cfg = read_shared_config(&state.config).clone();
    let dir = plugin_dir(name);
    let policy = TrustPolicy::up_to(cfg.max_plugin_trust);

    let loaded_name = match state.plugin_registry.load_one(&dir, policy) {
        Ok(n) => n,
        Err(e) => return format!("❌ Failed to enable plugin '{name}': {e}"),
    };

    // Replace any stale skills from a previous load of the same plugin.
    state.skill_registry.remove_plugin(&loaded_name);

    let skills_added =
        if let Some((manifest, plugin)) = state.plugin_registry.find_active_by_name(&loaded_name) {
            state.skill_registry.add_plugin(manifest, plugin)
        } else {
            0
        };

    state.plugin_status = plugin_status_summary(&state.plugin_registry, &blocked_warnings(state));
    crate::send_or_warn!(
        plugin_reload_tx.send(state.plugin_registry.clone()),
        "plugin registry receiver dropped; executor may have exited"
    );

    let hosted = state
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
    if state.plugin_registry.find_active_by_name(name).is_none() {
        return format!("❌ Plugin '{name}' is not active.");
    }

    state.skill_registry.remove_plugin(name);
    state.plugin_registry.remove(name);

    state.plugin_status = plugin_status_summary(&state.plugin_registry, &blocked_warnings(state));
    crate::send_or_warn!(
        plugin_reload_tx.send(state.plugin_registry.clone()),
        "plugin registry receiver dropped; executor may have exited"
    );

    format!("🔌 Disabled plugin '{name}'.")
}

/// `reload` — full rescan of the plugins directory.
async fn reload_plugins(
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    let cfg = read_shared_config(&state.config).clone();
    let before = state.plugin_registry.active_count();

    let (registry, warnings) = match crate::session::plugin_tools::load_plugin_registry(&cfg) {
        Ok(r) => r,
        Err(e) => return format!("❌ Plugin reload failed: {e}"),
    };

    state.plugin_registry = registry;

    // Rebuild the skill registry from scratch so it matches the fresh registry.
    state.skill_registry.clear();
    state
        .skill_registry
        .set_max_plugin_trust(cfg.max_plugin_trust);
    if let Err(e) = state.skill_registry.scan_and_load(&cfg) {
        tracing::warn!(error = %e, "skill rescan during /plugins reload failed");
    }
    for skill in crate::session::skills::builtin_skills() {
        state.skill_registry.register(skill);
    }

    let after = state.plugin_registry.active_count();
    state.plugin_status = state.skill_registry.plugin_status_summary();

    crate::send_or_warn!(
        plugin_reload_tx.send(state.plugin_registry.clone()),
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
    state.skill_registry.remove_plugin(name);
    state.plugin_registry.remove(name);

    let dir = plugin_dir(name);
    let policy = TrustPolicy::up_to(tier);

    let loaded_name = match state.plugin_registry.load_one(&dir, policy) {
        Ok(n) => n,
        Err(e) => return format!("❌ Failed to set trust tier for '{name}': {e}"),
    };

    let skills_added =
        if let Some((manifest, plugin)) = state.plugin_registry.find_active_by_name(&loaded_name) {
            state.skill_registry.add_plugin(manifest, plugin)
        } else {
            0
        };

    state.plugin_status = plugin_status_summary(&state.plugin_registry, &blocked_warnings(state));
    crate::send_or_warn!(
        plugin_reload_tx.send(state.plugin_registry.clone()),
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
    let active = active_plugin_names(&state.plugin_registry);
    state
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

/// `toggle <name>` — persistently enable/disable a workspace plugin source.
async fn toggle_plugin(
    name: &str,
    state: &mut AppState,
    plugin_reload_tx: &mpsc::UnboundedSender<PluginRegistry>,
) -> String {
    {
        let mut cfg = write_shared_config(&state.config);
        if !cfg.plugin_sources.contains_key(name) {
            return format!("❌ Unknown workspace plugin source '{name}'. Use /plugins sources to see configured sources, or /plugins add {name} <path>.");
        }
        let was_enabled = cfg.enabled_plugins.iter().any(|n| n == name);
        if was_enabled {
            cfg.enabled_plugins.retain(|n| n != name);
        } else {
            cfg.enabled_plugins.push(name.to_string());
        }
        if let Err(e) = crate::session::config::save_config(&cfg) {
            return format!("❌ Failed to save config while toggling '{name}': {e}");
        }
    }

    reload_plugins(state, plugin_reload_tx).await
}

/// `setup` — show a quick-start message for workspace plugin sources.
async fn setup_plugin_sources(
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
fn list_sources(state: &AppState) -> String {
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
async fn add_source(
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
fn remove_source(
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
fn resolve_source_path(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(p)
    } else {
        p
    }
}

/// Mutable access to shared config, recovering from lock poisoning.
fn write_shared_config(
    cfg: &SharedConfig,
) -> std::sync::RwLockWriteGuard<'_, crate::shared::Config> {
    cfg.write().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;
    use std::sync::{Arc, OnceLock};
    use tokio::sync::Mutex as TokioMutex;

    /// Serialize tests that mutate the process-wide `KIRKFORGE_DATA_DIR` env var.
    static ENV_LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();

    fn env_lock() -> &'static TokioMutex<()> {
        ENV_LOCK.get_or_init(|| TokioMutex::new(()))
    }

    /// Sets `KIRKFORGE_DATA_DIR` to `dir` for the lifetime of the guard.
    struct TempDataDir {
        prev: Option<std::ffi::OsString>,
        _guard: tokio::sync::MutexGuard<'static, ()>,
    }

    impl TempDataDir {
        async fn new(dir: &std::path::Path) -> Self {
            let guard = env_lock().lock().await;
            let prev = std::env::var_os("KIRKFORGE_DATA_DIR");
            std::env::set_var("KIRKFORGE_DATA_DIR", dir.as_os_str());
            Self {
                prev,
                _guard: guard,
            }
        }
    }

    impl Drop for TempDataDir {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("KIRKFORGE_DATA_DIR", v),
                None => std::env::remove_var("KIRKFORGE_DATA_DIR"),
            }
        }
    }

    fn test_state() -> AppState {
        AppState::new(Arc::new(std::sync::RwLock::new(Config::default())))
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
        let mut state = test_state();
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
        let mut state = test_state();
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
            plugin_dir.join("kirkforge.toml"),
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
        let mut state = test_state();
        let tx = dummy_reload_tx();

        let enable_msg = handle_plugins_command("enable demo", &mut state, &tx).await;
        assert!(
            enable_msg.contains("Enabled plugin 'demo'"),
            "unexpected enable message: {enable_msg}"
        );
        assert!(state.plugin_registry.find_active_by_name("demo").is_some());
        assert!(state.skill_registry.get_by_trigger("/demo").is_some());
        assert_eq!(state.plugin_status, Some("🔒1".to_string()));

        let disable_msg = handle_plugins_command("disable demo", &mut state, &tx).await;
        assert!(
            disable_msg.contains("Disabled plugin 'demo'"),
            "unexpected disable message: {disable_msg}"
        );
        assert!(state.plugin_registry.find_active_by_name("demo").is_none());
        assert!(state.skill_registry.get_by_trigger("/demo").is_none());
    }

    #[tokio::test]
    async fn trust_reloads_plugin_with_specific_tier() {
        let temp = tempfile::tempdir().unwrap();
        let plugins_dir = temp.path().join("plugins");
        let plugin_dir = plugins_dir.join("demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kirkforge.toml"),
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
        let mut state = test_state();
        // Clamp the host maximum to ReadOnly so the shell plugin is rejected
        // by default, then verify that `/plugins trust` overrides it for the
        // current session.
        {
            let mut cfg = state.config.write().unwrap();
            cfg.max_plugin_trust = TrustTier::ReadOnly;
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
        let p = resolve_source_path("/tmp/demo");
        assert_eq!(p, PathBuf::from("/tmp/demo"));
    }

    #[test]
    fn resolve_source_path_joins_relative_to_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let p = resolve_source_path("./demo");
        assert_eq!(p, cwd.join("demo"));
    }
}
