//! Lifecycle hook system — user-defined shell scripts triggered on events.
//!
//! Hooks are shell scripts placed in `~/.local/share/kirkforge/hooks/`.
//! Naming convention: `<event>.sh` — e.g., `pre-tool-bash.sh`,
//! `post-tool-write_file.sh`, `post-turn.sh`, `session-start.sh`,
//! `pre-compact.sh`, `post-compact.sh`.
//!
//! Each hook receives event data as environment variables:
//! - `KF_EVENT` — the event name (e.g., "post-turn")
//! - `KF_TOOL_NAME` — the tool being called (tool events only)
//! - `KF_TOOL_ARGS_JSON` — JSON-serialised tool arguments (tool events only)
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
use crate::session::process_group::{kill_process_group, setup_process_group};
use crate::shared::Config;
use crate::tools::bash::{
    cap_to_string, check_bash_command_str, drain_capped, MAX_BASH_OUTPUT_BYTES,
};

/// Discovers and runs lifecycle hook scripts.
#[derive(Debug, Clone)]
pub struct HookRunner {
    /// Directory containing hook scripts.
    hooks_dir: PathBuf,
    /// Set of available hook names (without `.sh` suffix).
    available: HashSet<String>,
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
        }
    }

    /// Check whether a hook with the given event name exists.
    pub fn has(&self, event_name: &str) -> bool {
        self.available.contains(event_name)
    }

    /// Run a hook script asynchronously (fire-and-forget).
    ///
    /// If no script exists for `event_name`, this is a no-op.
    /// The script is invoked via `bash` with a 5-second timeout.
    /// `env_vars` are passed as additional environment variables
    /// (pairs of key, value — both owned for the spawned future).
    ///
    /// Before executing, the script content passes through the same
    /// safety gate as the model's `bash` tool (metadata blocks,
    /// dangerous patterns, path/url deny lists). Stdout and stderr are
    /// capped so a runaway hook can't OOM the process.
    ///
    /// For hooks that may deny an operation (i.e. `pre-tool-*`), use
    /// [`Self::run_decision`] instead. This method always treats the
    /// hook as observational.
    pub fn run(&self, event_name: &str, env_vars: &[(&str, &str)], config: &Config) {
        if !self.has(event_name) {
            return;
        }

        let script_path = self.hooks_dir.join(format!("{}.sh", event_name));
        let event = event_name.to_string();
        let owned_vars: Vec<(String, String)> = env_vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let script_path_owned = script_path.clone();
        let config = config.clone();

        let handle = match tokio::runtime::Handle::try_current() {
            Ok(rt) => rt.spawn(async move {
                match run_hook_script(&script_path_owned, &owned_vars, &config).await {
                    Ok(HookDecision::Allow) => {}
                    Ok(HookDecision::Deny(reason)) => {
                        // Fire-and-forget path: a deny here is too late to
                        // block, so we log it as a warning.
                        tracing::warn!(
                            event = %event,
                            reason = %reason,
                            "Observational hook reported deny after the fact"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(event = %event, error = %e, "Hook run failed");
                    }
                }
            }),
            Err(e) => {
                tracing::warn!(event = %event, error = %e, "no Tokio runtime available; hook skipped");
                return;
            }
        };
        // Detach the task; hooks are best-effort and must not block.
        std::mem::drop(handle);
    }

    /// Run a hook that is allowed to deny an operation and await its
    /// decision.
    ///
    /// Returns [`HookDecision::Allow`] if the hook is missing, succeeds,
    /// exits with any non-zero code other than `2`, times out, or fails
    /// to execute. Returns [`HookDecision::Deny`] only when the hook
    /// exits with code `2`.
    pub async fn run_decision(
        &self,
        event_name: &str,
        env_vars: &[(&str, &str)],
        config: &Config,
    ) -> HookDecision {
        if !self.has(event_name) {
            return HookDecision::Allow;
        }

        let script_path = self.hooks_dir.join(format!("{}.sh", event_name));
        let owned_vars: Vec<(String, String)> = env_vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        match run_hook_script(&script_path, &owned_vars, config).await {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(event = %event_name, error = %e, "Decision hook failed (fail-open)");
                HookDecision::Allow
            }
        }
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

/// Default hooks directory: `~/.local/share/kirkforge/hooks/`.
fn default_hooks_dir() -> anyhow::Result<PathBuf> {
    let base = crate::session::data_dir()?;
    Ok(base.join("hooks"))
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
        config.bash_sandbox_workdir,
    ) {
        return Err(format!("hook script blocked: {}", reason));
    }

    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg(script)
        .kill_on_drop(true)
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
            } else if stdout_dropped > 0 || stderr_dropped > 0 {
                tracing::debug!(
                    script = %script.display(),
                    stdout_dropped,
                    stderr_dropped,
                    "Hook output was capped"
                );
            }
            if !stderr_text.is_empty() {
                tracing::debug!(
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
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            let (_raw_stdout, _stdout_dropped) = join_hook_drain(drain_stdout, "stdout").await?;
            let (raw_stderr, stderr_dropped) = join_hook_drain(drain_stderr, "stderr").await?;
            let stderr_text = cap_to_string(raw_stderr, stderr_dropped);
            if !stderr_text.is_empty() {
                tracing::debug!(
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
        Ok(Err(e)) => Err(format!("drain {}: {}", label, e)),
        Err(e) => Err(format!("drain {} task panicked: {}", label, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_hooks_dir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let hooks_dir = tmp.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        (tmp, hooks_dir)
    }

    fn write_hook(dir: &std::path::Path, name: &str, content: &str) {
        std::fs::write(dir.join(format!("{}.sh", name)), content).unwrap();
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

    #[tokio::test]
    async fn test_run_executes_hook() {
        let (_tmp, dir) = temp_hooks_dir();
        // Write a hook that creates a marker file
        let marker = dir.join("hook-ran.txt");
        let marker_str = marker.to_string_lossy().to_string();
        write_hook(
            &dir,
            "post-turn",
            &format!("#!/bin/bash\necho \"$KF_EVENT\" > {}", marker_str),
        );
        let runner = HookRunner::new(dir.clone());

        runner.run("post-turn", &[("KF_EVENT", "post-turn")], &default_config());

        // Poll for the marker so the test stays stable under heavy
        // parallel test loads. Give up after ~2 seconds.
        let mut content = String::from("not-run");
        for _ in 0..40 {
            if let Ok(c) = std::fs::read_to_string(&marker) {
                content = c;
                if content.trim() == "post-turn" {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(content.trim(), "post-turn");
    }

    #[tokio::test]
    async fn test_run_noop_for_missing_hook() {
        let (_tmp, dir) = temp_hooks_dir();
        let runner = HookRunner::new(dir);
        // Should not panic or spawn anything
        runner.run("nonexistent", &[], &default_config());
    }

    #[tokio::test]
    async fn test_run_hook_with_env_vars() {
        let (_tmp, dir) = temp_hooks_dir();
        let marker = dir.join("env-check.txt");
        let marker_str = marker.to_string_lossy().to_string();
        write_hook(
            &dir,
            "pre-tool-bash",
            &format!(
                "#!/bin/bash\necho \"$KF_TOOL_NAME,$KF_EVENT\" > {}",
                marker_str
            ),
        );
        let runner = HookRunner::new(dir.clone());

        runner.run(
            "pre-tool-bash",
            &[("KF_TOOL_NAME", "bash"), ("KF_EVENT", "pre-tool-bash")],
            &default_config(),
        );

        let mut content = String::new();
        for _ in 0..40 {
            if let Ok(c) = std::fs::read_to_string(&marker) {
                content = c;
                if content.trim() == "bash,pre-tool-bash" {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(content.trim(), "bash,pre-tool-bash");
    }

    #[tokio::test]
    async fn test_run_hook_timeout_does_not_panic() {
        let (_tmp, dir) = temp_hooks_dir();
        write_hook(&dir, "slow-hook", "#!/bin/bash\nsleep 30");
        let runner = HookRunner::new(dir);

        // Should not block — timeout kills it
        runner.run("slow-hook", &[], &default_config());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // If we get here without the 30s sleep blocking, timeout works
    }

    #[tokio::test]
    async fn test_run_hook_timeout_kills_descendants() {
        let (_tmp, dir) = temp_hooks_dir();
        let marker = dir.join("survivor-marker.txt");
        let marker_str = marker.to_string_lossy().to_string();
        write_hook(
            &dir,
            "slow-hook",
            &format!("#!/bin/bash\nsh -c 'sleep 30; touch {}'", marker_str),
        );
        let runner = HookRunner::new(dir);

        runner.run("slow-hook", &[], &default_config());
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
        let runner = HookRunner::new(dir);

        // Should be a no-op at runtime because the safety gate blocks it.
        runner.run("evil", &[], &default_config());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // The only observable behaviour is "did not panic"; tracing covers
        // the block reason.
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
            "expected Deny with stderr reason, got {:?}",
            decision
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
}
