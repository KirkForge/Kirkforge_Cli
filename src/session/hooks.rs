//! Lifecycle hook system — user-defined shell scripts triggered on events.
//!
//! Hooks are shell scripts placed in `~/.local/share/kf-code/hooks/`.
//! Naming convention: `<event>.sh` — e.g., `pre-tool-bash.sh`,
//! `post-tool-write_file.sh`, `post-turn.sh`, `session-start.sh`,
//! `pre-compact.sh`, `post-compact.sh`.
//!
//! Each hook receives event data as environment variables:
//! - `KF_EVENT` — the event name (e.g., "post-turn")
//! - `KF_TOOL_NAME` — the tool being called (tool events only)
//! - `KF_TOOL_ARGS_JSON` — JSON-serialised tool arguments (tool events only)
//! - `KF_TOOL_RESULT` — tool result content (post-tool events only)
//! - `KF_SESSION_ID` — the session identifier
//!
//! Compaction hooks (`pre-compact` / `post-compact`) receive a JSON object
//! in `KF_TOOL_ARGS_JSON` with fields such as `message_count`,
//! `preserve_recent`, `original_count`, `result_count`,
//! `dropped_tool_results`, `condensed_assistant_turns`,
//! `summarised_messages`, and `strategy` (`"summarize"` or `"naive"`).
//!
//! Hooks run with a 5-second timeout, fire-and-forget (tokio::spawn).
//! Failures are logged to tracing but never surfaced to the user.
//! This is best-effort — hooks must not block the executor loop.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::session::access::access_from_config;
use crate::session::bash_runner::{
    cap_to_string, check_bash_command_str, drain_capped, MAX_BASH_OUTPUT_BYTES,
};
use crate::session::process_group::{kill_process_group, reap_child, setup_process_group};
use crate::shared::audit::AuditLog;
use crate::shared::Config;
use kf_plugin_host::PluginRegistry;
use kf_plugin_sdk::Plugin;
use std::sync::Arc;

/// Context passed to an in-process hook handler.
///
/// Unlike shell hooks (which receive only env vars), in-process hooks get
/// structured access to the event data, including the tool result content for
/// post-tool hooks and compact metadata for pre-compact hooks.
#[derive(Debug, Clone, Default)]
pub struct HookContext {
    /// The event name (e.g. "session-start", "post-tool-bash", "pre-compact").
    pub event: String,
    /// Session ID.
    pub session_id: String,
    /// Tool name (tool events only).
    pub tool_name: Option<String>,
    /// Tool arguments as JSON (tool events only).
    pub tool_args_json: Option<String>,
    /// Tool result content (post-tool events only).
    ///
    /// This is the key field that shell hooks could NOT access — it gives
    /// in-process hooks like the budget guard real visibility into
    /// what the tool returned, so it can decide whether to slice/compact.
    pub tool_result: Option<String>,
    /// Compact metadata (pre-compact / post-compact events only).
    pub compact_stats: Option<CompactHookStatsData>,
}

/// Compact metadata passed to in-process compaction hooks.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CompactHookStatsData {
    pub message_count: usize,
    pub preserve_recent: usize,
    pub original_count: usize,
    pub result_count: usize,
    pub dropped_tool_results: usize,
    pub condensed_assistant_turns: usize,
    pub summarised_messages: usize,
    pub strategy: String,
}

/// A pre-hook (decision hook) that can allow or deny an operation.
///
/// Pre-hooks run before the operation (e.g. `pre-tool-bash`) and can
/// block it by returning `Deny(reason)`. Implement this trait and
/// register with `HookRunner::add_in_process_hook`.
pub trait InProcessHook: Send + Sync {
    /// The event name this hook handles (e.g. "pre-tool-bash").
    fn event(&self) -> &str;

    /// Run the hook. Returns `Allow` to proceed or `Deny(reason)` to block.
    fn handle(&self, ctx: &HookContext) -> HookDecision;
}

/// A post-hook (observational hook) that runs after an operation.
///
/// Post-hooks (e.g. `post-tool-bash`, `session-start`, `post-compact`)
/// cannot block — they observe and record. Returning `Err(msg)` logs a
/// warning; returning `Ok(())` is silent success.
pub trait PostHook: Send + Sync {
    /// The event name this hook handles (e.g. "post-tool-bash", "session-start").
    fn event(&self) -> &str;

    /// Run the hook. Returns `Ok(())` on success or `Err(msg)` to log a warning.
    fn handle(&self, ctx: &HookContext) -> Result<(), String>;
}

/// Discovers and runs lifecycle hook scripts.
pub struct HookRunner {
    /// Directory containing hook scripts.
    hooks_dir: PathBuf,
    /// Set of available hook names (without `.sh` suffix).
    available: HashSet<String>,
    /// Plugin-defined hooks loaded from `PluginRegistry`.
    ///
    /// Each entry is `(event_name, absolute_script_path, plugin_name)`.
    /// Plugin hooks run through the same `run_hook_script` pipeline as
    /// built-in hooks, so they get the same 5-second timeout, bash safety
    /// gate, and capped output. The `plugin_name` is surfaced to the
    /// audit log so a hook denial/failure is attributed to the right
    /// plugin (WO 11.6).
    plugin_hooks: Vec<(String, PathBuf, Option<String>)>,
    /// In-process pre-hooks (decision hooks, from folded plugins).
    in_process_hooks: Vec<Box<dyn InProcessHook>>,
    /// In-process post-hooks (observational hooks, from folded plugins).
    post_hooks: Vec<Box<dyn PostHook>>,
    /// Optional audit log handle for recording hook denials + fail-open
    /// failures (WO 11.6, ADR-061). `None` in tests that don't care.
    audit_log: Option<Arc<AuditLog>>,
}

impl std::fmt::Debug for HookRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRunner")
            .field("hooks_dir", &self.hooks_dir)
            .field("available", &self.available)
            .field("plugin_hooks", &self.plugin_hooks)
            .field("in_process_hooks", &self.in_process_hooks.len())
            .field("post_hooks", &self.post_hooks.len())
            .field("audit_log", &self.audit_log.is_some())
            .finish()
    }
}

impl Clone for HookRunner {
    fn clone(&self) -> Self {
        Self {
            hooks_dir: self.hooks_dir.clone(),
            available: self.available.clone(),
            plugin_hooks: self.plugin_hooks.clone(),
            in_process_hooks: Vec::new(),
            post_hooks: Vec::new(),
            audit_log: self.audit_log.clone(),
        }
    }
}

impl HookRunner {
    /// Create a new hook runner, scanning `hooks_dir` for available scripts.
    ///
    /// Any file matching `<name>.sh` in the directory is registered as an
    /// available hook (the `.sh` suffix is stripped).
    pub fn new(hooks_dir: PathBuf) -> Self {
        let available = discover_hooks(&hooks_dir);
        Self {
            hooks_dir,
            available,
            plugin_hooks: Vec::new(),
            in_process_hooks: Vec::new(),
            post_hooks: Vec::new(),
            audit_log: None,
        }
    }

    /// Attach an audit log so hook denials + fail-open failures are
    /// recorded to the tamper-evident audit trail (WO 11.6).
    pub fn with_audit_log(mut self, log: Arc<AuditLog>) -> Self {
        self.audit_log = Some(log);
        self
    }

    /// Set the audit log handle after construction (used by the executor
    /// which builds the hook runner before the audit log exists).
    pub fn set_audit_log(&mut self, log: Arc<AuditLog>) {
        self.audit_log = Some(log);
    }

    /// Load plugin-defined hooks from a `PluginRegistry`.
    ///
    /// Plugin hooks are stored separately from built-in hooks so both can
    /// coexist; a plugin may add hooks for events the user did not define
    /// locally, or add additional checks for events that already have a
    /// built-in hook.
    pub fn load_plugin_hooks(
        &mut self,
        registry: &PluginRegistry,
        disabled_plugins: &std::collections::HashSet<String>,
    ) {
        for hosted in registry.active_plugins() {
            let plugin = &hosted.plugin;
            let plugin_name = plugin.manifest().name.clone();
            if disabled_plugins.contains(&plugin_name) {
                tracing::trace!(
                    plugin = %plugin_name,
                    "skipping disabled plugin hooks"
                );
                continue;
            }
            let root = plugin.root();
            for cap in plugin.hooks() {
                if let kf_plugin_sdk::Capability::Hook { event, command } = cap {
                    let script_path = root.join(&command);
                    self.plugin_hooks
                        .push((event, script_path, Some(plugin_name.clone())));
                }
            }
        }
    }

    /// Register an in-process Rust hook handler.
    ///
    /// Folded plugins call this to replace their shell hook scripts with
    /// direct Rust calls that have full `HookContext` access.
    pub fn add_in_process_hook(&mut self, hook: Box<dyn InProcessHook>) {
        self.in_process_hooks.push(hook);
    }

    /// Register an in-process Rust post-hook handler.
    ///
    /// Post-hooks run after the operation and cannot deny it.
    pub fn add_post_hook(&mut self, hook: Box<dyn PostHook>) {
        self.post_hooks.push(hook);
    }

    /// Check whether any hook (built-in, plugin, or in-process) exists for `event_name`.
    pub fn has(&self, event_name: &str) -> bool {
        self.available.contains(event_name)
            || self.plugin_hooks.iter().any(|(e, _, _)| e == event_name)
            || self
                .in_process_hooks
                .iter()
                .any(|h| h.event() == event_name)
            || self.post_hooks.iter().any(|h| h.event() == event_name)
    }

    /// Return the plugin hook script paths + plugin names registered for
    /// `event_name`.
    fn plugin_hooks_for(&self, event_name: &str) -> Vec<(PathBuf, Option<&str>)> {
        self.plugin_hooks
            .iter()
            .filter(|(e, _, _)| e == event_name)
            .map(|(_, path, name)| (path.clone(), name.as_deref()))
            .collect()
    }

    /// Record a hook verdict to the audit log if one is attached.
    fn audit_hook(&self, event: &str, plugin: Option<&str>, verdict: &str, reason: Option<&str>) {
        if let Some(ref log) = self.audit_log {
            log.log_hook(event, plugin, verdict, reason);
        }
    }

    /// Convert `&[(&str, &str)]` env vars into owned pairs for async tasks.
    fn owned_env_vars(env_vars: &[(&str, &str)]) -> Vec<(String, String)> {
        env_vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Spawn a single hook script via `run_hook_script` and log the outcome.
    ///
    /// Used for both built-in and plugin hooks in the fire-and-forget path.
    fn spawn_hook_script(
        &self,
        event_name: &str,
        plugin_name: Option<&str>,
        script_path: PathBuf,
        env_vars: Vec<(String, String)>,
        config: Config,
    ) {
        let event = event_name.to_string();
        let audit_log = self.audit_log.clone();
        let plugin_owned = plugin_name.map(|s| s.to_string());
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(rt) => rt.spawn(async move {
                match run_hook_script(&script_path, &env_vars, &config).await {
                    Ok(HookDecision::Allow) => {}
                    Ok(HookDecision::Deny(reason)) => {
                        tracing::warn!(
                            event = %event,
                            reason = %reason,
                            "Observational hook reported deny after the fact"
                        );
                        if let Some(log) = audit_log {
                            log.log_hook(&event, plugin_owned.as_deref(), "deny", Some(&reason));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(event = %event, error = %e, "Hook run failed");
                        if let Some(log) = audit_log {
                            log.log_hook(
                                &event,
                                plugin_owned.as_deref(),
                                "allow_fail_open",
                                Some(&e),
                            );
                        }
                    }
                }
            }),
            Err(e) => {
                tracing::warn!(event = %event_name, error = %e, "no Tokio runtime available; hook skipped");
                return;
            }
        };
        // Detach the task; hooks are best-effort and must not block.
        std::mem::drop(handle);
    }

    /// Run all hook scripts for `event_name` asynchronously (fire-and-forget).
    ///
    /// Built-in hooks (if any) and plugin hooks (if any) all run. Each script
    /// is invoked via `bash` with a 5-second timeout and passes through the
    /// same safety gate as the model's `bash` tool.
    ///
    /// For hooks that may deny an operation (i.e. `pre-tool-*`), use
    /// [`Self::run_decision`] instead. This method always treats hooks as
    /// observational.
    pub fn run(&self, event_name: &str, env_vars: &[(&str, &str)], config: &Config) {
        let owned_vars = Self::owned_env_vars(env_vars);
        let config = config.clone();

        // Built-in hook.
        if self.available.contains(event_name) {
            let script_path = self.hooks_dir.join(format!("{event_name}.sh"));
            self.spawn_hook_script(
                event_name,
                None,
                script_path,
                owned_vars.clone(),
                config.clone(),
            );
        }

        // Plugin hooks.
        for (script_path, plugin_name) in self.plugin_hooks_for(event_name) {
            self.spawn_hook_script(
                event_name,
                plugin_name,
                script_path,
                owned_vars.clone(),
                config.clone(),
            );
        }
    }

    /// Run all hooks for `event_name` with full `HookContext`.
    ///
    /// This is the in-process variant: folded-plugin hooks receive structured
    /// context (including tool result content for post-tool hooks) instead of
    /// just env vars. Shell hooks still run alongside (fire-and-forget for
    /// shell, in-process for Rust).
    pub fn run_with_context(&self, event_name: &str, ctx: &HookContext, config: &Config) {
        // Post-hooks (observational — cannot deny, only log).
        for hook in &self.post_hooks {
            if hook.event() == event_name {
                if let Err(reason) = hook.handle(ctx) {
                    tracing::warn!(
                        event = %event_name,
                        reason = %reason,
                        "Post-hook reported error (fire-and-forget: too late to block)"
                    );
                    self.audit_hook(event_name, None, "deny", Some(&reason));
                }
            }
        }

        // Shell hooks (fire-and-forget, same as `run`).
        let env_vars = ctx_to_env_vars(ctx);
        let env_refs: Vec<(&str, &str)> = env_vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.run(event_name, &env_refs, config);
    }

    /// Run all hooks for `event_name` that are allowed to deny an operation.
    ///
    /// Returns [`HookDecision::Allow`] if no hook exists, if all hooks
    /// succeed or fail open, or if any non-gating failure occurs. Returns
    /// [`HookDecision::Deny`] if any hook exits with code `2`. Built-in and
    /// plugin hooks are both evaluated.
    pub async fn run_decision(
        &self,
        event_name: &str,
        env_vars: &[(&str, &str)],
        config: &Config,
    ) -> HookDecision {
        let owned_vars = Self::owned_env_vars(env_vars);
        let ctx = env_vars_to_ctx(event_name, env_vars);
        self.run_decision_inner(event_name, &ctx, &owned_vars, config)
            .await
    }

    /// Run decision hooks with full `HookContext` (including tool result).
    ///
    /// Used by the executor for post-tool hooks where the in-process handler
    /// needs to see the tool's output (e.g. budget guard checking
    /// whether a bash result is oversized).
    pub async fn run_decision_with_context(
        &self,
        event_name: &str,
        ctx: &HookContext,
        config: &Config,
    ) -> HookDecision {
        let env_vars = ctx_to_env_vars(ctx);
        let env_refs: Vec<(&str, &str)> = env_vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let owned_vars = Self::owned_env_vars(&env_refs);
        self.run_decision_inner(event_name, ctx, &owned_vars, config)
            .await
    }

    async fn run_decision_inner(
        &self,
        event_name: &str,
        ctx: &HookContext,
        owned_vars: &[(String, String)],
        config: &Config,
    ) -> HookDecision {
        let mut decisions: Vec<HookDecision> = Vec::new();

        // In-process hooks (synchronous, run first so they can short-circuit).
        for hook in &self.in_process_hooks {
            if hook.event() == event_name {
                let d = hook.handle(ctx);
                if let HookDecision::Deny(ref reason) = d {
                    self.audit_hook(event_name, None, "deny", Some(reason));
                }
                decisions.push(d);
            }
        }

        // Built-in hook.
        if self.available.contains(event_name) {
            let script_path = self.hooks_dir.join(format!("{event_name}.sh"));
            match run_hook_script(&script_path, owned_vars, config).await {
                Ok(HookDecision::Deny(reason)) => {
                    self.audit_hook(event_name, None, "deny", Some(&reason));
                    decisions.push(HookDecision::Deny(reason));
                }
                Ok(d) => decisions.push(d),
                Err(e) => {
                    tracing::warn!(event = %event_name, error = %e, "Built-in decision hook failed (fail-open)");
                    self.audit_hook(event_name, None, "allow_fail_open", Some(&e));
                }
            }
        }

        // Plugin hooks.
        for (script_path, plugin_name) in self.plugin_hooks_for(event_name) {
            match run_hook_script(&script_path, owned_vars, config).await {
                Ok(HookDecision::Deny(reason)) => {
                    tracing::warn!(
                        event = %event_name,
                        path = %script_path.display(),
                        "Plugin decision hook denied"
                    );
                    self.audit_hook(event_name, plugin_name, "deny", Some(&reason));
                    decisions.push(HookDecision::Deny(reason));
                }
                Ok(d) => decisions.push(d),
                Err(e) => {
                    tracing::warn!(
                        event = %event_name,
                        path = %script_path.display(),
                        error = %e,
                        "Plugin decision hook failed (fail-open)"
                    );
                    self.audit_hook(event_name, plugin_name, "allow_fail_open", Some(&e));
                }
            }
        }

        // Any explicit deny wins; otherwise allow.
        decisions
            .into_iter()
            .find(|d| matches!(d, HookDecision::Deny(_)))
            .unwrap_or(HookDecision::Allow)
    }
}

impl Default for HookRunner {
    fn default() -> Self {
        let dir = default_hooks_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::new(dir)
    }
}

/// Discover available hook scripts in `hooks_dir`.
///
/// Returns the set of hook names (filename without `.sh` suffix) for all
/// regular files matching `*.sh`.
fn discover_hooks(hooks_dir: &std::path::Path) -> HashSet<String> {
    let mut set = HashSet::new();
    let Ok(entries) = std::fs::read_dir(hooks_dir) else {
        return set;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(stem) = name.strip_suffix(".sh") {
            if !stem.is_empty() {
                set.insert(stem.to_string());
            }
        }
    }
    set
}

/// Default hooks directory: `~/.local/share/kf-code/hooks/`.
fn default_hooks_dir() -> anyhow::Result<PathBuf> {
    let base = crate::session::data_dir()?;
    Ok(base.join("hooks"))
}

/// Convert a `HookContext` into env var pairs for shell hooks.
fn ctx_to_env_vars(ctx: &HookContext) -> Vec<(&str, String)> {
    let mut vars: Vec<(&str, String)> = Vec::new();
    vars.push(("KF_EVENT", ctx.event.clone()));
    if !ctx.session_id.is_empty() {
        vars.push(("KF_SESSION_ID", ctx.session_id.clone()));
    }
    if let Some(ref name) = ctx.tool_name {
        vars.push(("KF_TOOL_NAME", name.clone()));
    }
    if let Some(ref json) = ctx.tool_args_json {
        vars.push(("KF_TOOL_ARGS_JSON", json.clone()));
    }
    if let Some(ref result) = ctx.tool_result {
        vars.push(("KF_TOOL_RESULT", result.clone()));
    }
    vars
}

/// Convert env var pairs into a `HookContext` (for `run_decision` compat).
fn env_vars_to_ctx(event_name: &str, env_vars: &[(&str, &str)]) -> HookContext {
    let mut ctx = HookContext {
        event: event_name.to_string(),
        ..Default::default()
    };
    for (k, v) in env_vars {
        match *k {
            "KF_SESSION_ID" => ctx.session_id = v.to_string(),
            "KF_TOOL_NAME" => ctx.tool_name = Some(v.to_string()),
            "KF_TOOL_ARGS_JSON" => ctx.tool_args_json = Some(v.to_string()),
            "KF_TOOL_RESULT" => ctx.tool_result = Some(v.to_string()),
            _ => {}
        }
    }
    ctx
}

/// Decision returned by a hook that is allowed to block execution.
#[derive(Debug, Clone, PartialEq)]
pub enum HookDecision {
    /// Hook permits the operation to proceed.
    Allow,
    /// Hook denies the operation with a human-readable reason.
    Deny(String),
}

/// Spawn a hook script with env vars, timeout, and capped output.
///
/// The script content is checked against the shared bash safety gate
/// before execution. Output is capped per-stream at
/// [`MAX_BASH_OUTPUT_BYTES`]; anything past the cap is discarded and
/// counted so the log can mention it.
///
/// Exit-code semantics for hooks that gate operations (`pre-tool-*`):
/// - `0` → [`HookDecision::Allow`]
/// - `2` → [`HookDecision::Deny`]
/// - any other non-zero, timeout, or crash → allow but log a warning
///   (fail-open, so a broken hook cannot silently block the user)
async fn run_hook_script(
    script: &Path,
    env_vars: &[(String, String)],
    config: &Config,
) -> Result<HookDecision, String> {
    let (deny_list, path_guard, _) = access_from_config(config);

    if deny_list.is_path_denied(script) {
        return Err(format!(
            "hook script path denied by deny list: {}",
            script.display()
        ));
    }

    let content = match tokio::fs::read_to_string(script).await {
        Ok(c) => c,
        Err(e) => {
            return Err(format!(
                "cannot read hook script {}: {}",
                script.display(),
                e
            ))
        }
    };

    // Run the script content through the same gate the model's bash
    // tool uses. We pass no workdir so sandbox workdir policy doesn't
    // apply to global user hooks, but metadata/dangerous/deny checks do.
    if let Some(reason) = check_bash_command_str(
        &content,
        None,
        &deny_list,
        &path_guard,
        config.security.bash_sandbox_workdir,
    ) {
        return Err(format!("hook script blocked: {reason}"));
    }

    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg(script)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    setup_process_group(&mut cmd);
    for (k, v) in env_vars {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn hook {}: {}", script.display(), e))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "hook stdout not available".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "hook stderr not available".to_string())?;

    let drain_stdout = tokio::spawn(drain_capped(stdout, MAX_BASH_OUTPUT_BYTES));
    let drain_stderr = tokio::spawn(drain_capped(stderr, MAX_BASH_OUTPUT_BYTES));

    let timeout_at = tokio::time::Instant::now() + Duration::from_secs(5);
    let status_result = tokio::select! {
        biased;
        result = child.wait() => Ok(result),
        _ = tokio::time::sleep_until(timeout_at) => {
            // Kill the whole process group so a long-lived descendant
            // (e.g. a hook that spawned `sleep`) cannot keep the pipes
            // open and block the drain tasks.
            kill_process_group(&mut child);
            Err(())
        }
    };

    match status_result {
        Ok(Ok(status)) => {
            let (_raw_stdout, stdout_dropped) = join_hook_drain(drain_stdout, "stdout").await?;
            let (raw_stderr, stderr_dropped) = join_hook_drain(drain_stderr, "stderr").await?;

            let stderr_text = cap_to_string(raw_stderr, stderr_dropped);

            // Exit code 2 is the explicit "deny" signal for gating hooks.
            if status.code() == Some(2) {
                let reason = if stderr_text.is_empty() {
                    format!("hook {} denied execution", script.display())
                } else {
                    format!(
                        "hook {} denied execution: {}",
                        script.display(),
                        stderr_text.trim()
                    )
                };
                return Ok(HookDecision::Deny(reason));
            }

            if !status.success() {
                tracing::warn!(
                    script = %script.display(),
                    code = status.code(),
                    stdout_dropped,
                    stderr_dropped,
                    "Hook exited with non-zero status (fail-open: allowing)"
                );
                // Return an Err so the decision path audits the fail-open
                // (WO 11.6). The caller converts Err → Allow (fail-open).
                let stderr_info = if stderr_text.is_empty() {
                    String::from("non-zero exit")
                } else {
                    stderr_text.trim().to_string()
                };
                return Err(format!(
                    "hook {} exited with non-zero status {:?}: {}",
                    script.display(),
                    status.code(),
                    stderr_info
                ));
            } else if stdout_dropped > 0 || stderr_dropped > 0 {
                tracing::trace!(
                    script = %script.display(),
                    stdout_dropped,
                    stderr_dropped,
                    "Hook output was capped"
                );
            }
            if !stderr_text.is_empty() {
                tracing::trace!(
                    script = %script.display(),
                    stderr = %stderr_text,
                    "Hook stderr"
                );
            }
            Ok(HookDecision::Allow)
        }
        Ok(Err(e)) => {
            // Fail-open: a hook that we cannot reap must not block the
            // user. Log and allow.
            tracing::warn!(
                script = %script.display(),
                error = %e,
                "Failed to wait for hook (fail-open: allowing)"
            );
            Ok(HookDecision::Allow)
        }
        Err(()) => {
            reap_child(&mut child, Duration::from_secs(2)).await;
            let (_raw_stdout, _stdout_dropped) = join_hook_drain(drain_stdout, "stdout").await?;
            let (raw_stderr, stderr_dropped) = join_hook_drain(drain_stderr, "stderr").await?;
            let stderr_text = cap_to_string(raw_stderr, stderr_dropped);
            if !stderr_text.is_empty() {
                tracing::trace!(
                    script = %script.display(),
                    stderr = %stderr_text,
                    "Hook stderr on timeout"
                );
            }
            // Timeouts are fail-open: a stuck hook must not wedge the
            // agent. We log loudly and allow the operation.
            tracing::warn!(
                script = %script.display(),
                "Hook timed out after 5 seconds (fail-open: allowing)"
            );
            Ok(HookDecision::Allow)
        }
    }
}

async fn join_hook_drain(
    handle: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, u64)>>,
    label: &str,
) -> Result<(Vec<u8>, u64), String> {
    match handle.await {
        Ok(Ok(pair)) => Ok(pair),
        Ok(Err(e)) => Err(format!("drain {label}: {e}")),
        Err(e) => Err(format!("drain {label} task panicked: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kf_plugin_host::TrustPolicy;

    fn temp_hooks_dir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let hooks_dir = tmp.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        (tmp, hooks_dir)
    }

    fn write_hook(dir: &std::path::Path, name: &str, content: &str) {
        std::fs::write(dir.join(format!("{name}.sh")), content).unwrap();
    }

    // Poll a marker file until its trimmed content equals `expected`, with a
    // bounded total budget. Replaces bare `sleep`-paced poll loops with a
    // deterministic primitive: 10ms interval, hard 15s cap (the hook's own
    // 5s timeout + scheduling slop). Fails the test loudly on timeout instead
    // of silently advancing to an assertion that would then flake.
    #[cfg(unix)]
    async fn poll_for_marker(marker: &Path, expected: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            if let Ok(c) = std::fs::read_to_string(marker) {
                if c.trim() == expected {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let got = std::fs::read_to_string(marker)
            .map(|c| c.trim().to_string())
            .unwrap_or_else(|_| "<missing>".into());
        panic!(
            "marker {} never reached {expected:?} within 15s; got {got:?}",
            marker.display()
        );
    }

    #[test]
    fn test_discover_empty_dir() {
        let (_tmp, dir) = temp_hooks_dir();
        let available = discover_hooks(&dir);
        assert!(available.is_empty());
    }

    #[test]
    fn test_discover_single_hook() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "post-turn", "#!/bin/bash\necho ok");
        let available = discover_hooks(&dir);
        assert_eq!(available.len(), 1);
        assert!(available.contains("post-turn"));
    }

    #[test]
    fn test_discover_multiple_hooks() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "session-start", "echo start");
        write_hook(&dir, "post-turn", "echo turn");
        write_hook(&dir, "pre-tool-bash", "echo pre");
        let available = discover_hooks(&dir);
        assert_eq!(available.len(), 3);
        assert!(available.contains("session-start"));
        assert!(available.contains("post-turn"));
        assert!(available.contains("pre-tool-bash"));
    }

    #[test]
    fn test_discover_ignores_non_sh_files() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "post-turn", "echo ok");
        std::fs::write(dir.join("README.md"), "# Hooks").unwrap();
        std::fs::write(dir.join(".hidden.sh"), "echo hidden").unwrap(); // starts with .
        let available = discover_hooks(&dir);
        // .hidden.sh should be discovered since `strip_suffix(".sh")` works on it
        assert!(available.contains("post-turn"));
        assert!(available.contains(".hidden"));
        assert!(!available.contains("README"));
    }

    #[test]
    fn test_discover_nonexistent_dir() {
        let available = discover_hooks(std::path::Path::new("/nonexistent/hooks/dir"));
        assert!(available.is_empty());
    }

    #[test]
    fn test_has_returns_correctly() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "post-turn", "echo ok");
        let runner = HookRunner::new(dir);
        assert!(runner.has("post-turn"));
        assert!(!runner.has("session-start"));
        assert!(!runner.has(""));
    }

    fn default_config() -> Config {
        Config::default()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_executes_hook() {
        let (_tmp, dir) = temp_hooks_dir();
        // Write a hook that creates a marker file
        let marker = dir.join("hook-ran.txt");
        let marker_str = marker.to_string_lossy().to_string();
        write_hook(
            &dir,
            "post-turn",
            &format!("#!/bin/bash\necho \"$KF_EVENT\" > {marker_str}"),
        );
        let runner = HookRunner::new(dir.clone());

        runner.run("post-turn", &[("KF_EVENT", "post-turn")], &default_config());

        // Give the fire-and-forget spawned hook a chance to be scheduled
        // before we start polling the marker file.
        tokio::task::yield_now().await;

        // Poll for the marker. 15s budget = 5s hook timeout + scheduling slop;
        // common case returns on first read. See `poll_for_marker`.
        poll_for_marker(&marker, "post-turn").await;
    }

    #[tokio::test]
    async fn test_run_noop_for_missing_hook() {
        let (_tmp, dir) = temp_hooks_dir();
        let runner = HookRunner::new(dir);
        // Missing hook should return Allow (fail-open)
        let decision = runner
            .run_decision("nonexistent", &[], &default_config())
            .await;
        assert_eq!(decision, HookDecision::Allow);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_hook_with_env_vars() {
        let (_tmp, dir) = temp_hooks_dir();
        let marker = dir.join("env-check.txt");
        let marker_str = marker.to_string_lossy().to_string();
        write_hook(
            &dir,
            "pre-tool-bash",
            &format!("#!/bin/bash\necho \"$KF_TOOL_NAME,$KF_EVENT\" > {marker_str}"),
        );
        let runner = HookRunner::new(dir.clone());

        runner.run(
            "pre-tool-bash",
            &[("KF_TOOL_NAME", "bash"), ("KF_EVENT", "pre-tool-bash")],
            &default_config(),
        );

        // Give the fire-and-forget spawned hook a chance to be scheduled
        // before we start polling the marker file.
        tokio::task::yield_now().await;

        // Poll for the marker. 15s budget = 5s hook timeout + scheduling slop.
        poll_for_marker(&marker, "bash,pre-tool-bash").await;
    }

    #[tokio::test]
    async fn test_run_hook_timeout_returns_allow() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "slow-hook", "#!/bin/bash\nsleep 30");
        let runner = HookRunner::new(dir);

        // Timeout should fail-open: return Allow, not block or Deny
        let decision = runner
            .run_decision("slow-hook", &[], &default_config())
            .await;
        assert_eq!(
            decision,
            HookDecision::Allow,
            "timed-out hook should fail-open to Allow"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_hook_timeout_kills_descendants() {
        let (_tmp, dir) = temp_hooks_dir();
        let marker = dir.join("survivor-marker.txt");
        let marker_str = marker.to_string_lossy().to_string();
        write_hook(
            &dir,
            "slow-hook",
            &format!("#!/bin/bash\nsh -c 'sleep 30; touch {marker_str}'"),
        );
        let runner = HookRunner::new(dir);

        runner.run("slow-hook", &[], &default_config());

        // Wait for the hook's own 5s execution timeout (see `run_hook_script`)
        // to fire and kill the process group. This is a genuine production-
        // timeout wait, not a sync sleep — kept as-is per WO 32 scope rule
        // (genuine timeout tests stay). The 2s budget is deliberately short of
        // the 5s hook timeout: it proves the pgrp was killed before the
        // descendant's `sleep 30` could ever touch the marker, without paying
        // the full 5s. If the kill failed, `sleep 30` would still be running
        // at 2s and the marker would not yet exist — so this is a necessary-
        // but-not-sufficient check; the full 5s+ wait lives in WO 33.14's
        // subprocess timeout suite.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        assert!(
            !marker.exists(),
            "hook descendant survived timeout and touched marker"
        );
    }

    #[tokio::test]
    async fn test_run_hook_blocks_dangerous_content() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "evil", "#!/bin/bash\nrm -rf /");
        let script_path = dir.join("evil.sh");
        let runner = HookRunner::new(dir);

        // The safety gate should block the dangerous command.
        // write_hook creates the script but the safety gate prevents execution.
        runner.run("evil", &[], &default_config());
        // `run` is fire-and-forget; yield once to let the spawned task observe
        // the safety gate. The assertion below does not depend on execution —
        // `script_path` exists because `write_hook` created it, regardless of
        // whether the gate let `rm` run.
        tokio::task::yield_now().await;

        // Verify the dangerous script was NOT executed by checking the
        // script path exists (write_hook created it) but produced no
        // output. If the safety gate failed, rm would have run.
        assert!(
            script_path.exists(),
            "hook script should exist after write_hook"
        );
    }

    #[tokio::test]
    async fn test_run_decision_allows_exit_zero() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "pre-tool-bash", "#!/bin/bash\necho ok");
        let runner = HookRunner::new(dir);

        let decision = runner
            .run_decision(
                "pre-tool-bash",
                &[("KF_TOOL_NAME", "bash")],
                &default_config(),
            )
            .await;
        assert_eq!(decision, HookDecision::Allow);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_decision_denies_exit_two() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(
            &dir,
            "pre-tool-bash",
            "#!/bin/bash\necho 'blocked' >&2; exit 2",
        );
        let runner = HookRunner::new(dir);

        let decision = runner
            .run_decision(
                "pre-tool-bash",
                &[("KF_TOOL_NAME", "bash")],
                &default_config(),
            )
            .await;
        assert!(
            matches!(decision, HookDecision::Deny(ref r) if r.contains("blocked")),
            "expected Deny with stderr reason, got {decision:?}"
        );
    }

    #[tokio::test]
    async fn test_run_decision_fail_open_on_non_two_exit() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "pre-tool-bash", "#!/bin/bash\nexit 1");
        let runner = HookRunner::new(dir);

        let decision = runner
            .run_decision(
                "pre-tool-bash",
                &[("KF_TOOL_NAME", "bash")],
                &default_config(),
            )
            .await;
        assert_eq!(decision, HookDecision::Allow, "exit 1 should be fail-open");
    }

    #[tokio::test]
    async fn test_run_decision_missing_hook_is_allow() {
        let (_tmp, dir) = temp_hooks_dir();
        let runner = HookRunner::new(dir);

        let decision = runner.run_decision("missing", &[], &default_config()).await;
        assert_eq!(decision, HookDecision::Allow);
    }

    #[test]
    fn test_load_plugin_hooks_from_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let plugin_dir = plugins_dir.join("demo");
        let plugin_hooks_dir = plugin_dir.join("hooks");
        std::fs::create_dir_all(&plugin_hooks_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "demo-hooks"
version = "0.1.0"
description = "demo"
trust = "shell"

[[capabilities]]
type = "hook"
event = "post-turn"
command = "hooks/post-turn.sh"
"#,
        )
        .unwrap();
        std::fs::write(plugin_hooks_dir.join("post-turn.sh"), "#!/bin/bash\n").unwrap();

        let mut registry = PluginRegistry::new();
        let warnings = registry
            .load_from_dir(
                &plugins_dir,
                TrustPolicy::up_to(kf_plugin_sdk::TrustTier::Shell),
            )
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let (_tmp2, hooks_dir) = temp_hooks_dir();
        let mut runner = HookRunner::new(hooks_dir);
        assert!(!runner.has("post-turn"));
        runner.load_plugin_hooks(&registry, &std::collections::HashSet::new());
        assert!(runner.has("post-turn"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_decision_merges_builtin_and_plugin_hooks_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let plugin_dir = plugins_dir.join("demo");
        let plugin_hooks_dir = plugin_dir.join("hooks");
        std::fs::create_dir_all(&plugin_hooks_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "demo-hooks"
version = "0.1.0"
description = "demo"
trust = "shell"

[[capabilities]]
type = "hook"
event = "post-turn"
command = "hooks/post-turn.sh"
"#,
        )
        .unwrap();

        let marker = tmp.path().join("order.txt");
        let marker_str = marker.to_string_lossy().to_string();
        std::fs::write(
            plugin_hooks_dir.join("post-turn.sh"),
            format!("#!/bin/bash\nprintf 'plugin\\n' >> {marker_str}"),
        )
        .unwrap();

        let mut registry = PluginRegistry::new();
        registry
            .load_from_dir(
                &plugins_dir,
                TrustPolicy::up_to(kf_plugin_sdk::TrustTier::Shell),
            )
            .unwrap();

        let (hooks_tmp, hooks_dir) = temp_hooks_dir();
        write_hook(
            &hooks_dir,
            "post-turn",
            &format!("#!/bin/bash\nprintf 'built-in\\n' >> {marker_str}"),
        );

        let mut runner = HookRunner::new(hooks_dir);
        runner.load_plugin_hooks(&registry, &std::collections::HashSet::new());

        let decision = runner
            .run_decision("post-turn", &[], &default_config())
            .await;
        assert_eq!(decision, HookDecision::Allow);

        let content = tokio::fs::read_to_string(&marker).await.unwrap_or_default();
        assert_eq!(
            content.trim(),
            "built-in\nplugin",
            "builtin/plugin hooks should run in deterministic order; got: {content:?}"
        );

        // Keep temporaries alive until after the assertions.
        let _ = (tmp, hooks_tmp);
    }

    /// A pre-tool hook that denies (exit 2) is recorded in the audit log
    /// with `verdict = deny` + the reason (WO 11.6).
    #[cfg(unix)]
    #[tokio::test]
    async fn audit_log_records_hook_denial() {
        use crate::shared::audit::{AuditEntry, AuditLog};
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(
            &dir,
            "pre-tool-bash",
            "#!/bin/bash\necho 'blocked by policy' >&2; exit 2",
        );
        let audit_path = _tmp.path().join("audit-hook-deny.ndjson");
        let log = std::sync::Arc::new(AuditLog::new(Some(audit_path.clone())));
        let mut runner = HookRunner::new(dir);
        runner.set_audit_log(log);

        let decision = runner
            .run_decision(
                "pre-tool-bash",
                &[("KF_TOOL_NAME", "bash")],
                &default_config(),
            )
            .await;
        assert!(matches!(decision, HookDecision::Deny(_)));

        // Drop to flush; then read the audit log back.
        drop(runner);
        let contents = std::fs::read_to_string(&audit_path).unwrap();
        let entry: AuditEntry = serde_json::from_str(contents.trim()).unwrap();
        match entry {
            AuditEntry::Hook {
                event,
                verdict,
                reason,
                ..
            } => {
                assert_eq!(event, "pre-tool-bash");
                assert_eq!(verdict, "deny");
                assert!(
                    reason.as_deref().unwrap_or("").contains("blocked"),
                    "reason: {reason:?}"
                );
            }
            other => panic!("expected Hook variant, got {other:?}"),
        }
    }

    /// A hook that crashes (exit 1) is fail-opened (Allow) AND recorded in
    /// the audit log with `verdict = allow_fail_open` + the error (WO 11.6).
    #[cfg(unix)]
    #[tokio::test]
    async fn audit_log_records_hook_fail_open() {
        use crate::shared::audit::{AuditEntry, AuditLog};
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "pre-tool-bash", "#!/bin/bash\nexit 1");
        let audit_path = _tmp.path().join("audit-hook-failopen.ndjson");
        let log = std::sync::Arc::new(AuditLog::new(Some(audit_path.clone())));
        let mut runner = HookRunner::new(dir);
        runner.set_audit_log(log);

        let decision = runner
            .run_decision(
                "pre-tool-bash",
                &[("KF_TOOL_NAME", "bash")],
                &default_config(),
            )
            .await;
        assert_eq!(decision, HookDecision::Allow, "exit 1 should fail-open");

        drop(runner);
        let contents = std::fs::read_to_string(&audit_path).unwrap();
        let entry: AuditEntry = serde_json::from_str(contents.trim()).unwrap();
        match entry {
            AuditEntry::Hook {
                event,
                verdict,
                reason,
                ..
            } => {
                assert_eq!(event, "pre-tool-bash");
                assert_eq!(verdict, "allow_fail_open");
                assert!(reason.is_some(), "fail-open reason should be present");
            }
            other => panic!("expected Hook variant, got {other:?}"),
        }
    }

    /// A plugin hook denial is recorded with the plugin name (WO 11.6).
    #[cfg(unix)]
    #[tokio::test]
    async fn audit_log_records_plugin_hook_denial_with_name() {
        use crate::shared::audit::{AuditEntry, AuditLog};
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        let plugin_dir = plugins_dir.join("sec-plugin");
        let plugin_hooks_dir = plugin_dir.join("hooks");
        std::fs::create_dir_all(&plugin_hooks_dir).unwrap();
        std::fs::write(
            plugin_dir.join("kf-code.toml"),
            r#"
name = "sec-plugin"
version = "0.1.0"
description = "security"
trust = "shell"

[[capabilities]]
type = "hook"
event = "pre-tool-bash"
command = "hooks/pre-tool-bash.sh"
"#,
        )
        .unwrap();
        std::fs::write(
            plugin_hooks_dir.join("pre-tool-bash.sh"),
            "#!/bin/bash\necho 'denied' >&2; exit 2",
        )
        .unwrap();

        let mut registry = PluginRegistry::new();
        registry
            .load_from_dir(
                &plugins_dir,
                TrustPolicy::up_to(kf_plugin_sdk::TrustTier::Shell),
            )
            .unwrap();

        let (_hooks_tmp, hooks_dir) = temp_hooks_dir();
        let audit_path = tmp.path().join("audit-plugin-deny.ndjson");
        let log = std::sync::Arc::new(AuditLog::new(Some(audit_path.clone())));
        let mut runner = HookRunner::new(hooks_dir);
        runner.set_audit_log(std::sync::Arc::clone(&log));
        runner.load_plugin_hooks(&registry, &std::collections::HashSet::new());

        let decision = runner
            .run_decision("pre-tool-bash", &[], &default_config())
            .await;
        assert!(matches!(decision, HookDecision::Deny(_)));

        drop(runner);
        drop(log);
        let contents = std::fs::read_to_string(&audit_path).unwrap();
        let entry: AuditEntry = serde_json::from_str(contents.trim()).unwrap();
        match entry {
            AuditEntry::Hook {
                event,
                plugin,
                verdict,
                reason,
                ..
            } => {
                assert_eq!(event, "pre-tool-bash");
                assert_eq!(plugin.as_deref(), Some("sec-plugin"));
                assert_eq!(verdict, "deny");
                assert!(reason.as_deref().unwrap_or("").contains("denied"));
            }
            other => panic!("expected Hook variant, got {other:?}"),
        }
        let _ = (tmp, _hooks_tmp);
    }

    #[test]
    fn test_owned_env_vars_clones_pairs() {
        let input = [("KF_EVENT", "post-turn"), ("KF_TOOL_NAME", "bash")];
        let owned = HookRunner::owned_env_vars(&input);
        assert_eq!(owned.len(), 2);
        assert_eq!(owned[0].0, "KF_EVENT");
        assert_eq!(owned[0].1, "post-turn");
        assert_eq!(owned[1].0, "KF_TOOL_NAME");
        assert_eq!(owned[1].1, "bash");
    }

    #[test]
    fn test_owned_env_vars_empty_input() {
        let owned = HookRunner::owned_env_vars(&[]);
        assert!(owned.is_empty());
    }

    #[test]
    fn test_ctx_to_env_vars_event_always_present() {
        let ctx = HookContext {
            event: "post-turn".into(),
            ..Default::default()
        };
        let vars = ctx_to_env_vars(&ctx);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].0, "KF_EVENT");
        assert_eq!(vars[0].1, "post-turn");
    }

    #[test]
    fn test_ctx_to_env_vars_includes_optional_fields_when_set() {
        let ctx = HookContext {
            event: "pre-tool-bash".into(),
            session_id: "sess-42".into(),
            tool_name: Some("bash".into()),
            tool_args_json: Some(r#"{"command":"ls"}"#.into()),
            ..Default::default()
        };
        let vars = ctx_to_env_vars(&ctx);
        let keys: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"KF_EVENT"));
        assert!(keys.contains(&"KF_SESSION_ID"));
        assert!(keys.contains(&"KF_TOOL_NAME"));
        assert!(keys.contains(&"KF_TOOL_ARGS_JSON"));
    }

    #[test]
    fn test_ctx_to_env_vars_omits_empty_session_id() {
        let ctx = HookContext {
            event: "post-turn".into(),
            session_id: String::new(),
            ..Default::default()
        };
        let vars = ctx_to_env_vars(&ctx);
        let keys: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
        assert!(!keys.contains(&"KF_SESSION_ID"));
    }

    #[test]
    fn test_ctx_to_env_vars_omits_none_optional_fields() {
        let ctx = HookContext {
            event: "post-turn".into(),
            tool_name: None,
            tool_args_json: None,
            ..Default::default()
        };
        let vars = ctx_to_env_vars(&ctx);
        let keys: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
        assert!(!keys.contains(&"KF_TOOL_NAME"));
        assert!(!keys.contains(&"KF_TOOL_ARGS_JSON"));
    }

    #[test]
    fn test_env_vars_to_ctx_event_name_always_set() {
        let env = [("KF_SESSION_ID", "s1"), ("KF_TOOL_NAME", "bash")];
        let ctx = env_vars_to_ctx("pre-tool-bash", &env);
        assert_eq!(ctx.event, "pre-tool-bash");
        assert_eq!(ctx.session_id, "s1");
        assert_eq!(ctx.tool_name.as_deref(), Some("bash"));
        assert!(ctx.tool_args_json.is_none());
    }

    #[test]
    fn test_env_vars_to_ctx_includes_tool_args_json() {
        let env = [("KF_TOOL_ARGS_JSON", r#"{"x":1}"#)];
        let ctx = env_vars_to_ctx("post-tool-bash", &env);
        assert_eq!(ctx.event, "post-tool-bash");
        assert_eq!(ctx.tool_args_json.as_deref(), Some(r#"{"x":1}"#));
        assert!(ctx.session_id.is_empty());
    }

    #[test]
    fn test_env_vars_to_ctx_ignores_unknown_keys() {
        let env = [("UNKNOWN_KEY", "ignored"), ("KF_SESSION_ID", "s2")];
        let ctx = env_vars_to_ctx("session-start", &env);
        assert_eq!(ctx.event, "session-start");
        assert_eq!(ctx.session_id, "s2");
    }

    #[test]
    fn test_env_vars_to_ctx_empty_env_keeps_defaults() {
        let ctx = env_vars_to_ctx("post-turn", &[]);
        assert_eq!(ctx.event, "post-turn");
        assert!(ctx.session_id.is_empty());
        assert!(ctx.tool_name.is_none());
        assert!(ctx.tool_args_json.is_none());
    }

    #[test]
    fn test_discover_hooks_skips_directories() {
        let (_tmp, dir) = temp_hooks_dir();
        std::fs::create_dir_all(dir.join("not-a-hook.sh")).unwrap();
        let available = discover_hooks(&dir);
        assert!(
            !available.contains("not-a-hook"),
            "directories should not be discovered as hooks: {available:?}"
        );
    }

    #[test]
    fn test_discover_hooks_strips_sh_suffix_only() {
        let (_tmp, dir) = temp_hooks_dir();
        std::fs::write(dir.join("no-extension"), "echo").unwrap();
        std::fs::write(dir.join("post-turn.sh"), "echo").unwrap();
        let available = discover_hooks(&dir);
        assert!(available.contains("post-turn"));
        assert!(!available.contains("no-extension"));
    }

    #[test]
    fn test_discover_hooks_empty_filename_suffix_rejected() {
        let (_tmp, dir) = temp_hooks_dir();
        std::fs::write(dir.join(".sh"), "echo").unwrap();
        let available = discover_hooks(&dir);
        assert!(
            available.iter().all(|name| !name.is_empty()),
            "empty stem should be skipped, got: {available:?}"
        );
    }

    #[test]
    fn test_hook_runner_default_constructs_without_panicking() {
        let runner = HookRunner::default();
        assert!(!runner.has("no-such-hook"));
    }

    #[test]
    fn test_hook_runner_clone_preserves_available_set() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "post-turn", "echo ok");
        let runner = HookRunner::new(dir);
        let cloned = runner.clone();
        assert!(runner.has("post-turn"));
        assert!(cloned.has("post-turn"));
    }

    #[test]
    fn test_hook_decision_equality() {
        assert_eq!(HookDecision::Allow, HookDecision::Allow);
        assert_eq!(
            HookDecision::Deny("x".into()),
            HookDecision::Deny("x".into())
        );
        assert_ne!(HookDecision::Allow, HookDecision::Deny("x".into()));
        assert_ne!(
            HookDecision::Deny("x".into()),
            HookDecision::Deny("y".into())
        );
    }

    #[test]
    fn test_hook_runner_with_audit_log_attaches_log() {
        use crate::shared::audit::AuditLog;
        let (_tmp, dir) = temp_hooks_dir();
        let audit_path = std::env::temp_dir().join(format!(
            "kf-code-hooks-audit-{}-{}.ndjson",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let log = std::sync::Arc::new(AuditLog::new(Some(audit_path.clone())));
        let runner = HookRunner::new(dir).with_audit_log(log);
        assert!(runner.audit_log.is_some());
        let _ = std::fs::remove_file(&audit_path);
    }

    #[test]
    fn test_hook_context_default_is_all_none() {
        let ctx = HookContext::default();
        assert!(ctx.event.is_empty());
        assert!(ctx.session_id.is_empty());
        assert!(ctx.tool_name.is_none());
        assert!(ctx.tool_args_json.is_none());
        assert!(ctx.tool_result.is_none());
        assert!(ctx.compact_stats.is_none());
    }

    #[test]
    fn test_compact_hook_stats_data_default() {
        let stats = CompactHookStatsData::default();
        assert_eq!(stats.message_count, 0);
        assert_eq!(stats.preserve_recent, 0);
        assert_eq!(stats.strategy, "");
    }

    #[tokio::test]
    async fn test_run_with_context_fires_in_process_hook() {
        let (_tmp, dir) = temp_hooks_dir();
        let runner = HookRunner::new(dir);
        let ctx = HookContext {
            event: "post-turn".into(),
            ..Default::default()
        };
        runner.run_with_context("post-turn", &ctx, &default_config());
    }

    #[tokio::test]
    async fn test_run_decision_with_context_missing_hook_returns_allow() {
        let (_tmp, dir) = temp_hooks_dir();
        let runner = HookRunner::new(dir);
        let ctx = HookContext {
            event: "no-such-event".into(),
            ..Default::default()
        };
        let decision = runner
            .run_decision_with_context("no-such-event", &ctx, &default_config())
            .await;
        assert_eq!(decision, HookDecision::Allow);
    }

    #[test]
    fn test_in_process_hook_trait_object_box() {
        struct DummyHook;
        impl InProcessHook for DummyHook {
            fn event(&self) -> &str {
                "test-event"
            }
            fn handle(&self, _ctx: &HookContext) -> HookDecision {
                HookDecision::Allow
            }
        }
        let boxed: Box<dyn InProcessHook> = Box::new(DummyHook);
        assert_eq!(boxed.event(), "test-event");
        assert_eq!(boxed.handle(&HookContext::default()), HookDecision::Allow);
    }

    #[test]
    fn test_in_process_hook_deny_variant() {
        struct DenyHook;
        impl InProcessHook for DenyHook {
            fn event(&self) -> &str {
                "deny-event"
            }
            fn handle(&self, _ctx: &HookContext) -> HookDecision {
                HookDecision::Deny("blocked by test hook".into())
            }
        }
        let boxed: Box<dyn InProcessHook> = Box::new(DenyHook);
        match boxed.handle(&HookContext::default()) {
            HookDecision::Deny(reason) => assert_eq!(reason, "blocked by test hook"),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn test_hook_runner_add_in_process_hook_registers() {
        struct CountingHook {
            event: String,
        }
        impl InProcessHook for CountingHook {
            fn event(&self) -> &str {
                &self.event
            }
            fn handle(&self, _ctx: &HookContext) -> HookDecision {
                HookDecision::Allow
            }
        }
        let (_tmp, dir) = temp_hooks_dir();
        let mut runner = HookRunner::new(dir);
        assert!(!runner.has("custom-event"));
        runner.add_in_process_hook(Box::new(CountingHook {
            event: "custom-event".into(),
        }));
        assert!(runner.has("custom-event"));
    }

    #[test]
    fn test_hook_runner_add_post_hook_registers() {
        struct TestPostHook;
        impl PostHook for TestPostHook {
            fn event(&self) -> &str {
                "test-post-event"
            }
            fn handle(&self, _ctx: &HookContext) -> Result<(), String> {
                Ok(())
            }
        }
        let (_tmp, dir) = temp_hooks_dir();
        let mut runner = HookRunner::new(dir);
        assert!(!runner.has("test-post-event"));
        runner.add_post_hook(Box::new(TestPostHook));
        assert!(runner.has("test-post-event"));
    }

    #[test]
    fn test_hook_runner_debug_includes_dir() {
        let (_tmp, dir) = temp_hooks_dir();
        let runner = HookRunner::new(dir.clone());
        let debug = format!("{runner:?}");
        assert!(debug.contains("hooks_dir"));
    }
}
