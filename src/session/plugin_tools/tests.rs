use super::*;
use crate::shared::{Config, SharedConfig, ToolOutcome};
use crate::tools::ToolContext;
use kirkforge_plugin::{Capability, Plugin, TrustTier};
use kirkforge_plugin_host::{PluginRegistry, TrustPolicy};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn make_greet_plugin() -> (tempfile::TempDir, PluginRegistry, SharedConfig) {
    let tmp = tempfile::tempdir().unwrap();
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

#[tokio::test]
async fn wrapper_for_plugin_tool() {
    let (_tmp, reg, cfg) = make_greet_plugin();
    let tools = all_plugin_tools(&reg, cfg);
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

    let tools = all_plugin_tools(&reg, cfg);
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

    let tools = all_plugin_tools(&reg, cfg);
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
        "#!/bin/sh\nprintf '%s' \"$KIRKFORGE_SECRET_VAR\"",
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

    std::env::set_var("KIRKFORGE_SECRET_VAR", "leaked");
    let tools = all_plugin_tools(&reg, cfg);
    let outcome = tools[0]
        .run(&ToolContext::new(), serde_json::Value::Null)
        .await;
    std::env::remove_var("KIRKFORGE_SECRET_VAR");

    assert!(
        matches!(outcome, ToolOutcome::Success { ref content } if content.is_empty()),
        "unlisted env var leaked into plugin tool: {outcome:?}"
    );
}

/// Plugin tool subprocesses receive a sanitized PATH so shell wrappers can
/// resolve standard utilities even when kirkforge is launched with a minimal
/// or world-writable PATH.
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
    let tools = all_plugin_tools(&reg, cfg);
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
        source_dir.join("kirkforge.toml"),
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
        let prior = std::env::var("KIRKFORGE_DATA_DIR").ok();
        std::env::set_var("KIRKFORGE_DATA_DIR", value);
        Self { prior, _lock }
    }

    async fn set_async(value: &str) -> Self {
        let _lock = crate::session::test_data_dir_lock().lock().await;
        let prior = std::env::var("KIRKFORGE_DATA_DIR").ok();
        std::env::set_var("KIRKFORGE_DATA_DIR", value);
        Self { prior, _lock }
    }
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var("KIRKFORGE_DATA_DIR", v),
            None => std::env::remove_var("KIRKFORGE_DATA_DIR"),
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
    let source_bin = repo_root.join("npm/kirkforge-plugin/node_modules/.bin");
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
    let data_bin = tmp.path().join("npm/kirkforge-plugin/node_modules/.bin");
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
    let data_bin = tmp.path().join("npm/kirkforge-plugin/node_modules/.bin");
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
        demo.join("kirkforge.toml"),
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
/// loads every bundled plugin from that directory without warnings. This
/// catches packaging mistakes that leave tools referenced by a manifest
/// missing from the installed plugin root.
#[test]
fn bundled_plugins_load_from_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let installed_plugins = tmp.path().join("plugins");

    // Copy the in-repo bundled plugins into a temp data directory so we
    // exercise the same code path an installed release uses.
    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins");
    copy_dir_all(&repo_plugins, &installed_plugins).unwrap();

    let _guard = DataDirGuard::set(&tmp.path().to_string_lossy());
    let (registry, warnings) = load_plugin_registry(&Config::default())
        .expect("loading installed plugins should not fail");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    let names: Vec<_> = registry
        .active_plugins()
        .iter()
        .map(|p| p.plugin.manifest().name.clone())
        .collect();
    for expected in [
        "kirkforge-draw",
        "kirkforge-video",
        "stratum",
        "kirkforge-plugin3",
        "kirkforge-plugin",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "expected bundled plugin {expected:?} to load from data dir; got {names:?}"
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
    let (registry, warnings) = load_plugin_registry(&Config::default())
        .expect("loading installed plugins should not fail");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    for hosted in registry.active_plugins() {
        let root = hosted.plugin.root().to_path_buf();
        for cap in hosted.plugin.tools() {
            if let kirkforge_plugin::Capability::Tool {
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
/// built (e.g. a bare `cargo test -p kirkforge`).
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
    let (registry, warnings) = load_plugin_registry(&Config::default())
        .expect("loading installed plugins should not fail");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    let tools = all_plugin_tools(
        &registry,
        Arc::new(std::sync::RwLock::new(Config::default())),
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
/// bundled `npm/kirkforge-plugin` tree must be reachable from the plugin
/// scripts so that `plugin_tools` can list verification engines through the
/// host's `PluginToolWrapper`. Skipped when node or the built SDK is not
/// available (e.g. a bare `cargo test -p kirkforge` without `npm ci`).
#[tokio::test]
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
    let repo_sdk = repo_root.join("npm/kirkforge-plugin/apps/cli/dist/index.js");
    if which_node().is_none() || !repo_sdk.exists() {
        eprintln!("skipping Node SDK end-to-end test: node or built SDK not available");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let installed_plugins = tmp.path().join("plugins");
    let installed_npm = tmp.path().join("npm/kirkforge-plugin");
    let repo_plugins = repo_root.join("plugins");
    let repo_npm = repo_root.join("npm/kirkforge-plugin");
    copy_dir_all(&repo_plugins, &installed_plugins).unwrap();
    copy_dir_all(&repo_npm, &installed_npm).unwrap();

    let _guard = DataDirGuard::set_async(&tmp.path().to_string_lossy()).await;
    let (registry, warnings) = load_plugin_registry(&Config::default())
        .expect("loading installed plugins should not fail");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

    let tools = all_plugin_tools(
        &registry,
        Arc::new(std::sync::RwLock::new(Config::default())),
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
/// exist on disk, and can be loaded by the plugin host under the default
/// trust policy. They remain disabled unless the operator toggles them on.
#[test]
fn default_plugin_sources_are_present_and_loadable() {
    let expected = [
        "kirkforge-draw",
        "kirkforge-video",
        "stratum",
        "kirkforge-plugin3",
        "kirkforge-plugin",
    ];
    let base = Config::default();
    for name in expected {
        assert!(
            base.tools.plugin_sources.contains_key(name),
            "built-in plugin source '{name}' is missing from default config"
        );
    }

    let mut cfg = Config::default();
    cfg.tools.plugin_sources = base.tools.plugin_sources;
    cfg.tools.enabled_plugins = expected.iter().map(|s| s.to_string()).collect();

    let mut registry = PluginRegistry::new();
    let warnings = load_workspace_plugins(&mut registry, &cfg);
    // All built-in sources exist and load with the default Shell trust policy.
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    for name in expected {
        assert!(
            registry.find_active_by_name(name).is_some(),
            "built-in plugin source '{name}' did not load"
        );
    }
}
