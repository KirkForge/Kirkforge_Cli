//! `PluginToolWrapper`: a `Tool` trait implementation that forwards calls to a
//! v1 plugin tool script.
//!
//! Plugin tool scripts are invoked asynchronously with a sandboxed working
//! directory, curated environment, timeout, and process-group cleanup.

use crate::session::bash_runner::{cap_to_string, drain_capped, MAX_BASH_OUTPUT_BYTES};
use crate::session::process_group::{kill_process_group, reap_child, setup_process_group};
use crate::shared::{
    intern_static_str, read_shared_config, Config, SharedConfig, ToolDef, ToolError, ToolOutcome,
};
use crate::tools::{Tool, ToolContext};
use kirkforge_plugin_host::KIRKFORGE_TOOL_ARGS;
use std::path::{Path, PathBuf};
use std::process::Stdio;
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

/// Return the Node SDK `node_modules/.bin` directories that should be
/// prepended to the plugin tool PATH.
///
/// Two layouts are supported:
///   1. Installed/data-directory layout (`~/.local/share/kirkforge/npm/...`).
///   2. Source layout: when the running binary is under `<repo>/target/`,
///      the workspace sibling `<repo>/npm/kirkforge-plugin/node_modules/.bin`
///      is also included so development builds resolve `tsc`/`pyright` without
///      a global install.
pub(crate) fn npm_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(data_dir) = crate::session::data_dir() {
        let installed = data_dir.join("npm/kirkforge-plugin/node_modules/.bin");
        if installed.is_dir() {
            dirs.push(installed);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        // Walk up from the binary looking for a workspace/source-layout Node SDK.
        // Handles both release/debug binaries at `<repo>/target/{release,debug}/kirkforge`
        // and test binaries at `<repo>/target/{release,debug}/deps/kirkforge-<hash>`.
        let mut current = exe.parent();
        while let Some(dir) = current {
            let candidate = dir.join("npm/kirkforge-plugin/node_modules/.bin");
            if candidate.is_dir() && !dirs.contains(&candidate) {
                dirs.push(candidate);
                break;
            }
            current = dir.parent();
        }
    }

    dirs
}

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
        // ToolDef requires 'static strings; intern so /reload plugins (which
        // rebuilds every wrapper) does not leak a fresh allocation each time.
        let name: &'static str = intern_static_str(&name);
        let desc: &'static str = intern_static_str(&description);
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
        cfg.security
            .sandbox_dir
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
    /// from inheriting sensitive or irrelevant session state. PATH is passed
    /// through the same sanitizer as the model's bash tool so plugin shell
    /// wrappers can reliably resolve `bash`, `node`, `jq`, `python3`, etc.
    fn curated_env(&self, cfg: &Config, args: &serde_json::Value) -> Vec<(String, String)> {
        let mut env = Vec::new();
        for key in BASELINE_ENV_VARS {
            if let Ok(v) = std::env::var(key) {
                // PATH gets sanitized so plugin wrappers don't fail when the
                // host launches kirkforge with a minimal or world-writable PATH.
                // Prepend any bundled Node SDK `node_modules/.bin` directories
                // (data-directory install or source-layout sibling) so Node SDK
                // tools like tsc and pyright resolve without a global install.
                let value = if *key == "PATH" {
                    let sanitized = crate::session::bash_runner::sanitized_path(&v);
                    let npm_bins = npm_bin_dirs();
                    if npm_bins.is_empty() {
                        sanitized
                    } else {
                        let mut path = npm_bins
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(":");
                        path.push(':');
                        path.push_str(&sanitized);
                        path
                    }
                } else {
                    v
                };
                env.push(((*key).to_string(), value));
            }
        }
        for key in &cfg.tools.plugin_allowed_env_vars {
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
        let timeout_secs = cfg.tools.tool_timeout_secs.unwrap_or(30).clamp(1, 3600);
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
