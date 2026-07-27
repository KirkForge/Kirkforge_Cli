//! Shared plugin-ops layer — pure functions used by both the TUI
//! `/plugins` slash-command family and the `kirkforge plugin` CLI
//! subcommand.
//!
//! The functions take a `&Config` (or `&mut Config`) and return a
//! human-readable `String` (queries) or `Result<String>` (mutations).
//! Neither `AppState` nor the live `PluginRegistry` is touched here —
//! the TUI keeps its `mpsc` reload plumbing; the CLI mutates the config
//! and prints "restart to apply" when there is no live registry.
//!
//! ADR-056 pins the shared-layer decision (WO 11.0).

use crate::shared::Config;
use kirkforge_plugin::{Plugin, PluginManifest};
use std::path::{Path, PathBuf};

/// `list` — format the active/blocked/available plugin summary from a
/// freshly loaded registry snapshot. Pure: no `AppState`, no channels.
///
/// This mirrors the TUI `list_plugins` output, but takes a pre-loaded
/// `PluginRegistry` so the CLI path (which loads once and prints) and
/// the TUI path (which loads into `AppState` and renders) share one
/// formatter.
pub fn list(cfg: &Config) -> String {
    let (registry, warnings) = match crate::session::plugin_tools::load_plugin_registry(cfg) {
        Ok(rw) => rw,
        Err(e) => return format!("❌ Failed to load plugin registry: {e:#}"),
    };

    let mut lines = Vec::new();

    let active = registry.active_plugins();
    let active_names: std::collections::HashSet<String> = active
        .iter()
        .map(|h| h.plugin.manifest.name.clone())
        .collect();
    if active.is_empty() {
        lines.push("Active plugins: none".to_string());
    } else {
        lines.push(format!("Active plugins ({}):", active.len()));
        for hosted in &active {
            let name = &hosted.plugin.manifest.name;
            let trust = hosted.effective_trust;
            lines.push(format!("  - {name} ({trust})"));
        }
    }

    if warnings.is_empty() {
        lines.push("Blocked plugins: none".to_string());
    } else {
        lines.push(format!("Blocked plugins ({}):", warnings.len()));
        for w in &warnings {
            lines.push(format!("  - {w}"));
        }
    }

    // Available dirs not currently loaded.
    let base = crate::session::plugin_tools::plugins_dir();
    let mut available: Vec<String> = Vec::new();
    if base.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.') || active_names.contains(name) {
                        continue;
                    }
                    available.push(name.to_string());
                }
            }
        }
    }
    available.sort();
    if available.is_empty() {
        lines.push("Available plugin directories: none".to_string());
    } else {
        lines.push(format!(
            "Available plugin directories ({}):",
            available.len()
        ));
        for d in &available {
            lines.push(format!("  - {d}"));
        }
    }

    // Workspace sources.
    if cfg.tools.plugin_sources.is_empty() {
        lines.push("Workspace plugin sources: none".to_string());
    } else {
        lines.push(format!(
            "Workspace plugin sources ({}):",
            cfg.tools.plugin_sources.len()
        ));
        let enabled: std::collections::HashSet<&str> = cfg
            .tools
            .enabled_plugins
            .iter()
            .map(|s| s.as_str())
            .collect();
        for (name, path) in &cfg.tools.plugin_sources {
            let is_compiled = crate::session::plugin_tools::folded_feature_enabled(name);
            let is_folded = crate::session::plugin_tools::is_folded(name);
            let feature = crate::session::plugin_tools::folded_feature(name);
            let source_label = if is_compiled {
                "compiled-in"
            } else if is_folded {
                "external (feature off)"
            } else {
                "external"
            };
            let on = enabled.contains(name.as_str());
            let status = if on {
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
            let feat = match feature {
                Some(f) if is_compiled => format!(" [{f}: on]"),
                Some(f) => format!(" [{f}: off]"),
                None => String::new(),
            };
            lines.push(format!(
                "  - {name} -> {} [{status}] ({source_label}){feat}",
                path.display()
            ));
        }
    }

    lines.join("\n")
}

/// `enable <name>` — add `name` to `enabled_plugins` and persist.
///
/// Returns a status message. The next `kirkforge run` (or a TUI reload)
/// picks up the change. The CLI path does not have a live registry to
/// reload — that is documented in the returned message.
pub fn enable(cfg: &mut Config, name: &str) -> anyhow::Result<String> {
    if !cfg.tools.plugin_sources.contains_key(name)
        && !crate::session::plugin_tools::is_folded(name)
    {
        // For an unknown source, check the data-dir plugins/<name>.
        let dir = crate::session::plugin_tools::plugins_dir().join(name);
        if !dir.is_dir() {
            return Ok(format!(
                "❌ Unknown plugin '{name}'. Use `kirkforge plugin add {name} <path>` \
                 to register a source, or place it under {dir}",
                dir = dir.display()
            ));
        }
    }
    if cfg.tools.enabled_plugins.iter().any(|n| n == name) {
        return Ok(format!("Plugin '{name}' is already enabled."));
    }
    cfg.tools.enabled_plugins.push(name.to_string());
    crate::session::config::save_config(cfg)?;
    Ok(format!(
        "Enabled plugin '{name}'. Run `kirkforge run` (or `/plugins reload` in the TUI) \
         to load it."
    ))
}

/// `disable <name>` — remove `name` from `enabled_plugins` and persist.
pub fn disable(cfg: &mut Config, name: &str) -> anyhow::Result<String> {
    let before = cfg.tools.enabled_plugins.len();
    cfg.tools.enabled_plugins.retain(|n| n != name);
    if cfg.tools.enabled_plugins.len() == before {
        return Ok(format!("Plugin '{name}' was not enabled."));
    }
    crate::session::config::save_config(cfg)?;
    Ok(format!(
        "Disabled plugin '{name}'. Run `kirkforge run` (or `/plugins reload` in the TUI) \
         to apply."
    ))
}

/// `toggle <name>` — flip the enabled state and persist.
pub fn toggle(cfg: &mut Config, name: &str) -> anyhow::Result<String> {
    if !cfg.tools.plugin_sources.contains_key(name) {
        return Ok(format!(
            "❌ Unknown workspace plugin source '{name}'. \
             Use `kirkforge plugin add {name} <path>` to register one."
        ));
    }
    let was_on = cfg.tools.enabled_plugins.iter().any(|n| n == name);
    if was_on {
        cfg.tools.enabled_plugins.retain(|n| n != name);
    } else {
        cfg.tools.enabled_plugins.push(name.to_string());
    }
    crate::session::config::save_config(cfg)?;
    let now = if was_on { "off" } else { "on" };
    Ok(format!("Toggled plugin '{name}' to {now}."))
}

/// `validate <path>` — load a manifest from a `kirkforge.toml` and report
/// every validation error. Pure read; no config mutation.
pub fn validate(path: &Path) -> anyhow::Result<String> {
    let manifest_path = if path.is_dir() {
        path.join("kirkforge.toml")
    } else {
        path.to_path_buf()
    };
    if !manifest_path.is_file() {
        anyhow::bail!("manifest not found: {}", manifest_path.display());
    }
    let manifest = PluginManifest::from_file(&manifest_path)?;
    manifest.validate_api_version()?;
    match manifest.validate() {
        Ok(()) => Ok(format!(
            "✓ manifest valid: {} v{} ({})",
            manifest.name, manifest.version, manifest.trust
        )),
        Err(errs) => {
            let mut lines = vec![format!("✗ manifest invalid: {}", manifest_path.display())];
            for e in &errs {
                lines.push(format!("  - {e}"));
            }
            Ok(lines.join("\n"))
        }
    }
}

/// `sources` — list configured workspace plugin sources.
pub fn sources(cfg: &Config) -> String {
    if cfg.tools.plugin_sources.is_empty() {
        return "No workspace plugin sources configured. Use `kirkforge plugin add <name> <path>`."
            .to_string();
    }
    let mut lines = vec![format!(
        "Workspace plugin sources ({}):",
        cfg.tools.plugin_sources.len()
    )];
    let enabled: std::collections::HashSet<&str> = cfg
        .tools
        .enabled_plugins
        .iter()
        .map(|s| s.as_str())
        .collect();
    for (name, path) in &cfg.tools.plugin_sources {
        let on = enabled.contains(name.as_str());
        let status = if on { "on" } else { "off" };
        lines.push(format!("  - {name} -> {} [{status}]", path.display()));
    }
    lines.join("\n")
}

/// `add <name> <path>` — register a workspace plugin source and persist.
pub fn add_source(cfg: &mut Config, name: &str, path: &str) -> anyhow::Result<String> {
    let resolved = resolve_source_path(path);
    if !resolved.exists() {
        anyhow::bail!("plugin source path does not exist: {}", resolved.display());
    }
    if !resolved.is_dir() {
        anyhow::bail!(
            "plugin source path is not a directory: {}",
            resolved.display()
        );
    }
    cfg.tools.plugin_sources.insert(name.to_string(), resolved);
    if !cfg.tools.enabled_plugins.iter().any(|n| n == name) {
        cfg.tools.enabled_plugins.push(name.to_string());
    }
    crate::session::config::save_config(cfg)?;
    Ok(format!(
        "Added plugin source '{name}' -> {}. It is now enabled; \
         run `kirkforge run` or `/plugins reload` to load it.",
        cfg.tools.plugin_sources[name].display()
    ))
}

/// `remove <name>` — unregister a workspace plugin source and persist.
pub fn remove_source(cfg: &mut Config, name: &str) -> anyhow::Result<String> {
    if cfg.tools.plugin_sources.remove(name).is_none() {
        anyhow::bail!("no workspace plugin source named '{name}'");
    }
    cfg.tools.enabled_plugins.retain(|n| n != name);
    crate::session::config::save_config(cfg)?;
    Ok(format!("Removed workspace plugin source '{name}'."))
}

/// `doctor` — load the registry, then probe each enabled plugin's tool/hook
/// command files for existence. Returns a health report.
pub fn doctor(cfg: &Config) -> String {
    let (registry, warnings) = match crate::session::plugin_tools::load_plugin_registry(cfg) {
        Ok(rw) => rw,
        Err(e) => return format!("❌ Failed to load plugin registry: {e:#}"),
    };

    let mut lines = Vec::new();
    if !warnings.is_empty() {
        lines.push(format!("Load warnings ({}):", warnings.len()));
        for w in &warnings {
            lines.push(format!("  - {w}"));
        }
    } else {
        lines.push("Load warnings: none".to_string());
    }

    let active = registry.active_plugins();
    if active.is_empty() {
        lines.push("Active plugins: none".to_string());
        return lines.join("\n");
    }

    lines.push(format!("Plugin health ({} active):", active.len()));
    for hosted in &active {
        let name = &hosted.plugin.manifest.name;
        let root = hosted.plugin.root();
        let mut missing: Vec<String> = Vec::new();
        for cap in hosted.plugin.tools() {
            if let kirkforge_plugin::Capability::Tool {
                name: cap_name,
                command: Some(cmd),
                ..
            } = cap
            {
                if !root.join(&cmd).exists() {
                    missing.push(format!("tool '{cap_name}' -> {}", cmd.display()));
                }
            }
        }
        for cap in hosted.plugin.hooks() {
            if let kirkforge_plugin::Capability::Hook { event, command } = cap {
                if !root.join(&command).exists() {
                    missing.push(format!("hook '{event}' -> {}", command.display()));
                }
            }
        }
        if missing.is_empty() {
            lines.push(format!("  - {name}: ok"));
        } else {
            lines.push(format!("  - {name}: missing {} command(s)", missing.len()));
            for m in &missing {
                lines.push(format!("      {m}"));
            }
        }
    }
    lines.join("\n")
}

/// Resolve a workspace source path relative to the current directory.
/// Kept in sync with `src/tui/commands/plugins/sources.rs::resolve_source_path`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;

    struct DataDirGuard {
        prev: Option<std::ffi::OsString>,
        _lock: tokio::sync::MutexGuard<'static, ()>,
    }

    impl DataDirGuard {
        fn new(dir: &std::path::Path) -> Self {
            let lock = crate::session::test_data_dir_lock().blocking_lock();
            let prev = std::env::var_os("KIRKFORGE_DATA_DIR");
            std::env::set_var("KIRKFORGE_DATA_DIR", dir.as_os_str());
            Self { prev, _lock: lock }
        }
    }

    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("KIRKFORGE_DATA_DIR", v),
                None => std::env::remove_var("KIRKFORGE_DATA_DIR"),
            }
        }
    }

    #[test]
    fn list_empty_reports_none() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let cfg = empty_config();
        let out = list(&cfg);
        assert!(out.contains("Active plugins: none"), "{out}");
        assert!(out.contains("Blocked plugins: none"), "{out}");
        assert!(out.contains("Available plugin directories: none"), "{out}");
    }

    #[test]
    fn enable_unknown_plugin_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let mut cfg = empty_config();
        let msg = enable(&mut cfg, "no-such-plugin").unwrap();
        assert!(msg.contains("Unknown plugin"), "{msg}");
        assert!(cfg.tools.enabled_plugins.is_empty());
    }

    #[test]
    fn enable_then_disable_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let plugin_dir = tmp.path().join("demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kirkforge.toml"),
            r#"
name = "demo"
version = "0.1.0"
description = "demo"
trust = "read-only"

[[capabilities]]
type = "skill"
trigger = "/demo"
prompt = "hi"
"#,
        )
        .unwrap();
        let mut cfg = empty_config();
        cfg.tools
            .plugin_sources
            .insert("demo".to_string(), plugin_dir);

        let on = enable(&mut cfg, "demo").unwrap();
        assert!(on.contains("Enabled plugin 'demo'"), "{on}");
        assert!(cfg.tools.enabled_plugins.iter().any(|n| n == "demo"));

        let off = disable(&mut cfg, "demo").unwrap();
        assert!(off.contains("Disabled plugin 'demo'"), "{off}");
        assert!(!cfg.tools.enabled_plugins.iter().any(|n| n == "demo"));
    }

    #[test]
    fn toggle_unknown_source_rejects() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let mut cfg = empty_config();
        let msg = toggle(&mut cfg, "missing").unwrap();
        assert!(msg.contains("Unknown workspace plugin source"), "{msg}");
    }

    #[test]
    fn validate_valid_manifest_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("kirkforge.toml"),
            r#"
name = "demo"
version = "0.1.0"
description = "demo"
trust = "read-only"
"#,
        )
        .unwrap();
        let out = validate(&dir).unwrap();
        assert!(out.contains("manifest valid"), "{out}");
    }

    #[test]
    fn validate_broken_manifest_reports_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("kirkforge.toml"),
            r#"
name = "Bad_Name"
version = "not-semver"
description = "bad"
trust = "read-only"
"#,
        )
        .unwrap();
        let out = validate(&dir).unwrap();
        assert!(out.contains("manifest invalid"), "{out}");
        assert!(out.contains("name"), "{out}");
        assert!(out.contains("version"), "{out}");
    }

    #[test]
    fn validate_missing_manifest_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("empty");
        std::fs::create_dir_all(&dir).unwrap();
        let err = validate(&dir).unwrap_err();
        assert!(err.to_string().contains("manifest not found"));
    }

    #[test]
    fn sources_empty_reports_none() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let cfg = empty_config();
        let out = sources(&cfg);
        assert!(out.contains("No workspace plugin sources"), "{out}");
    }

    #[test]
    fn add_source_missing_path_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let mut cfg = empty_config();
        let err = add_source(&mut cfg, "x", "/does/not/exist").unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn remove_source_missing_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let mut cfg = empty_config();
        let err = remove_source(&mut cfg, "ghost").unwrap_err();
        assert!(err.to_string().contains("no workspace plugin source"));
    }

    #[test]
    fn doctor_empty_registry_reports_none() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let cfg = empty_config();
        let out = doctor(&cfg);
        assert!(out.contains("Active plugins: none"), "{out}");
    }

    #[test]
    fn doctor_reports_missing_tool_command() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kirkforge.toml"),
            r#"
name = "demo"
version = "0.1.0"
description = "demo"
trust = "shell"

[[capabilities]]
type = "tool"
name = "demo/greet"
description = "greet"
command = "tools/greet.sh"
"#,
        )
        .unwrap();
        // No tools dir / script — the loader drops the capability and
        // emits a warning; doctor surfaces the warning.
        let mut cfg = empty_config();
        cfg.tools.enabled_plugins = vec!["demo".to_string()];
        cfg.tools
            .plugin_sources
            .insert("demo".to_string(), plugin_dir.clone());
        let out = doctor(&cfg);
        assert!(out.contains("Load warnings"), "{out}");
        assert!(out.contains("not accessible"), "{out}");
    }

    /// `Config::default()` includes the four in-repo default plugin_sources;
    /// tests that assert "empty" need a config with those cleared.
    fn empty_config() -> Config {
        let mut cfg = Config::default();
        cfg.tools.plugin_sources.clear();
        cfg.tools.enabled_plugins.clear();
        cfg
    }
}
