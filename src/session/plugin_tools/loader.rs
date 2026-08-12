//! Plugin loader functions: resolve the plugins directory, build the trust
//! policy from config, and load plugins into a `PluginRegistry`.
//!
//! This is the "plugin loader hub" that couples to config, access, the plugin
//! host crate, and the `PluginToolWrapper` defined in [`super::wrapper`].
//!
//! ## Two-path dispatch (ADR-050)
//!
//! Folded plugins (Stratum, Budget) can run as either:
//! - **Compiled-in** (feature on): tools register as direct Rust calls in
//!   `main/mod.rs`; the shell plugin dir is skipped here.
//! - **External** (feature off): the shell plugin dir loads here as
//!   `PluginToolWrapper` shell-outs (graceful degradation).
//!
//! The `enabled_plugins` config is the single toggle for both paths.

use crate::shared::{Config, SharedConfig};
use crate::tools::Tool;
use kf_plugin_host::{PluginRegistry, TrustPolicy};
use kf_plugin_sdk::{Capability, Plugin};
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
    ("kf-budget", "budget"),
    ("kf-plugin", "kf-plugin-tools"),
];

/// Check if a plugin name is folded and whether its feature is compiled in.
pub fn folded_feature_enabled(name: &str) -> bool {
    match name {
        #[cfg(feature = "stratum")]
        "stratum" => true,
        #[cfg(feature = "budget")]
        "kf-budget" => true,
        #[cfg(feature = "kf-plugin-tools")]
        "kf-plugin" => true,
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

/// Default plugins directory: `~/.local/share/kf-code/plugins/`.
pub fn plugins_dir() -> PathBuf {
    crate::session::data_dir()
        .map(|d| d.join("plugins"))
        .unwrap_or_else(|_| PathBuf::from(".local/share/kf-code/plugins"))
}

/// Build the host trust policy from the current config snapshot.
///
/// When signature validation is enabled, an info-level message is logged.
/// Workspace plugins loaded via [`load_workspace_plugins`] automatically get
/// `verify_signatures` set to `false` (local-trust bypass) since they are
/// local development paths, not registry-issued artifacts.
pub fn trust_policy_from_config(cfg: &Config) -> TrustPolicy {
    if cfg.tools.plugin_signature_validation {
        tracing::info!(
            "plugin signature verification is enabled; set plugin_signature_validation = false to disable"
        );
    }
    TrustPolicy {
        max: cfg.tools.max_plugin_trust,
        reject_on_excess: cfg.tools.reject_on_excess_plugin_trust,
        verify_signatures: cfg.tools.plugin_signature_validation,
        signature_key_path: cfg.tools.plugin_public_key_path.as_ref().map(PathBuf::from),
    }
}

/// Build a trust policy for local workspace plugins.
///
/// Workspace plugin dirs (`cfg.tools.plugin_sources`) are NOT trust roots by
/// default — a model with `write_file` access can drop a plugin + manifest
/// there and wait for `/reload` (H9). The operator must explicitly opt in via
/// `plugin_trust_workspace = true` to restore the local-trust bypass.
///
/// - `trust_workspace == false` (default): `verify_signatures` is forced on
///   regardless of the global `plugin_signature_validation` toggle, so
///   unsigned/invalid workspace plugins fail-closed at load. The bar is
///   higher than data-dir plugins because workspace dirs are model-writable.
/// - `trust_workspace == true`: signatures are bypassed as before, with a
///   `tracing::warn!` so the operator sees the trust decision in logs.
fn local_trust_policy(base: &TrustPolicy, trust_workspace: bool) -> TrustPolicy {
    if trust_workspace {
        tracing::warn!(
            "plugin_trust_workspace = true: workspace plugins bypass signature verification \
             (unsafe if a model can write to the workspace)"
        );
        TrustPolicy {
            verify_signatures: false,
            ..base.clone()
        }
    } else {
        tracing::warn!(
            "plugin_trust_workspace = false: workspace plugins require signatures and will be \
             rejected without a configured plugin_public_key_path"
        );
        TrustPolicy {
            verify_signatures: true,
            ..base.clone()
        }
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
    let ws_policy = local_trust_policy(&policy, cfg.tools.plugin_trust_workspace);
    let mut warnings = Vec::new();

    for name in &cfg.tools.enabled_plugins {
        // Skip plugins that are disabled at runtime. This works alongside
        // the `enabled_plugins` toggle: enabled_plugins controls which
        // workspace sources are loaded; disabled_plugins controls whether
        // an already-loaded or compiled-in plugin is active.
        if cfg.tools.disabled_plugins.contains(name) {
            tracing::trace!(
                plugin = %name,
                "skipping disabled workspace plugin (disabled_plugins)"
            );
            continue;
        }

        // Folded plugins with feature ON are served by compiled-in Rust code
        // (registered in main/mod.rs). Skip the shell-plugin dir so the two
        // paths don't double-register the same tool names. When the feature is
        // OFF, fall through to the shell-plugin path (graceful degradation).
        if folded_feature_enabled(name) {
            tracing::trace!(
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
            // bundled plugins under the data directory (`~/.local/share/kf-code/plugins`).
            plugins_dir().join(name)
        };
        if !resolved.exists() {
            warnings.push(format!(
                "{name}: plugin source directory does not exist: {resolved}",
                resolved = resolved.display()
            ));
            continue;
        }
        match registry.load_one(&resolved, ws_policy.clone()) {
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
///
/// Plugins listed in `disabled_plugins` are excluded at runtime — their
/// tools will not appear in the tool list even though the plugin is loaded
/// in the registry.
pub fn all_plugin_tools(
    registry: &PluginRegistry,
    shared_config: SharedConfig,
    audit_log: Option<std::sync::Arc<crate::shared::audit::AuditLog>>,
) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    let (disabled, global_sandbox) = {
        let cfg = crate::shared::read_shared_config(&shared_config);
        let disabled = cfg.tools.disabled_plugins.clone();
        let sandbox = cfg.security.sandbox.clone();
        (disabled, sandbox)
    };

    for hosted in registry.active_plugins() {
        let plugin_name = hosted.plugin.manifest.name.as_str();
        // Folded plugins with feature ON are served by compiled-in Rust tools
        // (registered in main/run_session.rs). Skip shell-wrapper creation so
        // a manifest loaded from the data dir can't double-register the same
        // tool names as the compiled-in impls (ADR-050).
        if folded_feature_enabled(plugin_name) {
            tracing::trace!(
                plugin = plugin_name,
                "skipping shell-wrapper for folded plugin (compiled-in)"
            );
            continue;
        }
        if disabled.contains(plugin_name) {
            tracing::trace!(plugin = plugin_name, "skipping disabled plugin tools");
            continue;
        }

        let root = hosted.plugin.root().to_path_buf();
        let per_plugin_sandbox =
            global_sandbox.merge_with(hosted.plugin.manifest().resource_limits.as_ref());
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
                    per_plugin_sandbox.clone(),
                    hosted.effective_trust,
                );
                let wrapper = match &audit_log {
                    Some(log) => wrapper.with_audit_log(std::sync::Arc::clone(log)),
                    None => wrapper,
                };
                tools.push(Arc::new(wrapper));
            }
        }
    }

    tools
}

/// Spawn a file-system watcher on the plugins directory that sends a
/// reload signal on `reload_tx` when a `kf-code.toml` or tool/hook
/// script changes (WO 11.4, ADR-059). The watcher debounces events for
/// 500ms (coalescing editor multi-file saves) before firing.
///
/// Returns the watcher handle (must be held alive for the watcher to
/// keep running). The watcher runs in a background thread; the
/// reload signal is a `()` on the channel.
pub fn spawn_plugin_watcher(
    plugins_dir: PathBuf,
    reload_tx: tokio::sync::mpsc::UnboundedSender<()>,
) -> Option<notify_debouncer_mini::Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>> {
    if !plugins_dir.is_dir() {
        return None;
    }
    use notify_debouncer_mini::{new_debouncer, DebounceEventResult};
    use std::time::Duration;

    let (tx, rx) = std::sync::mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(Duration::from_millis(500), tx).ok()?;

    if debouncer
        .watcher()
        .watch(
            &plugins_dir,
            notify_debouncer_mini::notify::RecursiveMode::Recursive,
        )
        .is_err()
    {
        tracing::warn!(
            dir = %plugins_dir.display(),
            "failed to start plugin directory watcher; hot-reload disabled"
        );
        return None;
    }

    // Spawn a thread that forwards debounced events to the async
    // reload channel. The thread exits when the watcher is dropped
    // (the `rx` channel closes).
    std::thread::spawn(move || {
        for result in rx {
            let events = match result {
                Ok(events) => events,
                Err(e) => {
                    tracing::trace!(error = %e, "plugin watcher debounce error");
                    continue;
                }
            };
            for event in events {
                let path = &event.path;
                let relevant = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| {
                        ext == "toml" || ext == "sh" || ext == "js" || ext == "ts" || ext == "py"
                    })
                    || path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == "kf-code.toml");
                if relevant {
                    tracing::trace!(
                        path = %path.display(),
                        "plugin file changed; triggering reload"
                    );
                    if reload_tx.send(()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    tracing::info!(
        dir = %plugins_dir.display(),
        "plugin hot-reload watcher started (500ms debounce)"
    );
    Some(debouncer)
}

#[cfg(test)]
mod loader_tests {
    use super::*;
    use crate::shared::Config;

    #[test]
    fn trust_policy_from_config_defaults() {
        let cfg = Config::default();
        let policy = trust_policy_from_config(&cfg);
        assert!(policy.verify_signatures, "default should verify sigs");
        assert!(policy.signature_key_path.is_none());
    }

    #[test]
    fn trust_policy_from_config_maps_all_fields() {
        let mut cfg = Config::default();
        cfg.tools.max_plugin_trust = kf_plugin_sdk::TrustTier::Network;
        cfg.tools.reject_on_excess_plugin_trust = true;
        cfg.tools.plugin_signature_validation = true;
        cfg.tools.plugin_public_key_path = Some("/keys/pub.key".into());
        let policy = trust_policy_from_config(&cfg);
        assert_eq!(policy.max, kf_plugin_sdk::TrustTier::Network);
        assert!(policy.reject_on_excess);
        assert!(policy.verify_signatures);
        assert_eq!(
            policy.signature_key_path,
            Some(PathBuf::from("/keys/pub.key"))
        );
    }

    #[test]
    fn trust_policy_from_config_no_key_path() {
        let mut cfg = Config::default();
        cfg.tools.plugin_signature_validation = true;
        cfg.tools.plugin_public_key_path = None;
        let policy = trust_policy_from_config(&cfg);
        assert!(policy.verify_signatures);
        assert!(policy.signature_key_path.is_none());
    }

    #[test]
    fn folded_feature_enabled_returns_false_for_unknown() {
        assert!(!folded_feature_enabled("nonexistent-plugin"));
    }

    #[test]
    fn is_folded_returns_false_for_unknown() {
        assert!(!is_folded("nonexistent-plugin"));
    }

    #[test]
    fn folded_feature_name_for_known_returns_name() {
        // "stratum" is always in the FOLDED_PLUGINS list
        assert_eq!(folded_feature("stratum"), Some("stratum"));
    }

    #[test]
    fn folded_feature_name_for_unknown_returns_none() {
        assert_eq!(folded_feature("nonexistent"), None);
    }

    #[test]
    fn is_folded_recognizes_all_known_folded_plugins() {
        for (name, _feature) in FOLDED_PLUGINS {
            assert!(
                is_folded(name),
                "is_folded should return true for known folded plugin '{name}'"
            );
        }
    }

    #[test]
    fn folded_feature_returns_each_known_feature_name() {
        for (name, feature) in FOLDED_PLUGINS {
            assert_eq!(
                folded_feature(name),
                Some(*feature),
                "folded_feature({name}) should return {feature:?}"
            );
        }
    }

    #[test]
    fn folded_feature_enabled_for_known_returns_feature_state() {
        // Whether each known folded plugin's feature is enabled depends on
        // build flags; we just assert the function does not panic for every
        // known name and returns a bool.
        for (name, _) in FOLDED_PLUGINS {
            let _ = folded_feature_enabled(name);
        }
    }

    #[test]
    fn plugins_dir_ends_with_plugins_subdir() {
        let dir = plugins_dir();
        assert!(
            dir.ends_with("plugins"),
            "plugins_dir should end with 'plugins', got {dir:?}"
        );
    }

    #[test]
    fn is_folded_is_case_sensitive() {
        // Plugin names are kebab-case identifiers; uppercase must not match.
        assert!(!is_folded("Stratum"));
        assert!(!is_folded("STRATUM"));
    }

    #[test]
    fn folded_feature_is_case_sensitive() {
        assert_eq!(folded_feature("Stratum"), None);
        assert_eq!(folded_feature("STRATUM"), None);
    }

    #[test]
    fn load_workspace_plugins_empty_enabled_returns_no_warnings() {
        let mut cfg = Config::default();
        cfg.tools.enabled_plugins.clear();
        cfg.tools.plugin_sources.clear();
        let mut registry = PluginRegistry::new();
        let warnings = load_workspace_plugins(&mut registry, &cfg);
        assert!(warnings.is_empty(), "no enabled plugins → no warnings");
        assert!(
            registry.active_plugins().is_empty(),
            "registry stays empty when nothing is enabled"
        );
    }

    #[test]
    fn load_workspace_plugins_warns_when_enabled_has_no_source() {
        let mut cfg = Config::default();
        cfg.tools.enabled_plugins.clear();
        cfg.tools.plugin_sources.clear();
        cfg.tools.enabled_plugins.push("ghost-plugin".into());
        let mut registry = PluginRegistry::new();
        let warnings = load_workspace_plugins(&mut registry, &cfg);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("ghost-plugin"));
        assert!(warnings[0].contains("no plugin_source"));
    }

    #[test]
    fn load_workspace_plugins_warns_when_source_dir_missing() {
        // A non-folded enabled name with a configured source that does not
        // exist on disk: the resolver falls back to the data-dir plugins path
        // and, when that also does not exist, emits a "does not exist" warning.
        let mut cfg = Config::default();
        cfg.tools.enabled_plugins.clear();
        cfg.tools.plugin_sources.clear();
        cfg.tools.enabled_plugins.push("never-built-plugin".into());
        cfg.tools.plugin_sources.insert(
            "never-built-plugin".into(),
            PathBuf::from("/nonexistent/path/that/does/not/exist"),
        );
        let mut registry = PluginRegistry::new();
        let warnings = load_workspace_plugins(&mut registry, &cfg);
        assert!(
            !warnings.is_empty(),
            "a missing source dir must produce a warning"
        );
        assert!(warnings.iter().any(|w| w.contains("never-built-plugin")));
        assert!(registry.active_plugins().is_empty());
    }

    #[test]
    fn signature_validation_is_default_on() {
        let cfg = Config::default();
        assert!(
            cfg.tools.plugin_signature_validation,
            "plugin_signature_validation should default to true (R7)"
        );
    }

    #[test]
    fn local_trust_policy_default_enforces_signatures() {
        // H9 / WO 27.4: workspace plugins are NOT trusted by default. The
        // policy forces verify_signatures = true (fail-closed) regardless of
        // the base policy, so unsigned plugins are rejected unless the
        // operator opts in via plugin_trust_workspace = true.
        let base = TrustPolicy {
            max: kf_plugin_sdk::TrustTier::Shell,
            reject_on_excess: true,
            verify_signatures: false,
            signature_key_path: None,
        };
        let local = local_trust_policy(&base, false);
        assert!(
            local.verify_signatures,
            "default (no opt-in) must enforce signatures on workspace plugins"
        );
        assert_eq!(local.max, base.max, "other fields must be preserved");
        assert_eq!(local.reject_on_excess, base.reject_on_excess);
        assert_eq!(local.signature_key_path, base.signature_key_path);
    }

    #[test]
    fn local_trust_policy_opt_in_bypasses_signature_verification() {
        let base = TrustPolicy {
            max: kf_plugin_sdk::TrustTier::Shell,
            reject_on_excess: true,
            verify_signatures: true,
            signature_key_path: Some(PathBuf::from("/keys/pub.key")),
        };
        let local = local_trust_policy(&base, true);
        assert!(
            !local.verify_signatures,
            "opt-in must bypass signature verification for workspace plugins"
        );
        assert_eq!(local.max, base.max, "other fields must be preserved");
        assert_eq!(local.reject_on_excess, base.reject_on_excess);
        assert_eq!(local.signature_key_path, base.signature_key_path);
    }
}
