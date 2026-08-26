//! Shared plugin-ops layer — pure functions used by both the TUI
//! `/plugins` slash-command family and the `kf-code plugin` CLI
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
use kf_plugin_sdk::{Plugin, PluginManifest};
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
/// Returns a status message. The next `kf-code run` (or a TUI reload)
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
                "❌ Unknown plugin '{name}'. Use `kf-code plugin add {name} <path>` \
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
        "Enabled plugin '{name}'. Run `kf-code run` (or `/plugins reload` in the TUI) \
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
        "Disabled plugin '{name}'. Run `kf-code run` (or `/plugins reload` in the TUI) \
         to apply."
    ))
}

/// `toggle <name>` — flip the enabled state and persist.
pub fn toggle(cfg: &mut Config, name: &str) -> anyhow::Result<String> {
    if !cfg.tools.plugin_sources.contains_key(name) {
        return Ok(format!(
            "❌ Unknown workspace plugin source '{name}'. \
             Use `kf-code plugin add {name} <path>` to register one."
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

/// `validate <path>` — load a manifest from a `kf-code.toml` and report
/// every validation error. Pure read; no config mutation.
pub fn validate(path: &Path) -> anyhow::Result<String> {
    let manifest_path = if path.is_dir() {
        path.join("kf-code.toml")
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
        return "No workspace plugin sources configured. Use `kf-code plugin add <name> <path>`."
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
         run `kf-code run` or `/plugins reload` to load it.",
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
            if let kf_plugin_sdk::Capability::Tool {
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
            if let kf_plugin_sdk::Capability::Hook { event, command } = cap {
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

/// Scaffold a new plugin directory with a valid `kf-code.toml`
/// (WO 11.8, ADR-063). The scaffolded manifest uses `trust = "read-only"`
/// (safest default — a copy-pasted scaffold can't accidentally run
/// shell commands until the author bumps the trust tier) and a
/// placeholder skill prompt. Also creates `tools/`, `hooks/`, and a
/// `README.md`.
pub fn init(name: &str, path: Option<&Path>) -> anyhow::Result<PathBuf> {
    // Validate the plugin name (same kebab-case rule as the manifest).
    if name.is_empty() || name.starts_with('-') || name.ends_with('-') {
        anyhow::bail!("plugin name must not be empty or start/end with a hyphen");
    }
    let mut prev_hyphen = false;
    for ch in name.chars() {
        if ch == '-' {
            if prev_hyphen {
                anyhow::bail!("plugin name must not contain consecutive hyphens");
            }
            prev_hyphen = true;
        } else if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            prev_hyphen = false;
        } else {
            anyhow::bail!(
                "plugin name must be lowercase alphanumeric segments joined by single hyphens (got '{name}')"
            );
        }
    }

    let base = path.map(|p| p.to_path_buf()).unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("plugins")
    });
    let plugin_dir = base.join(name);
    if plugin_dir.exists() {
        anyhow::bail!("plugin directory already exists: {}", plugin_dir.display());
    }

    std::fs::create_dir_all(plugin_dir.join("tools"))?;
    std::fs::create_dir_all(plugin_dir.join("hooks"))?;
    // .gitkeep so the empty dirs survive a commit.
    std::fs::write(plugin_dir.join("tools/.gitkeep"), "")?;
    std::fs::write(plugin_dir.join("hooks/.gitkeep"), "")?;

    let manifest = format!(
        r#"# {name} plugin manifest.
# Scaffolded by `kf-code plugin init`. Edit this file, then run
# `kf-code plugin enable {name}` (or `/plugins enable {name}` in the
# TUI) to activate.

name = "{name}"
version = "0.1.0"
description = "{name} plugin"
trust = "read-only"

# ── Skill (slash command) ──────────────────────────────────────────────
# A read-only skill prompt. The user invokes /{name} and the model gets
# this prompt with {{args}} replaced by the user's input.

[[capabilities]]
type = "skill"
trigger = "/{name}"
prompt = """
You are the {name} plugin. The user invoked /{name}.

TODO: implement this skill. Replace this prompt with the skill's
instructions for the model.

User request: {{{{args}}}}
"""
model-hint = "default"

# ── Optional: tool, hook, verifier ─────────────────────────────────────
# To add a tool (shell script), bump `trust` to "shell", add a tool
# script under tools/, and declare a tool capability:
#
# [[capabilities]]
# type = "tool"
# name = "{name}/my_tool"
# description = "does a thing"
# command = "tools/my_tool.sh"
#
# To add a hook (lifecycle script), bump `trust` to "shell", add a
# script under hooks/, and declare a hook capability:
#
# [[capabilities]]
# type = "hook"
# event = "post-turn"
# command = "hooks/post-turn.sh"
#
# To add a verifier, declare a verifier capability with a check script:
#
# [[capabilities]]
# type = "verifier"
# name = "{name}-check"
# priority = 1
# command = "verifiers/check.sh"
"#
    );
    std::fs::write(plugin_dir.join("kf-code.toml"), manifest)?;

    let readme = format!(
        r#"# {name}

A KirkForge plugin. See `kf-code.toml` for the manifest schema.

## Getting started

1. Edit `kf-code.toml` — replace the placeholder prompt, add tools/hooks/verifiers.
2. Run `kf-code plugin validate {path}` to check the manifest.
3. Run `kf-code plugin enable {name}` (or `/plugins enable {name}` in the TUI).
4. Run `kf-code run` to start a session with the plugin active.

## Signing (optional)

If you want signature verification, generate a minisign keypair and sign
the manifest:
```
minisign -S -m kf-code.toml
```
Then configure `plugin_signature_validation = true` and
`plugin_public_key_path = <path-to-pubkey>` in your kf-code config.
"#,
        path = plugin_dir.display()
    );
    std::fs::write(plugin_dir.join("README.md"), readme)?;

    Ok(plugin_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::EnvGuard;
    use crate::shared::Config;

    struct DataDirGuard {
        _env: EnvGuard,
        _lock: tokio::sync::MutexGuard<'static, ()>,
    }

    impl DataDirGuard {
        fn new(dir: &std::path::Path) -> Self {
            let lock = crate::session::test_data_dir_lock().blocking_lock();
            let env = EnvGuard::set("KF_CODE_DATA_DIR", dir.as_os_str());
            Self {
                _env: env,
                _lock: lock,
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
            plugin_dir.join("kf-code.toml"),
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
            dir.join("kf-code.toml"),
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
            dir.join("kf-code.toml"),
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
            plugin_dir.join("kf-code.toml"),
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
        // WO 27.4: the demo plugin is unsigned; the trust gate now rejects
        // unsigned workspace plugins by default. Opt in so this test still
        // exercises the doctor's "missing tool command" path.
        cfg.tools.plugin_trust_workspace = true;
        // WO 46.13: ledger defaults on; this test exercises the doctor's
        // missing-tool-command path, not the consent gate, so opt out.
        cfg.tools.plugin_consent_ledger = false;
        let out = doctor(&cfg);
        assert!(out.contains("Load warnings"), "{out}");
        assert!(out.contains("not accessible"), "{out}");
    }

    /// WO 11.8: `init` scaffolds a valid plugin, and `validate` passes
    /// on the scaffolded manifest (round-trip).
    #[test]
    fn init_scaffolds_valid_plugin_and_validate_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("plugins");
        let plugin_dir = init("my-plugin", Some(&parent)).unwrap();
        assert!(plugin_dir.join("kf-code.toml").is_file());
        assert!(plugin_dir.join("tools").is_dir());
        assert!(plugin_dir.join("hooks").is_dir());
        assert!(plugin_dir.join("README.md").is_file());

        // The scaffolded manifest must be valid.
        let out = validate(&plugin_dir).unwrap();
        assert!(out.contains("manifest valid"), "{out}");
        assert!(out.contains("my-plugin"), "{out}");
    }

    #[test]
    fn init_rejects_invalid_name() {
        let tmp = tempfile::tempdir().unwrap();
        let err = init("BadName", Some(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("lowercase"), "{err}");
        let err = init("", Some(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn init_rejects_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("plugins");
        std::fs::create_dir_all(parent.join("demo")).unwrap();
        let err = init("demo", Some(&parent)).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn resolve_source_path_keeps_absolute_path() {
        let abs = if cfg!(windows) {
            "C:\\plugins\\demo"
        } else {
            "/plugins/demo"
        };
        let resolved = resolve_source_path(abs);
        assert_eq!(resolved, PathBuf::from(abs));
    }

    #[test]
    fn resolve_source_path_joins_relative_to_cwd() {
        let resolved = resolve_source_path("relative/path");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(resolved, cwd.join("relative/path"));
    }

    #[test]
    fn sources_non_empty_lists_each_source_with_status() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let mut cfg = empty_config();
        let dir = tmp.path().join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        cfg.tools
            .plugin_sources
            .insert("demo".to_string(), dir.clone());
        // Not enabled — should show as off.
        let out = sources(&cfg);
        assert!(out.contains("Workspace plugin sources (1)"), "{out}");
        assert!(out.contains("demo"), "{out}");
        assert!(out.contains("[off]"), "{out}");

        // Enable it — should show as on.
        cfg.tools.enabled_plugins.push("demo".to_string());
        let out2 = sources(&cfg);
        assert!(out2.contains("[on]"), "{out2}");
    }

    #[test]
    fn add_source_missing_dir_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let mut cfg = empty_config();
        // A path that exists but is a file, not a directory.
        let file_path = tmp.path().join("not-a-dir");
        std::fs::write(&file_path, "x").unwrap();
        let err = add_source(&mut cfg, "x", file_path.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[test]
    fn add_source_adds_to_enabled_list() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::new(tmp.path());
        let plugin_dir = tmp.path().join("demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let mut cfg = empty_config();
        let msg = add_source(&mut cfg, "demo", plugin_dir.to_str().unwrap()).unwrap();
        assert!(msg.contains("Added plugin source 'demo'"), "{msg}");
        assert!(cfg.tools.enabled_plugins.iter().any(|n| n == "demo"));
        // Clean up the persisted config so the test doesn't leak state.
        let _ = std::fs::remove_file(
            std::env::var("KF_CODE_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("config.toml"),
        );
    }

    #[test]
    fn init_rejects_consecutive_hyphens() {
        let tmp = tempfile::tempdir().unwrap();
        let err = init("foo--bar", Some(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("consecutive hyphens"), "{err}");
    }

    #[test]
    fn init_rejects_leading_or_trailing_hyphen() {
        let tmp = tempfile::tempdir().unwrap();
        let err = init("-leading", Some(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("hyphen"), "{err}");
        let err = init("trailing-", Some(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("hyphen"), "{err}");
    }

    #[test]
    fn init_rejects_uppercase_and_non_ascii_alphanumeric() {
        let tmp = tempfile::tempdir().unwrap();
        let err = init("UpperCase", Some(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("lowercase"), "{err}");
        let err = init("with_underscore", Some(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("lowercase"), "{err}");
    }

    #[test]
    fn init_accepts_valid_kebab_name() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path();
        let dir = init("valid-name-1", Some(parent)).unwrap();
        assert!(dir.ends_with("valid-name-1"), "{dir:?}");
        std::fs::remove_dir_all(&dir).ok();
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
