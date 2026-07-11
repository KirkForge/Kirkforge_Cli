//! Tool wrappers for KirkForge plugins.
//!
//! Plugin tools are loaded from `~/.local/share/kirkforge/plugins` via the
//! `PluginRegistry`. Each plugin tool is wrapped to implement the executor's
//! `Tool` trait. Plugin tool scripts are invoked asynchronously with a
//! sandboxed working directory, curated environment, timeout, and process-group
//! cleanup.

use crate::session::bash_runner::{cap_to_string, drain_capped, MAX_BASH_OUTPUT_BYTES};
use crate::session::process_group::{kill_process_group, reap_child, setup_process_group};
use crate::shared::{read_shared_config, Config, SharedConfig, ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use kirkforge_plugin::{Capability, Plugin};
use kirkforge_plugin_host::{PluginRegistry, TrustPolicy, KIRKFORGE_TOOL_ARGS};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

/// Default environment variables forwarded into a plugin tool subprocess.
/// We keep the surface small: PATH plus basic user/locale/temp variables.
const BASELINE_ENV_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "TMPDIR",
    "TEMP",
    "TMP",
    "XDG_RUNTIME_DIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
];

/// A `Tool` trait implementation that forwards calls to a v1 plugin tool script.
pub struct PluginToolWrapper {
    def: ToolDef,
    plugin_root: PathBuf,
    command: PathBuf,
    shared_config: SharedConfig,
}

impl PluginToolWrapper {
    /// Create a new wrapper for a single plugin tool.
    pub fn new(
        name: String,
        description: String,
        schema: serde_json::Value,
        plugin_root: PathBuf,
        command: PathBuf,
        shared_config: SharedConfig,
    ) -> Self {
        // ToolDef requires 'static strings; leak session-lifetime metadata.
        let name: &'static str = Box::leak(name.into_boxed_str());
        let desc: &'static str = Box::leak(description.into_boxed_str());
        Self {
            def: ToolDef {
                name,
                description: desc,
                parameters: schema,
            },
            plugin_root,
            command,
            shared_config,
        }
    }

    /// Resolve the working directory for the plugin tool subprocess.
    ///
    /// If the operator configured a non-empty `sandbox_dir`, the tool runs
    /// there. An empty or missing `sandbox_dir` resolves to the current
    /// working directory so plugin tools operate on the user's project,
    /// not the plugin installation directory. Only if cwd cannot be
    /// determined do we fall back to the plugin root as a last resort.
    fn sandbox_dir(&self, cfg: &Config) -> PathBuf {
        cfg.sandbox_dir
            .as_ref()
            .and_then(|s| {
                let p = Path::new(s);
                if p.as_os_str().is_empty() {
                    None
                } else {
                    Some(p.to_path_buf())
                }
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| self.plugin_root.clone())
    }

    /// Build the curated environment for the plugin tool subprocess.
    ///
    /// Only the baseline allowlist and any explicitly-configured
    /// `plugin_allowed_env_vars` are forwarded. This prevents a plugin tool
    /// from inheriting sensitive or irrelevant session state.
    fn curated_env(&self, cfg: &Config, args: &serde_json::Value) -> Vec<(String, String)> {
        let mut env = Vec::new();
        for key in BASELINE_ENV_VARS {
            if let Ok(v) = std::env::var(key) {
                env.push(((*key).to_string(), v));
            }
        }
        for key in &cfg.plugin_allowed_env_vars {
            if let Ok(v) = std::env::var(key) {
                env.push((key.clone(), v));
            }
        }
        env.push((KIRKFORGE_TOOL_ARGS.to_string(), args.to_string()));
        env.push(("KIRKFORGE_TOOL_ARGS_JSON".to_string(), args.to_string()));
        env
    }

    /// Maximum serialized argument size passed via environment variable.
    /// Most platforms cap the total environment block (Linux ~128 KiB,
    /// macOS smaller), so fail early instead of getting a cryptic `E2BIG`.
    const MAX_ENV_ARGS_BYTES: usize = 64 * 1024;
}

#[async_trait::async_trait]
impl Tool for PluginToolWrapper {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let args_json = args.to_string();
        if args_json.len() > Self::MAX_ENV_ARGS_BYTES {
            return ToolOutcome::Failure(ToolError::InvalidArgs {
                message: format!(
                    "plugin tool arguments exceed {} bytes ({} bytes); pass smaller payloads",
                    Self::MAX_ENV_ARGS_BYTES,
                    args_json.len()
                ),
            });
        }

        let cfg = read_shared_config(&self.shared_config).clone();
        let cmd_path = self.plugin_root.join(&self.command);
        let cwd = self.sandbox_dir(&cfg);
        let timeout_secs = cfg.tool_timeout_secs.unwrap_or(30).clamp(1, 3600);
        let timeout_at = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

        let mut command = tokio::process::Command::new(&cmd_path);
        command
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        setup_process_group(&mut command);

        for (k, v) in self.curated_env(&cfg, &args) {
            command.env(k, v);
        }

        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Execution {
                    message: format!("failed to spawn plugin tool '{}': {e}", self.def.name),
                    exit_code: None,
                    stderr: String::new(),
                });
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: "plugin tool stdout not available".into(),
                });
            }
        };
        let stderr = match child.stderr.take() {
            Some(s) => s,
            None => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: "plugin tool stderr not available".into(),
                });
            }
        };

        let drain_stdout = tokio::spawn(drain_capped(stdout, MAX_BASH_OUTPUT_BYTES));
        let drain_stderr = tokio::spawn(drain_capped(stderr, MAX_BASH_OUTPUT_BYTES));

        enum Finish {
            Status(std::io::Result<std::process::ExitStatus>),
            Timeout,
            Cancelled,
        }

        let finish = tokio::select! {
            biased;
            status = child.wait() => Finish::Status(status),
            _ = tokio::time::sleep_until(timeout_at) => Finish::Timeout,
            _ = ctx.token.cancelled() => Finish::Cancelled,
        };

        match finish {
            Finish::Status(Ok(status)) => {
                let (raw_stdout, stdout_dropped) =
                    match join_plugin_drain(drain_stdout, "stdout").await {
                        Ok(r) => r,
                        Err(e) => {
                            return ToolOutcome::Failure(ToolError::Internal {
                                message: format!("plugin tool stdout drain failed: {e}"),
                            });
                        }
                    };
                let (raw_stderr, stderr_dropped) =
                    match join_plugin_drain(drain_stderr, "stderr").await {
                        Ok(r) => r,
                        Err(e) => {
                            return ToolOutcome::Failure(ToolError::Internal {
                                message: format!("plugin tool stderr drain failed: {e}"),
                            });
                        }
                    };
                let stdout_text = cap_to_string(raw_stdout, stdout_dropped);
                let stderr_text = cap_to_string(raw_stderr, stderr_dropped);

                if status.success() {
                    ToolOutcome::Success {
                        content: stdout_text,
                    }
                } else {
                    ToolOutcome::Failure(ToolError::Execution {
                        message: format!("plugin tool '{}' exited unsuccessfully", self.def.name),
                        exit_code: status.code(),
                        stderr: stderr_text,
                    })
                }
            }
            Finish::Status(Err(e)) => ToolOutcome::Failure(ToolError::Execution {
                message: format!("failed to wait for plugin tool '{}': {e}", self.def.name),
                exit_code: None,
                stderr: String::new(),
            }),
            Finish::Timeout => {
                kill_process_group(&mut child);
                // Drains are best-effort after a kill; the timeout outcome is
                // already determined, so ignore any drain errors.
                #[allow(unused_must_use)]
                {
                    join_plugin_drain(drain_stdout, "stdout").await;
                    join_plugin_drain(drain_stderr, "stderr").await;
                }
                reap_child(&mut child, Duration::from_secs(2)).await;
                ToolOutcome::Failure(ToolError::Timeout {
                    after_secs: timeout_secs,
                })
            }
            Finish::Cancelled => {
                kill_process_group(&mut child);
                // Drains are best-effort after a kill; the cancelled outcome is
                // already determined, so ignore any drain errors.
                #[allow(unused_must_use)]
                {
                    join_plugin_drain(drain_stdout, "stdout").await;
                    join_plugin_drain(drain_stderr, "stderr").await;
                }
                reap_child(&mut child, Duration::from_secs(2)).await;
                ToolOutcome::Failure(ToolError::Cancelled)
            }
        }
    }
}

async fn join_plugin_drain(
    handle: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, u64)>>,
    label: &str,
) -> std::io::Result<(Vec<u8>, u64)> {
    match tokio::time::timeout(Duration::from_secs(5), handle).await {
        Ok(Ok(Ok(pair))) => Ok(pair),
        Ok(Ok(Err(e))) => Err(std::io::Error::other(format!("drain {label}: {e}"))),
        Ok(Err(e)) => Err(std::io::Error::other(format!(
            "drain {label} task panicked: {e}"
        ))),
        Err(_) => Err(std::io::Error::other(format!(
            "drain {label} did not finish within 5s"
        ))),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use kirkforge_plugin::TrustTier;

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
            cfg.sandbox_dir = Some(sandbox.to_string_lossy().to_string());
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
            cfg.sandbox_dir = Some(String::new());
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

        let cfg = Config {
            plugin_sources: {
                let mut m = std::collections::HashMap::new();
                m.insert("workspace-demo".to_string(), source_dir.clone());
                m
            },
            enabled_plugins: vec!["workspace-demo".to_string()],
            ..Config::default()
        };

        let mut registry = PluginRegistry::new();
        let warnings = load_workspace_plugins(&mut registry, &cfg);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(registry.find_active_by_name("workspace-demo").is_some());
    }

    #[test]
    fn load_workspace_plugins_warns_for_missing_source() {
        let cfg = Config {
            plugin_sources: {
                let mut m = std::collections::HashMap::new();
                m.insert("missing".to_string(), PathBuf::from("/does/not/exist"));
                m
            },
            enabled_plugins: vec!["missing".to_string()],
            ..Config::default()
        };

        let mut registry = PluginRegistry::new();
        let warnings = load_workspace_plugins(&mut registry, &cfg);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("does not exist"));
    }

    struct DataDirGuard {
        prior: Option<String>,
    }

    impl DataDirGuard {
        fn set(value: &str) -> Self {
            let prior = std::env::var("KIRKFORGE_DATA_DIR").ok();
            std::env::set_var("KIRKFORGE_DATA_DIR", value);
            Self { prior }
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
        let cfg = Config {
            plugin_sources: [("demo".to_string(), PathBuf::from("/nonexistent/demo"))]
                .into_iter()
                .collect(),
            enabled_plugins: vec!["demo".to_string()],
            ..Config::default()
        };

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

        let _data_guard = DataDirGuard::set(&tmp.path().to_string_lossy());
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

        let _guard = DataDirGuard::set(&tmp.path().to_string_lossy());
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
                base.plugin_sources.contains_key(name),
                "built-in plugin source '{name}' is missing from default config"
            );
        }

        let cfg = Config {
            plugin_sources: base.plugin_sources,
            enabled_plugins: expected.iter().map(|s| s.to_string()).collect(),
            ..Config::default()
        };

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
}
