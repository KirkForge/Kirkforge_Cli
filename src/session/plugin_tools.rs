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
    /// there. Otherwise it falls back to the plugin root (legacy v1 behaviour).
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
        env
    }
}

#[async_trait::async_trait]
impl Tool for PluginToolWrapper {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let cfg = read_shared_config(&self.shared_config).clone();
        let cmd_path = self.plugin_root.join(&self.command);
        let cwd = self.sandbox_dir(&cfg);
        let timeout_secs = cfg.tool_timeout_secs.unwrap_or(30).clamp(1, 3600);
        let timeout_at = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

        let mut command = tokio::process::Command::new(&cmd_path);
        command
            .current_dir(&cwd)
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

/// Load the plugin registry from the configured plugins directory.
///
/// Returns the registry together with any load warnings (e.g. rejected or
/// signature-invalid plugins).
pub fn load_plugin_registry(cfg: &Config) -> anyhow::Result<(PluginRegistry, Vec<String>)> {
    let dir = plugins_dir();
    let mut registry = PluginRegistry::new();
    let warnings = registry
        .load_from_dir(&dir, trust_policy_from_config(cfg))
        .unwrap_or_default();
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
}
