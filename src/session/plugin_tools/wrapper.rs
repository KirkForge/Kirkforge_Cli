//! `PluginToolWrapper`: a `Tool` trait implementation that forwards calls to a
//! v1 plugin tool script.
//!
//! Plugin tool scripts are invoked asynchronously with a sandboxed working
//! directory, curated environment, timeout, and process-group cleanup.

use crate::session::bash_runner::{
    cap_to_string, drain_capped, setup_rlimits, MAX_BASH_OUTPUT_BYTES,
};
use crate::session::process_group::{kill_process_group, reap_child, setup_process_group};
use crate::shared::audit::AuditLog;
use crate::shared::{
    intern_static_str, read_shared_config, Config, SandboxConfig, SharedConfig, ToolDef, ToolError,
    ToolOutcome,
};
use crate::tools::{Tool, ToolContext};
use kf_plugin_host::KF_CODE_TOOL_ARGS;
use kf_plugin_sdk::TrustTier;
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
///   1. Installed/data-directory layout (`~/.local/share/kf-code/npm/...`).
///   2. Source layout: when the running binary is under `<repo>/target/`,
///      the workspace sibling `<repo>/npm/kf-plugin/node_modules/.bin`
///      is also included so development builds resolve `tsc`/`pyright` without
///      a global install.
pub(crate) fn npm_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(data_dir) = crate::session::data_dir() {
        let installed = data_dir.join("npm/kf-plugin/node_modules/.bin");
        if installed.is_dir() {
            dirs.push(installed);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        // Walk up from the binary looking for a workspace/source-layout Node SDK.
        // Handles both release/debug binaries at `<repo>/target/{release,debug}/kf-code`
        // and test binaries at `<repo>/target/{release,debug}/deps/kf-code-<hash>`.
        let mut current = exe.parent();
        while let Some(dir) = current {
            let candidate = dir.join("npm/kf-plugin/node_modules/.bin");
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
    /// Per-plugin sandbox config (global default merged with the
    /// manifest's `resource_limits` override, WO 11.5). Applied via
    /// `setup_rlimits` in the spawn path (Unix only).
    sandbox: SandboxConfig,
    /// Effective trust tier for the owning plugin (M11). Tools from
    /// ReadOnly plugins are blocked at dispatch time.
    trust: TrustTier,
    /// Optional audit log for recording plugin tool invocations (H4).
    audit_log: Option<std::sync::Arc<AuditLog>>,
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
        sandbox: SandboxConfig,
        trust: TrustTier,
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
            sandbox,
            trust,
            audit_log: None,
        }
    }

    /// Attach an audit log for recording plugin tool invocations (H4).
    pub fn with_audit_log(mut self, log: std::sync::Arc<AuditLog>) -> Self {
        self.audit_log = Some(log);
        self
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
                // host launches kf-code with a minimal or world-writable PATH.
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
        env.push((KF_CODE_TOOL_ARGS.to_string(), args.to_string()));
        env.push(("KF_CODE_TOOL_ARGS_JSON".to_string(), args.to_string()));
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

        // M11: enforce trust tier at dispatch time. A ReadOnly plugin may not
        // execute tool commands — its Skill prompts can only produce
        // read-only model output.
        if self.trust == TrustTier::ReadOnly {
            tracing::warn!(
                tool = %self.def.name,
                trust = %self.trust,
                "plugin tool blocked: ReadOnly trust tier does not allow execution"
            );
            return ToolOutcome::Failure(ToolError::AccessDenied {
                message: format!(
                    "plugin tool '{}' blocked: ReadOnly trust tier does not allow execution",
                    self.def.name
                ),
            });
        }

        let start = std::time::Instant::now();
        let args_summary: String = args_json.chars().take(200).collect();
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
        // WO 11.5 / H3: rlimits are always applied (the harden flag
        // controls only bash sandbox settings, not resource limits).
        setup_rlimits(&mut command, &self.sandbox);

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

        let outcome = match finish {
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
        };

        // H4: audit-log plugin tool invocations when an audit log is attached.
        if let Some(ref audit) = self.audit_log {
            let exit_code = match &outcome {
                ToolOutcome::Failure(ToolError::Execution { exit_code, .. }) => *exit_code,
                _ => None,
            };
            audit.log_plugin_tool(
                self.def.name,
                &args_summary,
                exit_code,
                start.elapsed().as_millis() as u64,
            );
        }

        outcome
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

#[cfg(test)]
mod wrapper_tests {
    use super::*;
    use crate::shared::Config;
    use crate::tools::{Tool, ToolContext};
    use std::sync::Arc;

    fn make_wrapper() -> PluginToolWrapper {
        let cfg = Arc::new(std::sync::RwLock::new(Config::default()));
        PluginToolWrapper::new(
            "test_tool".into(),
            "A test tool".into(),
            serde_json::json!({"type": "object"}),
            PathBuf::from("/tmp/test-plugin"),
            PathBuf::from("tool.sh"),
            cfg,
            SandboxConfig::default(),
            TrustTier::Shell,
        )
    }

    fn make_ctx() -> ToolContext {
        ToolContext::new()
    }

    #[tokio::test]
    async fn run_rejects_args_over_64kb() {
        let wrapper = make_wrapper();
        let ctx = make_ctx();
        // Create args > 64KB
        let big_string = "x".repeat(70_000);
        let args = serde_json::json!({"data": big_string});
        let outcome = wrapper.run(&ctx, args).await;
        match outcome {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(
                    message.contains("exceed"),
                    "message should mention size limit, got: {message}"
                );
                assert!(message.contains("65536"));
            }
            _ => panic!("expected InvalidArgs failure, got {outcome:?}"),
        }
    }

    #[tokio::test]
    async fn run_accepts_small_args() {
        let wrapper = make_wrapper();
        let ctx = make_ctx();
        let args = serde_json::json!({"x": "small"});
        let outcome = wrapper.run(&ctx, args).await;
        // It will fail to spawn (no real script), but it should NOT fail
        // with InvalidArgs — that guard is before the spawn.
        if let ToolOutcome::Failure(ToolError::InvalidArgs { .. }) = outcome {
            panic!("small args should not trigger InvalidArgs guard");
        }
    }

    /// M11: a ReadOnly trust tier blocks tool execution at dispatch time.
    #[tokio::test]
    async fn run_blocks_readonly_trust_tier() {
        let cfg = Arc::new(std::sync::RwLock::new(Config::default()));
        let wrapper = PluginToolWrapper::new(
            "blocked_tool".into(),
            "should be blocked".into(),
            serde_json::json!({"type": "object"}),
            PathBuf::from("/tmp/test-plugin"),
            PathBuf::from("tool.sh"),
            cfg,
            SandboxConfig::default(),
            TrustTier::ReadOnly,
        );
        let ctx = make_ctx();
        let outcome = wrapper.run(&ctx, serde_json::json!({"x": 1})).await;
        match outcome {
            ToolOutcome::Failure(ToolError::AccessDenied { message }) => {
                assert!(
                    message.contains("ReadOnly"),
                    "message should mention ReadOnly, got: {message}"
                );
                assert!(
                    message.contains("blocked_tool"),
                    "message should mention tool name, got: {message}"
                );
            }
            _ => panic!("expected AccessDenied failure, got {outcome:?}"),
        }
    }

    #[test]
    fn max_env_args_bytes_is_64k() {
        assert_eq!(PluginToolWrapper::MAX_ENV_ARGS_BYTES, 64 * 1024);
    }

    #[test]
    fn def_returns_stored_values() {
        let wrapper = make_wrapper();
        let def = wrapper.def();
        assert_eq!(def.name, "test_tool");
        assert_eq!(def.description, "A test tool");
    }

    #[test]
    fn sandbox_dir_uses_configured_sandbox_when_non_empty() {
        let wrapper = make_wrapper();
        let mut cfg = Config::default();
        cfg.security.sandbox_dir = Some("/configured/sandbox".into());
        assert_eq!(
            wrapper.sandbox_dir(&cfg),
            PathBuf::from("/configured/sandbox")
        );
    }

    #[test]
    fn sandbox_dir_ignores_empty_sandbox_string() {
        let wrapper = make_wrapper();
        let mut cfg = Config::default();
        cfg.security.sandbox_dir = Some(String::new());
        // Empty sandbox falls through to current_dir (env-dependent), so
        // just assert it does NOT return the empty string.
        let dir = wrapper.sandbox_dir(&cfg);
        assert_ne!(dir, PathBuf::new(), "empty sandbox should not be used");
        assert_ne!(dir.as_os_str(), "");
    }

    #[test]
    fn sandbox_dir_uses_cwd_when_sandbox_unset() {
        let wrapper = make_wrapper();
        let cfg = Config::default();
        let dir = wrapper.sandbox_dir(&cfg);
        // With no sandbox configured, should fall to current_dir (or
        // plugin_root as a last resort). Either way it must not be the
        // plugin root "/tmp/test-plugin" unless cwd happens to be that.
        assert!(dir.is_absolute() || dir == PathBuf::from("/tmp/test-plugin"));
    }

    #[test]
    fn curated_env_includes_kf_code_tool_args() {
        let wrapper = make_wrapper();
        let cfg = Config::default();
        let args = serde_json::json!({"x": 1});
        let env = wrapper.curated_env(&cfg, &args);
        let has_args = env
            .iter()
            .any(|(k, v)| k == "KF_CODE_TOOL_ARGS" && v == r#"{"x":1}"#);
        assert!(has_args, "KF_CODE_TOOL_ARGS must be set, got {env:?}");
        let has_args_json = env.iter().any(|(k, _)| k == "KF_CODE_TOOL_ARGS_JSON");
        assert!(has_args_json, "KF_CODE_TOOL_ARGS_JSON must be set");
    }

    #[test]
    fn curated_env_includes_baseline_vars_when_present() {
        let wrapper = make_wrapper();
        let cfg = Config::default();
        let env = wrapper.curated_env(&cfg, &serde_json::json!({}));
        // PATH is almost always present in the test environment.
        let has_path = env.iter().any(|(k, _)| k == "PATH");
        assert!(
            has_path,
            "PATH should be forwarded when present, got {env:?}"
        );
    }

    #[test]
    fn curated_env_includes_allowed_env_vars_from_config() {
        let wrapper = make_wrapper();
        let mut cfg = Config::default();
        // Force a var that we set in the test process to be forwarded.
        std::env::set_var("KF_CODE_TEST_ENVVAR", "forwarded");
        cfg.tools.plugin_allowed_env_vars = vec!["KF_CODE_TEST_ENVVAR".into()];
        let env = wrapper.curated_env(&cfg, &serde_json::json!({}));
        std::env::remove_var("KF_CODE_TEST_ENVVAR");
        let hit = env
            .iter()
            .any(|(k, v)| k == "KF_CODE_TEST_ENVVAR" && v == "forwarded");
        assert!(hit, "allowed env var should be forwarded, got {env:?}");
    }

    #[test]
    fn curated_env_skips_missing_allowed_vars() {
        let wrapper = make_wrapper();
        let mut cfg = Config::default();
        cfg.tools.plugin_allowed_env_vars = vec!["KF_CODE_DEFINITELY_NOT_SET_XYZ".into()];
        let env = wrapper.curated_env(&cfg, &serde_json::json!({}));
        let hit = env
            .iter()
            .any(|(k, _)| k == "KF_CODE_DEFINITELY_NOT_SET_XYZ");
        assert!(!hit, "unset allowed var should not appear, got {env:?}");
    }

    #[test]
    fn npm_bin_dirs_returns_vec_without_panic() {
        // The function walks the filesystem; just assert it returns a Vec
        // without panicking and does not contain duplicates.
        let dirs = npm_bin_dirs();
        let mut seen = std::collections::HashSet::new();
        for d in &dirs {
            assert!(seen.insert(d.clone()), "duplicate npm bin dir: {d:?}");
        }
    }

    /// bucketlist 3.39: cancelling the `ToolContext` token while a plugin
    /// tool is running takes the `Finish::Cancelled` branch — the child is
    /// killed/reaped and the outcome is `Failure(ToolError::Cancelled)`.
    #[cfg(unix)]
    #[tokio::test]
    async fn run_returns_cancelled_when_token_fires() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("sleep.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let cfg = Arc::new(std::sync::RwLock::new(Config::default()));
        let wrapper = PluginToolWrapper::new(
            "sleep_tool".into(),
            "sleeps".into(),
            serde_json::json!({"type": "object"}),
            dir.path().to_path_buf(),
            PathBuf::from("sleep.sh"),
            cfg,
            SandboxConfig::default(),
            TrustTier::Shell,
        );

        let ctx = make_ctx();
        // Cancel from a separate task after the child has spawned and
        // entered the select! wait (run() borrows ctx, so it cannot be
        // spawned itself).
        let token = ctx.token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            token.cancel();
        });

        let outcome = wrapper.run(&ctx, serde_json::json!({})).await;
        match outcome {
            ToolOutcome::Failure(ToolError::Cancelled) => {}
            other => panic!("expected Cancelled failure, got {other:?}"),
        }
    }
}
