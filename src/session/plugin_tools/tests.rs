use super::*;
use crate::shared::{Config, SharedConfig, ToolOutcome};
use crate::tools::ToolContext;
use kf_plugin_host::{PluginRegistry, TrustPolicy};
use kf_plugin_sdk::{Capability, Plugin, TrustTier};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
fn make_greet_plugin() -> (tempfile::TempDir, PluginRegistry, SharedConfig) {
    let tmp = tempfile::tempdir().unwrap();
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
description = "Greet someone"
command = "greet.sh"
"#,
    )
    .unwrap();
    std::fs::write(plugin_dir.join("greet.sh"), "#!/bin/sh\nprintf 'hello'").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(plugin_dir.join("greet.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(plugin_dir.join("greet.sh"), perms).unwrap();
    }

    let mut reg = PluginRegistry::new();
    reg.load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::Shell))
        .unwrap();

    let cfg = Arc::new(std::sync::RwLock::new(Config::default()));
    (tmp, reg, cfg)
}

#[cfg(unix)]
// WO 27.2-R2: was "known-broken" — root cause was SandboxConfig::default()
// zeroing rlimits (derive Default vs serde 300/2048/512), fixed in this commit.
// Verified passing 2026-08-11.
#[tokio::test]
async fn wrapper_for_plugin_tool() {
    let (_tmp, reg, cfg) = make_greet_plugin();
    let tools = all_plugin_tools(&reg, cfg, None);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].def().name, "demo/greet");

    let outcome = tools[0]
        .run(&ToolContext::new(), serde_json::Value::Null)
        .await;
    assert!(
        matches!(outcome, ToolOutcome::Success { ref content } if content == "hello"),
        "got: {outcome:?}"
    );
}

#[cfg(unix)]
// WO 27.2-R2: un-ignored after SandboxConfig::default() fix (838e611)
#[tokio::test]
async fn sandbox_uses_configured_sandbox_dir() {
    let (tmp, reg, cfg) = make_greet_plugin();
    let sandbox = tmp.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    {
        let mut cfg = cfg.write().unwrap();
        cfg.security.sandbox_dir = Some(sandbox.to_string_lossy().to_string());
    }

    // Replace the script with one that prints its cwd.
    let plugin_dir = reg
        .active_plugins()
        .first()
        .unwrap()
        .plugin
        .root()
        .to_path_buf();
    std::fs::write(plugin_dir.join("greet.sh"), "#!/bin/sh\npwd").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(plugin_dir.join("greet.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(plugin_dir.join("greet.sh"), perms).unwrap();
    }

    let tools = all_plugin_tools(&reg, cfg, None);
    assert_eq!(tools.len(), 1);

    let outcome = tools[0]
        .run(&ToolContext::new(), serde_json::Value::Null)
        .await;
    let cwd = match outcome {
        ToolOutcome::Success { content } => content,
        other => panic!("expected Success, got {other:?}"),
    }
    .trim()
    .to_string();
    assert_eq!(
        std::fs::canonicalize(Path::new(&cwd)).unwrap_or_else(|_| PathBuf::from(&cwd)),
        std::fs::canonicalize(&sandbox).unwrap()
    );
}

#[cfg(unix)]
// WO 27.2-R2: un-ignored after SandboxConfig::default() fix (838e611)
#[tokio::test]
async fn sandbox_uses_current_dir_when_sandbox_dir_empty() {
    let (_tmp, reg, cfg) = make_greet_plugin();
    {
        let mut cfg = cfg.write().unwrap();
        // Explicit empty string is the "unsandboxed" escape hatch, but
        // plugin tools must still run in the user's cwd, not the plugin
        // installation directory.
        cfg.security.sandbox_dir = Some(String::new());
    }

    let plugin_dir = reg
        .active_plugins()
        .first()
        .unwrap()
        .plugin
        .root()
        .to_path_buf();
    std::fs::write(plugin_dir.join("greet.sh"), "#!/bin/sh\npwd").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(plugin_dir.join("greet.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(plugin_dir.join("greet.sh"), perms).unwrap();
    }

    let tools = all_plugin_tools(&reg, cfg, None);
    let outcome = tools[0]
        .run(&ToolContext::new(), serde_json::Value::Null)
        .await;
    let cwd = match outcome {
        ToolOutcome::Success { content } => content,
        other => panic!("expected Success, got {other:?}"),
    }
    .trim()
    .to_string();
    let expected = std::env::current_dir().unwrap();
    assert_eq!(
        std::fs::canonicalize(Path::new(&cwd)).unwrap_or_else(|_| PathBuf::from(&cwd)),
        std::fs::canonicalize(&expected).unwrap()
    );

    // Sanity check: the cwd is NOT the plugin directory.
    assert_ne!(
        std::fs::canonicalize(Path::new(&cwd)).unwrap_or_else(|_| PathBuf::from(&cwd)),
        std::fs::canonicalize(&plugin_dir).unwrap()
    );
}

#[cfg(unix)]
// WO 27.2-R2: un-ignored after SandboxConfig::default() fix (838e611)
#[tokio::test]
async fn curated_env_blocks_unlisted_vars() {
    let (_tmp, reg, cfg) = make_greet_plugin();
    let plugin_dir = reg
        .active_plugins()
        .first()
        .unwrap()
        .plugin
        .root()
        .to_path_buf();
    // Replace greet.sh with one that echoes a non-baseline variable.
    std::fs::write(
        plugin_dir.join("greet.sh"),
        "#!/bin/sh\nprintf '%s' \"$KF_CODE_SECRET_VAR\"",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(plugin_dir.join("greet.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(plugin_dir.join("greet.sh"), perms).unwrap();
    }

    std::env::set_var("KF_CODE_SECRET_VAR", "leaked");
    let tools = all_plugin_tools(&reg, cfg, None);
    let outcome = tools[0]
        .run(&ToolContext::new(), serde_json::Value::Null)
        .await;
    std::env::remove_var("KF_CODE_SECRET_VAR");

    assert!(
        matches!(outcome, ToolOutcome::Success { ref content } if content.is_empty()),
        "unlisted env var leaked into plugin tool: {outcome:?}"
    );
}

/// Plugin tool subprocesses receive a sanitized PATH so shell wrappers can
/// resolve standard utilities even when kf-code is launched with a minimal
/// or world-writable PATH.
#[cfg(unix)]
// WO 27.2-R2: un-ignored after SandboxConfig::default() fix (838e611)
#[tokio::test]
async fn curated_env_sanitizes_path_for_plugin_tools() {
    let (_tmp, reg, cfg) = make_greet_plugin();
    let plugin_dir = reg
        .active_plugins()
        .first()
        .unwrap()
        .plugin
        .root()
        .to_path_buf();

    // Script asks the shell to locate `sh` via PATH. With an empty/malicious
    // host PATH this would fail; the sanitized PATH must include /bin.
    std::fs::write(
        plugin_dir.join("greet.sh"),
        "#!/bin/sh\ncommand -v sh || printf 'NOT_FOUND'",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(plugin_dir.join("greet.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(plugin_dir.join("greet.sh"), perms).unwrap();
    }

    struct PathGuard {
        prior: Option<String>,
    }
    impl PathGuard {
        fn set(value: &str) -> Self {
            let prior = std::env::var("PATH").ok();
            std::env::set_var("PATH", value);
            Self { prior }
        }
    }
    impl Drop for PathGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    let _guard = PathGuard::set("/tmp/evil");
    let tools = all_plugin_tools(&reg, cfg, None);
    let outcome = tools[0]
        .run(&ToolContext::new(), serde_json::Value::Null)
        .await;

    assert!(
        matches!(outcome, ToolOutcome::Success { ref content } if content.trim().ends_with("/bin/sh")),
        "plugin tool should resolve sh via sanitized PATH, got: {outcome:?}"
    );
}

#[test]
fn load_workspace_plugins_loads_enabled_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("workspace-plugin");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(
        source_dir.join("kf-code.toml"),
        r#"
name = "workspace-demo"
version = "0.1.0"
description = "workspace demo"
trust = "read-only"

[[capabilities]]
type = "skill"
trigger = "/workspace-demo"
prompt = "hello"
"#,
    )
    .unwrap();

    let mut cfg = Config::default();
    cfg.tools.plugin_sources = {
        let mut m = std::collections::HashMap::new();
        m.insert("workspace-demo".to_string(), source_dir.clone());
        m
    };
    cfg.tools.enabled_plugins = vec!["workspace-demo".to_string()];
    cfg.tools.plugin_trust_workspace = true;

    let mut registry = PluginRegistry::new();
    let warnings = load_workspace_plugins(&mut registry, &cfg);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert!(registry.find_active_by_name("workspace-demo").is_some());
}

#[test]
fn load_workspace_plugins_warns_for_missing_source() {
    let mut cfg = Config::default();
    cfg.tools.plugin_sources = {
        let mut m = std::collections::HashMap::new();
        m.insert("missing".to_string(), PathBuf::from("/does/not/exist"));
        m
    };
    cfg.tools.enabled_plugins = vec!["missing".to_string()];

    let mut registry = PluginRegistry::new();
    let warnings = load_workspace_plugins(&mut registry, &cfg);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("does not exist"));
}

struct DataDirGuard {
    prior: Option<String>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl DataDirGuard {
    fn set(value: &str) -> Self {
        let _lock = crate::session::test_data_dir_lock().blocking_lock();
        let prior = std::env::var("KF_CODE_DATA_DIR").ok();
        std::env::set_var("KF_CODE_DATA_DIR", value);
        Self { prior, _lock }
    }

    async fn set_async(value: &str) -> Self {
        let _lock = crate::session::test_data_dir_lock().lock().await;
        let prior = std::env::var("KF_CODE_DATA_DIR").ok();
        std::env::set_var("KF_CODE_DATA_DIR", value);
        Self { prior, _lock }
    }
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var("KF_CODE_DATA_DIR", v),
            None => std::env::remove_var("KF_CODE_DATA_DIR"),
        }
    }
}

/// `npm_bin_dirs()` must include the source-layout Node SDK bin directory
/// when the running binary lives under the workspace `target/` tree, even if
/// the data directory has no Node SDK installed. This lets developers run
/// Node SDK plugin tools from a source build without a global `tsc`/`pyright`.
#[test]
fn npm_bin_dirs_includes_source_layout_from_target_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = DataDirGuard::set(tmp.path().to_string_lossy().as_ref());

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_bin = repo_root.join("npm/kf-plugin/node_modules/.bin");
    // The source-layout Node SDK install only exists after `npm ci`, which
    // the Rust CI jobs don't run. The detection logic is what we're testing,
    // not whether a sibling language's install happened, so ensure the
    // gitignored dir is present before reading. `create_dir_all` is a no-op
    // when an install already exists; otherwise it makes an empty `.bin`
    // (node_modules is gitignored, so this never pollutes the tree).
    std::fs::create_dir_all(&source_bin).unwrap();

    let dirs = npm_bin_dirs();
    assert!(
        dirs.contains(&source_bin),
        "expected npm_bin_dirs to contain source-layout bin {source_bin:?}; got {dirs:?}"
    );

    // The temporary data directory has no npm install, so no data-dir entry
    // should be present.
    let data_bin = tmp.path().join("npm/kf-plugin/node_modules/.bin");
    assert!(
        !dirs.contains(&data_bin),
        "unexpected data-dir bin {data_bin:?} in {dirs:?}"
    );
}

/// When the data directory contains a bundled Node SDK install, its bin
/// directory is also included alongside the source-layout candidate.
#[test]
fn npm_bin_dirs_includes_data_dir_install() {
    let tmp = tempfile::tempdir().unwrap();
    let data_bin = tmp.path().join("npm/kf-plugin/node_modules/.bin");
    std::fs::create_dir_all(&data_bin).unwrap();
    let _guard = DataDirGuard::set(tmp.path().to_string_lossy().as_ref());

    let dirs = npm_bin_dirs();
    assert!(
        dirs.contains(&data_bin),
        "expected npm_bin_dirs to contain data-dir bin {data_bin:?}; got {dirs:?}"
    );
}

/// When a configured workspace plugin source path does not exist (e.g. a
/// release binary whose compile-time source-repo paths are stale), the host
/// falls back to the data-directory plugins folder before giving up.
#[test]
fn workspace_plugin_source_falls_back_to_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let demo = plugins.join("demo");
    std::fs::create_dir_all(&demo).unwrap();
    std::fs::write(
        demo.join("kf-code.toml"),
        r#"
name = "demo"
version = "0.1.0"
description = "demo"
trust = "shell"

[[capabilities]]
type = "tool"
name = "demo/hello"
description = "hello"
command = "hello.sh"
"#,
    )
    .unwrap();
    std::fs::write(demo.join("hello.sh"), "#!/bin/sh\nprintf hello").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(demo.join("hello.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(demo.join("hello.sh"), perms).unwrap();
    }

    let _guard = DataDirGuard::set(&tmp.path().to_string_lossy());
    let mut cfg = Config::default();
    cfg.tools.plugin_sources = [("demo".to_string(), PathBuf::from("/nonexistent/demo"))]
        .into_iter()
        .collect();
    cfg.tools.enabled_plugins = vec!["demo".to_string()];
    cfg.tools.plugin_trust_workspace = true;

    let mut registry = PluginRegistry::new();
    let warnings = load_workspace_plugins(&mut registry, &cfg);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert!(
        registry.find_active_by_name("demo").is_some(),
        "demo plugin should load from data-dir fallback"
    );
}

/// Recursively copy `src` into `dst`, preserving permissions on Unix and
/// symlinks where possible. Used by installed-layout regression tests.
fn copy_dir_all(
    src: impl AsRef<std::path::Path>,
    dst: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(&dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.as_ref().join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(entry.path(), &dest_path)?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                let target = std::fs::read_link(entry.path())?;
                symlink(target, dest_path)?;
            }
            #[cfg(not(unix))]
            {
                // On Windows follow the symlink; bundled plugins contain
                // no symlinks that matter at load time.
                if entry.path().is_dir() {
                    copy_dir_all(entry.path(), &dest_path)?;
                } else {
                    std::fs::copy(entry.path(), &dest_path)?;
                }
            }
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
            #[cfg(unix)]
            {
                let perms = entry.metadata()?.permissions();
                std::fs::set_permissions(&dest_path, perms)?;
            }
        }
    }
    Ok(())
}

/// Installed-layout regression: when the data directory contains a copy of
/// the bundled `plugins/` tree (as `install.sh` produces), the plugin host
/// loads every bundled shell plugin from that directory without warnings.
/// This catches packaging mistakes that leave tools referenced by a manifest
/// missing from the installed plugin root.
///
/// Only `kf-plugin` ships as a shell plugin source. The folded plugins
/// (`stratum`, `kf-budget`) are compiled-in behind cargo features (see
/// `folded_feature_enabled`) and do not ship in the bundled `plugins/` tree.
// WO 27.2-R2: un-ignored after rewriting stale premise (stratum/kf-budget
// are compiled-in, not shell-shipped).
#[test]
fn bundled_plugins_load_from_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let installed_plugins = tmp.path().join("plugins");

    // Copy the in-repo bundled plugins into a temp data directory so we
    // exercise the same code path an installed release uses.
    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins");
    copy_dir_all(&repo_plugins, &installed_plugins).unwrap();

    let _guard = DataDirGuard::set(&tmp.path().to_string_lossy());
    let mut cfg = Config::default();
    cfg.tools.plugin_signature_validation = false;
    cfg.tools.plugin_trust_workspace = true;
    let (registry, warnings) =
        load_plugin_registry(&cfg).expect("loading installed plugins should not fail");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    let names: Vec<String> = registry
        .active_plugins()
        .iter()
        .map(|p| p.plugin.manifest().name.clone())
        .collect();
    // Only kf-plugin ships as a shell plugin source.
    assert!(
        names.contains(&"kf-plugin".to_string()),
        "expected bundled plugin \"kf-plugin\" to load from data dir; got {names:?}"
    );
    // Folded plugins (stratum, kf-budget) are compiled-in behind cargo
    // features; they must NOT shell-load from the bundled tree (no shell
    // source dir ships, and the shell loader skips them when feature-on).
    for folded in ["stratum", "kf-budget"] {
        assert!(
            !names.contains(&folded.to_string()),
            "folded plugin {folded:?} should not shell-load from bundled tree (compiled-in); got {names:?}"
        );
    }
}

/// Every declared tool command file must exist in the installed plugin root.
/// This catches manifest drift and packaging mistakes that omit a tool
/// script from a release archive.
#[test]
fn bundled_plugin_tool_commands_exist_in_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let installed_plugins = tmp.path().join("plugins");
    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins");
    copy_dir_all(&repo_plugins, &installed_plugins).unwrap();

    let _guard = DataDirGuard::set(&tmp.path().to_string_lossy());
    let mut cfg = Config::default();
    cfg.tools.plugin_signature_validation = false;
    cfg.tools.plugin_trust_workspace = true;
    let (registry, warnings) =
        load_plugin_registry(&cfg).expect("loading installed plugins should not fail");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    for hosted in registry.active_plugins() {
        let root = hosted.plugin.root().to_path_buf();
        for cap in hosted.plugin.tools() {
            if let kf_plugin_sdk::Capability::Tool {
                name,
                command: Some(cmd),
                ..
            } = cap
            {
                let path = root.join(cmd);
                assert!(
                    path.exists(),
                    "tool {name:?} command missing: {}",
                    path.display()
                );
            }
        }
    }
}

/// End-to-end installed-layout regression for a Rust-binary-backed plugin:
/// `stratum_mode` must return the active mode through the host's
/// `PluginToolWrapper`. Skipped when the workspace `stratum` binary is not
/// built (e.g. a bare `cargo test -p kf-code`).
#[tokio::test]
async fn bundled_stratum_mode_tool_executes_via_host() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stratum_bin = [
        repo_root.join("target/debug/stratum"),
        repo_root.join("target/release/stratum"),
    ]
    .into_iter()
    .find(|p| p.exists());
    let Some(stratum_bin) = stratum_bin else {
        eprintln!("skipping stratum_mode end-to-end test: stratum binary not built");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let installed_plugins = tmp.path().join("plugins");
    let repo_plugins = repo_root.join("plugins");
    copy_dir_all(&repo_plugins, &installed_plugins).unwrap();

    // Copy the stratum binary next to the plugin scripts so the installed
    // layout can resolve it without mutating the global PATH (which would
    // race with other concurrent tests).
    let installed_stratum_tools = installed_plugins.join("stratum/tools");
    std::fs::copy(&stratum_bin, installed_stratum_tools.join("stratum")).unwrap();

    let _data_guard = DataDirGuard::set_async(&tmp.path().to_string_lossy()).await;
    let mut cfg = Config::default();
    cfg.tools.plugin_signature_validation = false;
    cfg.tools.plugin_trust_workspace = true;
    let (registry, warnings) =
        load_plugin_registry(&cfg).expect("loading installed plugins should not fail");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    let tools = all_plugin_tools(
        &registry,
        Arc::new(std::sync::RwLock::new(Config::default())),
        None,
    );
    let tool = tools
        .iter()
        .find(|t| t.def().name == "stratum_mode")
        .expect("stratum_mode should be registered");

    let outcome = tool.run(&ToolContext::new(), serde_json::json!({})).await;
    assert!(
        matches!(outcome, ToolOutcome::Success { ref content } if content.trim() == "full"),
        "expected stratum_mode to return 'full', got {outcome:?}"
    );
}

/// End-to-end installed-layout regression for the Node SDK plugin: the
/// Skipped when node or the built SDK is not available (e.g. a bare
/// `cargo test -p kf-code` without `npm ci`). Marked #[ignore]
/// because without the SDK build step the test silently no-ops and
/// provides no coverage.
#[tokio::test]
#[ignore]
async fn bundled_node_sdk_tool_executes_via_host() {
    fn which_node() -> Option<PathBuf> {
        std::env::var("PATH").ok().and_then(|path| {
            path.split(':').find_map(|dir| {
                let candidate = PathBuf::from(dir).join("node");
                if candidate.is_file() {
                    Some(candidate)
                } else {
                    None
                }
            })
        })
    }

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_sdk = repo_root.join("npm/kf-plugin/apps/cli/dist/index.js");
    if which_node().is_none() || !repo_sdk.exists() {
        eprintln!("skipping Node SDK end-to-end test: node or built SDK not available");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let installed_plugins = tmp.path().join("plugins");
    let installed_npm = tmp.path().join("npm/kf-plugin");
    let repo_plugins = repo_root.join("plugins");
    let repo_npm = repo_root.join("npm/kf-plugin");
    copy_dir_all(&repo_plugins, &installed_plugins).unwrap();
    copy_dir_all(&repo_npm, &installed_npm).unwrap();

    let _guard = DataDirGuard::set_async(&tmp.path().to_string_lossy()).await;
    let mut cfg = Config::default();
    cfg.tools.plugin_signature_validation = false;
    cfg.tools.plugin_trust_workspace = true;
    let (registry, warnings) =
        load_plugin_registry(&cfg).expect("loading installed plugins should not fail");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    let tools = all_plugin_tools(
        &registry,
        Arc::new(std::sync::RwLock::new(Config::default())),
        None,
    );
    let tool = tools
        .iter()
        .find(|t| t.def().name == "plugin_tools")
        .expect("plugin_tools should be registered");

    let outcome = tool.run(&ToolContext::new(), serde_json::json!({})).await;
    assert!(
        matches!(outcome, ToolOutcome::Success { ref content } if content.contains("KirkForge Native Lint Engines")),
        "expected plugin_tools to list native lint engines, got {outcome:?}"
    );
}

/// Verify the built-in workspace plugin sources are registered by default,
/// exist on disk, and can be loaded by the plugin host.
///
/// Only `kf-plugin` ships as a shell plugin source. The folded plugins
/// (`stratum`, `kf-budget`) are compiled-in behind cargo features; their
/// presence in `enabled_plugins` is governed by `folded_feature_enabled`,
/// not by `plugin_sources`.
// WO 27.2-R2: un-ignored after rewriting stale premise (stratum/kf-budget
// are compiled-in, not in default plugin_sources).
#[test]
fn default_plugin_sources_are_present_and_loadable() {
    let base = Config::default();

    // Only kf-plugin ships as a shell plugin source.
    let mut shell_sources: Vec<&str> = base
        .tools
        .plugin_sources
        .keys()
        .map(|s| s.as_str())
        .collect();
    shell_sources.sort();
    assert_eq!(
        shell_sources,
        vec!["kf-plugin"],
        "default plugin_sources should contain only kf-plugin; got {shell_sources:?}"
    );
    // The folded plugins must NOT appear in plugin_sources — they're
    // compiled-in (feature on) or have no shell fallback (feature off).
    for folded in &["stratum", "kf-budget"] {
        assert!(
            !base.tools.plugin_sources.contains_key(*folded),
            "folded plugin {folded:?} should not be in default plugin_sources"
        );
    }

    // Folded plugins' presence in enabled_plugins tracks the feature flag.
    for folded in &["stratum", "kf-budget"] {
        let in_enabled = base.tools.enabled_plugins.iter().any(|n| n == folded);
        assert_eq!(
            in_enabled,
            crate::session::plugin_tools::folded_feature_enabled(folded),
            "enabled_plugins membership for {folded:?} must match folded_feature_enabled"
        );
    }

    // Load with signature checks disabled — this test pins source presence
    // + loadability, not signature verification (covered elsewhere).
    let mut cfg = Config::default();
    cfg.tools.plugin_signature_validation = false;
    cfg.tools.plugin_trust_workspace = true;

    let mut registry = PluginRegistry::new();
    let warnings = load_workspace_plugins(&mut registry, &cfg);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    // kf-plugin is always shell-loaded from its default source.
    assert!(
        registry.find_active_by_name("kf-plugin").is_some(),
        "kf-plugin should shell-load from default sources"
    );
    // Folded plugins are never shell-loaded (compiled-in or no source).
    for folded in &["stratum", "kf-budget"] {
        assert!(
            registry.find_active_by_name(folded).is_none(),
            "folded plugin {folded:?} should not shell-load (compiled-in or no source)"
        );
    }
}

/// Verify that folded plugins with feature OFF fall back to shell loading.
#[test]
fn folded_plugin_shell_fallback_when_feature_off() {
    let folded_names = ["stratum", "kf-budget"];
    let base = Config::default();

    // Only test plugins whose feature is NOT compiled in AND whose source dir
    // exists in the config.
    let off: Vec<&str> = folded_names
        .iter()
        .copied()
        .filter(|n| !crate::session::plugin_tools::folded_feature_enabled(n))
        .filter(|n| base.tools.plugin_sources.contains_key(*n))
        .collect();
    if off.is_empty() {
        return;
    }

    let mut cfg = Config::default();
    cfg.tools.plugin_sources = base.tools.plugin_sources;
    cfg.tools.enabled_plugins = off.iter().map(|s| s.to_string()).collect();

    let mut registry = PluginRegistry::new();
    let warnings = load_workspace_plugins(&mut registry, &cfg);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    for name in &off {
        assert!(
            registry.find_active_by_name(name).is_some(),
            "folded plugin '{name}' with feature off should shell-load as fallback"
        );
    }
}

/// Verify `is_folded` and `folded_feature` return correct values.
#[test]
fn folded_plugin_identification() {
    assert!(crate::session::plugin_tools::is_folded("stratum"));
    assert!(crate::session::plugin_tools::is_folded("kf-budget"));
    assert!(!crate::session::plugin_tools::is_folded("kf-plugin"));
    assert!(!crate::session::plugin_tools::is_folded("custom-plugin"));

    assert_eq!(
        crate::session::plugin_tools::folded_feature("stratum"),
        Some("stratum")
    );
    assert_eq!(
        crate::session::plugin_tools::folded_feature("kf-budget"),
        Some("budget")
    );
    assert_eq!(
        crate::session::plugin_tools::folded_feature("kf-plugin"),
        None
    );
}

// ── WO 18.0.3: budget registration gate regression test ──
//
// The runtime gate in `run_session.rs` and `executor/mod.rs` must use the
// same plugin name as `default_plugin_sources()` and `folded_feature_enabled()`.
// A previous bug used "kf-plugin-sdk3" instead of "kf-budget", silently
// disabling budget tools/hooks on default builds.

#[cfg(feature = "budget")]
mod budget_registration {
    use super::*;

    #[test]
    fn budget_tools_present_in_default_toolset() {
        let cfg = Config::default();

        // The default config must include kf-budget in enabled_plugins.
        assert!(
            cfg.tools.enabled_plugins.iter().any(|n| n == "kf-budget"),
            "default config must include kf-budget in enabled_plugins, got: {:?}",
            cfg.tools.enabled_plugins
        );

        // kf-budget must not be in disabled_plugins.
        assert!(
            !cfg.tools.disabled_plugins.contains("kf-budget"),
            "default config must not have kf-budget in disabled_plugins"
        );

        // folded_feature_enabled must return true for kf-budget when the
        // budget feature is compiled in.
        assert!(
            folded_feature_enabled("kf-budget"),
            "folded_feature_enabled must return true for kf-budget when the budget feature is on"
        );

        // The runtime gate must use the same name as the config.
        // This assertion would have caught the "kf-plugin-sdk3" bug.
        let gate_allows_budget = cfg.tools.enabled_plugins.iter().any(|n| n == "kf-budget")
            && !cfg.tools.disabled_plugins.contains("kf-budget");
        assert!(
            gate_allows_budget,
            "runtime gate must allow budget when kf-budget is in enabled_plugins and not in disabled_plugins"
        );

        // Verify budget tools are producible.
        let budget = crate::session::budget::new_session_budget(&cfg);
        let store = crate::session::budget::new_session_store();
        let tools = crate::session::budget::all_budget_tools(
            &budget,
            &store,
            Arc::new(kf_compress_core::store::InMemoryOffloadStore::new()),
        );
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(
            names.contains(&"budget_status"),
            "budget tools must include budget_status, got: {names:?}"
        );
        assert!(
            names.contains(&"budget_set"),
            "budget tools must include budget_set, got: {names:?}"
        );
    }

    #[test]
    fn folded_feature_enabled_nonexistent_returns_false() {
        assert!(
            !folded_feature_enabled("nonexistent-plugin"),
            "folded_feature_enabled must return false for a nonexistent plugin"
        );
    }

    #[cfg(feature = "stratum")]
    #[test]
    fn stratum_tools_present_in_default_toolset() {
        let cfg = Config::default();
        assert!(
            cfg.tools.enabled_plugins.iter().any(|n| n == "stratum"),
            "default config must include stratum in enabled_plugins"
        );
        assert!(
            folded_feature_enabled("stratum"),
            "folded_feature_enabled must return true for stratum when the feature is on"
        );
        let tools = crate::session::stratum::stratum_tools(Arc::new(
            kf_compress_core::store::InMemoryOffloadStore::new(),
        ));
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(
            names.contains(&"stratum_run"),
            "stratum tools must include stratum_run, got: {names:?}"
        );
        assert!(
            names.contains(&"stratum_apply"),
            "stratum tools must include stratum_apply, got: {names:?}"
        );
    }
}

// ── WO 11.9: plugin system end-to-end integration test ──
//
// Loads a mock plugin declaring all 4 capability kinds (skill, tool,
// hook, verifier) and exercises each through the appropriate path:
//  - skill: rendered prompt via Plugin::skill_prompt
//  - tool: PluginToolWrapper.run (subprocess with curated env)
//  - hook: HookRunner.run_decision (exit 0 → Allow, exit 2 → Deny)
//  - verifier: PluginVerifier via the host subprocess
// Asserts the trust-filtering contract: at ReadOnly max, the shell tool
// and hook are filtered out; the skill and verifier remain.

#[cfg(unix)]
mod e2e {
    use super::*;
    use crate::session::hooks::{HookDecision, HookRunner};
    use crate::shared::audit::{AuditEntry, AuditLog};
    use crate::shared::ToolError;
    use kf_plugin_host::VerifierVerdict;
    use kf_plugin_sdk::{Capability, Plugin, TrustTier};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn chmod_x(path: &std::path::Path) {
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    /// Build a mock plugin dir with all 4 capability kinds.
    fn make_e2e_plugin(root: &std::path::Path) {
        let hooks_dir = root.join("hooks");
        let tools_dir = root.join("tools");
        let verifier_dir = root.join("verifiers");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::create_dir_all(&verifier_dir).unwrap();

        std::fs::write(
            root.join("kf-code.toml"),
            r#"
name = "e2e-plugin"
version = "0.1.0"
description = "e2e mock with all 4 capability kinds"
trust = "shell"

[[capabilities]]
type = "skill"
trigger = "/e2e"
prompt = "E2E skill: {{args}}"

[[capabilities]]
type = "tool"
name = "e2e/echo"
description = "echo its args + curated env"
command = "tools/echo.sh"

[[capabilities]]
type = "hook"
event = "pre-tool-bash"
command = "hooks/pre-tool-bash.sh"

[[capabilities]]
type = "verifier"
name = "e2e-check"
priority = 1
command = "verifiers/check.sh"
"#,
        )
        .unwrap();

        // Tool: echoes a sentinel so the test knows it ran, and prints
        // a non-baseline env var to prove env curation.
        std::fs::write(
            tools_dir.join("echo.sh"),
            "#!/bin/sh\nprintf 'e2e-tool-ran:%s' \"$1\"\n",
        )
        .unwrap();
        chmod_x(&tools_dir.join("echo.sh"));

        // Hook: exit 0 (allow) by default; exit 2 when KF_DENY=1 is set.
        std::fs::write(
            hooks_dir.join("pre-tool-bash.sh"),
            "#!/bin/sh\nif [ \"$KF_DENY\" = 1 ]; then echo 'e2e-deny' >&2; exit 2; fi\nexit 0\n",
        )
        .unwrap();
        chmod_x(&hooks_dir.join("pre-tool-bash.sh"));

        // Verifier: exits 1 (error verdict) with a message on stderr.
        std::fs::write(
            verifier_dir.join("check.sh"),
            "#!/bin/sh\necho 'e2e-verifier: found issue' >&2\nexit 1\n",
        )
        .unwrap();
        chmod_x(&verifier_dir.join("check.sh"));
    }

    // WO 27.2-R2: un-ignored after SandboxConfig::default() fix (838e611)
    #[tokio::test]
    async fn e2e_plugin_all_four_capability_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let plugin_dir = plugins_dir.join("e2e-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        make_e2e_plugin(&plugin_dir);

        // Load at Shell trust (all 4 capabilities should be active).
        let mut registry = PluginRegistry::new();
        let warnings = registry
            .load_from_dir(&plugins_dir, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        assert!(warnings.is_empty(), "load warnings: {warnings:?}");
        assert_eq!(registry.active_count(), 1);

        // ── 1. Skill registered + callable ──
        assert!(registry.skill_by_trigger("/e2e").is_some());
        let (_, plugin) = registry.skill_by_trigger("/e2e").unwrap();
        let prompt = plugin.skill_prompt("/e2e", "hello").unwrap();
        assert!(prompt.contains("E2E skill: hello"), "prompt: {prompt}");

        // ── 2. Tool registered + callable ──
        assert!(registry.tool_by_name("e2e/echo").is_some());
        let cfg = Arc::new(std::sync::RwLock::new(Config::default()));
        let tools = all_plugin_tools(&registry, cfg, None);
        assert_eq!(tools.len(), 1, "expected 1 plugin tool");
        let outcome = tools[0]
            .run(&ToolContext::new(), serde_json::json!({"arg": "world"}))
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("e2e-tool-ran"),
                    "tool output should contain sentinel: {content}"
                );
            }
            other => panic!("expected tool Success, got {other:?}"),
        }

        // ── 3. Hook fires + verdict correct (Allow and Deny cases) ──
        assert!(!registry.hooks_for_event("pre-tool-bash").is_empty());
        let mut hook_runner = HookRunner::new(tmp.path().join("unused-hooks"));
        hook_runner.load_plugin_hooks(&registry, &std::collections::HashSet::new());

        // Allow case (no KF_DENY).
        let allow = hook_runner
            .run_decision("pre-tool-bash", &[], &Config::default())
            .await;
        assert_eq!(allow, HookDecision::Allow, "allow case");

        // Deny case (KF_DENY=1).
        let deny = hook_runner
            .run_decision("pre-tool-bash", &[("KF_DENY", "1")], &Config::default())
            .await;
        assert!(
            matches!(deny, HookDecision::Deny(ref r) if r.contains("e2e-deny")),
            "deny case: {deny:?}"
        );

        // ── 4. Verifier registered + produces a verdict ──
        assert!(registry.verifier_by_name("e2e-check").is_some());
        let (_, vplugin) = registry.verifier_by_name("e2e-check").unwrap();
        let vcaps: Vec<Capability> = vplugin.verifiers();
        let vcap = vcaps
            .iter()
            .find(|c| matches!(c, Capability::Verifier { name, .. } if name == "e2e-check"))
            .unwrap();
        if let Capability::Verifier {
            command: Some(_), ..
        } = vcap
        {
            let pv = kf_plugin_host::PluginVerifier::from_capability(vcap, vplugin.root())
                .expect("verifier should build from capability");
            let mut env = std::collections::HashMap::new();
            env.insert("KF_EVENT".to_string(), "post-tool-bash".to_string());
            env.insert("KF_TOOL_NAME".to_string(), "bash".to_string());
            let verdict = pv.run(&env).expect("verifier should execute");
            match verdict {
                VerifierVerdict::Fail { message } => {
                    assert!(
                        message.contains("e2e-verifier"),
                        "verifier fail message: {message}"
                    );
                }
                VerifierVerdict::Pass => panic!("expected Fail verdict, got Pass"),
            }
        } else {
            panic!("e2e-check verifier should have a command");
        }

        // ── 5. Trust filtering: at ReadOnly max, tool + hook filtered out ──
        let mut reg_ro = PluginRegistry::new();
        let warnings_ro = reg_ro
            .load_from_dir(
                &plugins_dir,
                TrustPolicy::up_to(TrustTier::ReadOnly).with_reject_on_excess(false),
            )
            .unwrap();
        // Plugin is active (skill + verifier are ReadOnly-kind) but tool
        // and hook (Shell-kind) are filtered.
        assert_eq!(reg_ro.active_count(), 1, "warnings: {warnings_ro:?}");
        assert!(reg_ro.skill_by_trigger("/e2e").is_some(), "skill survives");
        assert!(reg_ro.tool_by_name("e2e/echo").is_none(), "tool filtered");
        assert!(
            reg_ro.hooks_for_event("pre-tool-bash").is_empty(),
            "hook filtered"
        );
        assert!(
            reg_ro.verifier_by_name("e2e-check").is_some(),
            "verifier survives"
        );

        // ── 6. Audit log: hook verdict recorded (WO 11.6) ──
        let audit_path = tmp.path().join("e2e-audit.ndjson");
        let log = Arc::new(AuditLog::new(Some(audit_path.clone())));
        let mut audited_runner = HookRunner::new(tmp.path().join("unused-hooks"));
        audited_runner.set_audit_log(log);
        audited_runner.load_plugin_hooks(&registry, &std::collections::HashSet::new());
        let _ = audited_runner
            .run_decision("pre-tool-bash", &[("KF_DENY", "1")], &Config::default())
            .await;
        drop(audited_runner);
        let audit_contents = std::fs::read_to_string(&audit_path).unwrap();
        assert!(
            audit_contents.contains("\"kind\":\"hook\""),
            "audit log should contain a hook entry: {audit_contents}"
        );
        assert!(
            audit_contents.contains("\"verdict\":\"deny\""),
            "audit log should contain a deny verdict: {audit_contents}"
        );
        assert!(
            audit_contents.contains("e2e-plugin"),
            "audit log should attribute to e2e-plugin: {audit_contents}"
        );
    }
}

#[cfg(unix)]
mod resource_limits_tests {
    use super::*;
    use crate::shared::SandboxConfig;
    use crate::shared::ToolError;
    use kf_plugin_sdk::{Plugin, ResourceLimits, TrustTier};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn make_cpu_burn_plugin(cpu_secs: u64) -> (tempfile::TempDir, PluginRegistry, SharedConfig) {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        let plugin_dir = plugins.join("burn");
        let tools_dir = plugin_dir.join("tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            format!(
                r#"
name = "burn"
version = "0.1.0"
description = "cpu burn"
trust = "shell"

[[capabilities]]
type = "tool"
name = "burn/spin"
description = "infinite cpu loop"
command = "tools/spin.sh"

[resource_limits]
cpu_secs = {cpu_secs}
"#,
            ),
        )
        .unwrap();
        std::fs::write(tools_dir.join("spin.sh"), "#!/bin/sh\nwhile :; do :; done").unwrap();
        let mut perms = std::fs::metadata(tools_dir.join("spin.sh"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(tools_dir.join("spin.sh"), perms).unwrap();

        let mut reg = PluginRegistry::new();
        reg.load_from_dir(&plugins, TrustPolicy::up_to(TrustTier::Shell))
            .unwrap();
        // Config with harden=true so rlimits apply.
        let mut cfg = Config::default();
        cfg.security.sandbox.harden = true;
        let shared = Arc::new(std::sync::RwLock::new(cfg));
        (tmp, reg, shared)
    }

    /// A plugin tool with `resource_limits.cpu_secs = 2` is killed by
    /// SIGXCPU within ~2s when `harden` is true (WO 11.5).
    #[cfg(unix)]
    #[ignore = "requires setrlimit and a real CPU burn"]
    #[tokio::test]
    async fn plugin_tool_resource_limit_kills_cpu_burn_with_sigxcpu() {
        let (_tmp, reg, cfg) = make_cpu_burn_plugin(2);
        let tools = all_plugin_tools(&reg, cfg, None);
        assert_eq!(tools.len(), 1, "expected 1 plugin tool");

        let start = std::time::Instant::now();
        let outcome = tools[0]
            .run(&ToolContext::new(), serde_json::Value::Null)
            .await;
        let elapsed = start.elapsed();

        // The rlimit should fire well before the 30s tool timeout.
        assert!(
            elapsed < std::time::Duration::from_secs(25),
            "plugin tool ran for {elapsed:?} — rlimit did not fire (expected SIGXCPU within ~2s)"
        );

        // The outcome must be a failure: the child was signal-killed.
        match outcome {
            ToolOutcome::Failure(ToolError::Execution { exit_code, .. }) => {
                // Signal-killed processes report a negative exit code.
                assert!(
                    exit_code.map(|c| c < 0).unwrap_or(false),
                    "expected signal-killed (negative exit code), got exit_code={exit_code:?}"
                );
            }
            other => panic!("expected Failure from SIGXCPU, got {other:?}"),
        }
    }

    /// `SandboxConfig::merge_with` overlays per-plugin limits on the
    /// global default (WO 11.5).
    #[test]
    fn sandbox_merge_with_overlays_per_plugin_limits() {
        let global = SandboxConfig {
            harden: true,
            no_network: false,
            block_edits: false,
            accept_unsandboxed: false,
            cpu_limit_secs: 300,
            memory_limit_mb: 2048,
            filesize_limit_mb: 512,
        };
        let limits = ResourceLimits {
            cpu_secs: Some(60),
            memory_mb: None,
            filesize_mb: Some(128),
        };
        let merged = global.merge_with(Some(&limits));
        assert!(merged.harden, "harden flag inherited from global");
        assert_eq!(merged.cpu_limit_secs, 60, "cpu overridden");
        assert_eq!(merged.memory_limit_mb, 2048, "memory falls back to global");
        assert_eq!(merged.filesize_limit_mb, 128, "filesize overridden");
    }

    #[test]
    fn sandbox_merge_with_none_returns_clone() {
        let global = SandboxConfig {
            harden: true,
            no_network: false,
            block_edits: false,
            accept_unsandboxed: false,
            cpu_limit_secs: 300,
            memory_limit_mb: 2048,
            filesize_limit_mb: 512,
        };
        let merged = global.merge_with(None);
        assert_eq!(merged, global);
    }

    /// `resource_limits` parses from the manifest TOML.
    #[test]
    fn resource_limits_parses_from_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("demo");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "demo"
version = "0.1.0"
description = "demo"
trust = "read-only"

[resource_limits]
cpu_secs = 10
memory_mb = 256
"#,
        )
        .unwrap();
        let plugin = kf_plugin_sdk::LoadedPlugin::load(&plugin_dir).unwrap();
        let limits = plugin.manifest.resource_limits.expect("resource_limits");
        assert_eq!(limits.cpu_secs, Some(10));
        assert_eq!(limits.memory_mb, Some(256));
        assert_eq!(limits.filesize_mb, None);
    }
}

#[cfg(unix)]
mod hot_reload_tests {
    use super::*;

    /// The plugin file watcher fires a reload signal within ~2s when a
    /// `kf-code.toml` is modified (WO 11.4, ADR-059). Timing-sensitive;
    /// uses a 3s timeout and is `#[ignore]` to avoid CI flake.
    #[cfg(unix)]
    #[ignore = "timing-sensitive file-system watcher test"]
    #[tokio::test]
    async fn plugin_watcher_fires_on_manifest_change() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let plugin_dir = plugins_dir.join("demo");
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

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _watcher = crate::session::plugin_tools::spawn_plugin_watcher(plugins_dir.clone(), tx);
        assert!(_watcher.is_some(), "watcher should start");

        // Give the watcher a moment to initialize.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Modify the manifest.
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "demo"
version = "0.2.0"
description = "updated"
trust = "read-only"

[[capabilities]]
type = "skill"
trigger = "/demo"
prompt = "updated"
"#,
        )
        .unwrap();

        // Wait for the reload signal (500ms debounce + watcher latency).
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
        assert!(
            result.is_ok(),
            "plugin watcher did not fire within 3s after manifest change"
        );
    }
}
