use crate::session::bash_runner::{cap_to_string, drain_capped, MAX_BASH_OUTPUT_BYTES};
use crate::shared::access::{DenyList, PathGuard};
use crate::shared::bash_safety::check_bash_command_str;
use crate::shared::shell::{
    global_registry, is_timeout_marker, run_shell_with_token, JobStatus, ShellError, ShellOutput,
};
use crate::shared::{DockerConfig, SandboxConfig, ToolDef, ToolError, ToolOutcome};
use crate::tools::bash_minify;
use crate::tools::{Tool, ToolContext};
use std::path::PathBuf;
use std::process::Stdio;

// ponytail: PTY support (portable-pty) deferred — adds ~2 MB to the
// release binary. Current pipe-based stdout/stderr capture is sufficient
// for all non-interactive commands. Add PTY behind an `--interactive` flag
// when interactive terminal programs (vim, top, python REPL) are needed.
// Upgrade path: `portable-pty` crate, gated behind `cfg(feature = "pty")`.

/// Maximum bash timeout in seconds. Clamped to prevent Duration/Instant
/// overflow when a model passes an enormous value.
const MAX_BASH_TIMEOUT_SECS: u64 = 24 * 60 * 60; // 24 hours

// Build the `docker run` CLI arg vector from a resolved config. Pure:
// no I/O, no Docker daemon. `workdir` must already be canonicalized and
// colon-checked by the caller (run_docker does both); this fn only
// stringifies it for the `-v <host>:/work` bind mount. `timeout_secs`
// is accepted to keep the signature complete but is NOT emitted as a
// Docker flag — the timeout is enforced by run_docker via a
// `tokio::time::sleep` select branch, not by the container runtime.
// Unit-tested in-process by the `build_docker_args_*` tests below.
fn build_docker_args(
    cfg: &DockerConfig,
    workdir: &std::path::Path,
    cmd: &str,
    _timeout_secs: u64,
) -> Vec<String> {
    let workdir_str = workdir.to_string_lossy();
    vec![
        "run".to_string(),
        "--rm".to_string(),
        "--network".to_string(),
        "none".to_string(),
        "--memory".to_string(),
        cfg.memory.clone(),
        "--cpus".to_string(),
        cfg.cpus.clone(),
        "-v".to_string(),
        format!("{workdir_str}:/work"),
        "-w".to_string(),
        "/work".to_string(),
        cfg.image.clone(),
        "/bin/sh".to_string(),
        "-c".to_string(),
        cmd.to_string(),
    ]
}

pub struct Bash {
    deny_list: DenyList,
    path_guard: PathGuard,
    bash_sandbox_workdir: bool,
    docker_config: Option<DockerConfig>,
    sandbox_config: SandboxConfig,
    /// Extra landlock allow-list paths (WO 27.1), sourced from
    /// `config.security.landlock_extra_paths`. Defaults empty; set by
    /// `all_tools` after construction so `Bash::new`'s arity is unchanged.
    pub(crate) landlock_extra_paths: Vec<PathBuf>,
}

impl Bash {
    pub fn new(
        deny_list: DenyList,
        path_guard: PathGuard,
        bash_sandbox_workdir: bool,
        docker_config: Option<DockerConfig>,
        sandbox_config: SandboxConfig,
    ) -> Self {
        Self {
            deny_list,
            path_guard,
            bash_sandbox_workdir,
            docker_config,
            sandbox_config,
            landlock_extra_paths: Vec::new(),
        }
    }

    /// Run a command inside a Docker container instead of directly on the host.
    async fn run_docker(
        &self,
        cmd: &str,
        workdir: &std::path::Path,
        timeout_secs: u64,
        token: &tokio_util::sync::CancellationToken,
    ) -> Result<(i32, String, String), ShellError> {
        // WO 15.10: the only caller guards with
        // `docker_config.as_ref().map_or(false, |c| c.enabled)`, so this
        // is `Some` in practice — but the invariant was implicit
        // (`.expect` would panic the runtime if a future caller forgets
        // the guard). Surface it as a normal `ShellError::Spawn` so the
        // tool result is a failure the model can react to, not a panic.
        let cfg = match self.docker_config.as_ref() {
            Some(c) => c,
            None => {
                return Err(ShellError::Spawn(
                    "docker_config is None (run_docker called without docker enabled)".into(),
                ));
            }
        };

        // WO 15.3: route the model-supplied `cmd` through the same
        // deny-list / dangerous-pattern gate the foreground path uses.
        // The Docker branch previously skipped `check_bash_command_str`,
        // so a model-supplied `rm -rf /` or metadata-endpoint curl ran
        // unchecked inside the container.
        if let Some(denied) = check_bash_command_str(
            cmd,
            None,
            &self.deny_list,
            &self.path_guard,
            self.bash_sandbox_workdir,
        ) {
            return Err(ShellError::Spawn(denied));
        }

        // WO 15.3: sanitize the bind-mount source. Docker parses the first
        // `:` in `-v SRC:/work` as the host/container split, so a workdir
        // string containing `:` (e.g. `/tmp/evil:/etc:ro`) would mount
        // `/tmp/evil` at `/etc`. The workdir comes from `DockerConfig`
        // (config), not the model, so this is defense-in-depth — but a
        // misconfigured path with `:` must not silently inject mount opts.
        // Canonicalize first so a relative `.` resolves, then reject if the
        // canonical path string contains a `:` (a valid filesystem path
        // never contains `:` on Unix; on Windows `C:\` would, but the Docker
        // tool is Unix-only by nature of the container runtime).
        let resolved_workdir = match workdir.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return Err(ShellError::Spawn(format!(
                    "docker workdir cannot be resolved: {} ({e})",
                    workdir.display()
                )));
            }
        };
        // M20: verify the canonical workdir is within the project root.
        // Without this check, a symlink inside workdir pointing to /etc
        // would canonicalize to /etc and get mounted read-write.
        if let Some(ref sandbox) = self.path_guard.sandbox_dir {
            let canonical_root = match sandbox.canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    return Err(ShellError::Spawn(format!(
                        "cannot canonicalize project root '{}': {e}",
                        sandbox.display()
                    )));
                }
            };
            if !resolved_workdir.starts_with(&canonical_root) {
                return Err(ShellError::Spawn(format!(
                    "Docker workdir escapes project root: {} is outside {}",
                    resolved_workdir.display(),
                    canonical_root.display()
                )));
            }
        }

        let workdir_str = resolved_workdir.to_string_lossy();
        if workdir_str.contains(':') {
            return Err(ShellError::Spawn(format!(
                "docker workdir contains ':' which would inject Docker mount options: {workdir_str}"
            )));
        }

        // `resolved_workdir` is already canonicalized + colon-checked above;
        // build_docker_args just stringifies it for the bind-mount source.
        let docker_args = build_docker_args(cfg, &resolved_workdir, cmd, timeout_secs);

        // WO 46.17: own process group so a timeout-driven kill reaches
        // container-spawned helper processes too — kill_on_drop only
        // reaps the direct `docker` child, leaving in-container
        // grandchildren holding the stdout/stderr pipes open past the
        // timeout (same class as WO 46.2 / 43.23).
        let mut docker_cmd = tokio::process::Command::new("docker");
        docker_cmd
            .args(&docker_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // WO 47.15: scrub credential-shaped env vars so container children
        // cannot exfiltrate provider/session secrets via `printenv` — the
        // docker client forwards its full env into `docker run` by default.
        // Mirrors the foreground/background bash paths (WO 43.28).
        crate::session::bash_runner::scrub_secrets_from_child_env(&mut docker_cmd);
        crate::session::process_group::setup_process_group(&mut docker_cmd);
        let mut child = docker_cmd
            .spawn()
            .map_err(|e| ShellError::Spawn(format!("docker spawn failed: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ShellError::Spawn("no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ShellError::Spawn("no stderr".into()))?;

        // WO 47.32: capped drain. read_to_end into unbounded Vecs let a
        // runaway container command OOM the host process — this is the
        // same MAX_BASH_OUTPUT_BYTES cap every other bash spawn path
        // enforces (run_shell_with_token, workflow bash). The drainer
        // keeps reading past the cap into a sink, so the container never
        // blocks on a full pipe and the marker reports the dropped count.
        let out_handle = tokio::spawn(drain_capped(stdout, MAX_BASH_OUTPUT_BYTES));
        let err_handle = tokio::spawn(drain_capped(stderr, MAX_BASH_OUTPUT_BYTES));

        let status = tokio::select! {
            status = child.wait() => status.map_err(|e| ShellError::Spawn(format!("docker wait: {e}")))?,
            _ = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)) => {
                let _ = child.kill().await;
                // mm-H18 (WO 47.32): report a timeout-shaped result, not
                // Cancelled — a timeout-killed container must surface as
                // ToolError::Timeout like the non-docker path. Drain join
                // keeps the 1s ceiling so orphaned reader tasks don't
                // linger as zombies after this branch returns.
                let (out, err) = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    async {
                        (
                            join_capped_drain(out_handle).await,
                            join_capped_drain(err_handle).await,
                        )
                    },
                )
                .await
                .unwrap_or_else(|_| ((Vec::new(), 0), (Vec::new(), 0)));
                return Ok(docker_timeout_result(timeout_secs, out, err));
            }
            _ = token.cancelled() => {
                let _ = child.kill().await;
                let _: Result<_, _> = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    async {
                        let _ = out_handle.await;
                        let _ = err_handle.await;
                    },
                ).await;
                return Err(ShellError::Cancelled);
            }
        };

        let (out_bytes, out_dropped) = join_capped_drain(out_handle).await;
        let (err_bytes, err_dropped) = join_capped_drain(err_handle).await;

        let stdout_str = cap_to_string(out_bytes, out_dropped);
        let stderr_str = cap_to_string(err_bytes, err_dropped);

        Ok((status.code().unwrap_or(-1), stdout_str, stderr_str))
    }
}

// Best-effort join of a capped drain task: a join error, io error, or
// deadline miss yields an empty capture instead of failing the whole
// invocation (same semantics as the old unwrap-or-default chain).
async fn join_capped_drain(
    handle: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, u64)>>,
) -> (Vec<u8>, u64) {
    match handle.await {
        Ok(Ok(pair)) => pair,
        _ => (Vec::new(), 0),
    }
}

// mm-H18 (WO 47.32): the docker timeout result, shaped exactly like
// run_shell_with_token's timeout path — a killed exit code (9 = SIGKILL
// on Unix; just a failing code on Windows) plus the stdout marker prefix
// is_timeout_marker recognizes, so Bash::run maps a timeout-killed
// container to ToolError::Timeout instead of a generic failure or
// Cancelled.
fn docker_timeout_result(
    timeout_secs: u64,
    out: (Vec<u8>, u64),
    err: (Vec<u8>, u64),
) -> (i32, String, String) {
    (
        9,
        format!(
            "[timed out after {timeout_secs} seconds]\n{}",
            cap_to_string(out.0, out.1)
        ),
        cap_to_string(err.0, err.1),
    )
}

#[async_trait::async_trait]
impl Tool for Bash {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash",
            description: "Execute a bash command. Use for running tests, builds, git operations, and file inspection. Output is captured and returned. Set \"background\": true to run long-lived commands in the background. Note: if your project invokes `python` (not `python3`), ensure the symlink exists (`ln -s $(which python3) /usr/local/bin/python`); many distros ship python3 only.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 30)",
                        "default": 30
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Working directory (default: project root)",
                        "default": "."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Run in background. Use bash_status to check status.",
                        "default": false
                    },
                    "interactive": {
                        "type": "boolean",
                        "description": "Allocate a PTY for interactive commands (requires --features pty). Use for vim, top, REPLs. Ignored when PTY support is not compiled in.",
                        "default": false
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let cmd = match args.get("command").and_then(|c| c.as_str()) {
            Some(c) => c.to_string(),
            None => {
                return ToolOutcome::Failure(crate::shared::ToolError::invalid_args(
                    "Missing 'command' argument",
                ));
            }
        };

        let timeout_secs = args
            .get("timeout")
            .and_then(|t| t.as_u64())
            .unwrap_or(30)
            .min(MAX_BASH_TIMEOUT_SECS);
        let workdir = args.get("workdir").and_then(|w| w.as_str()).unwrap_or(".");
        let workdir_path = PathBuf::from(shellexpand::tilde(workdir).as_ref());

        if ctx.dry_run {
            // Validate the command through the same safety gate the real
            // execution uses, even in dry-run mode, so the user sees whether
            // the command would be allowed.
            if let Some(denied) = check_bash_command_str(
                &cmd,
                Some(workdir),
                &self.deny_list,
                &self.path_guard,
                self.bash_sandbox_workdir,
            ) {
                return ToolOutcome::Failure(crate::shared::ToolError::AccessDenied {
                    message: denied,
                });
            }
            return ToolOutcome::Success {
                content: format!(
                    "Dry run: would execute bash command: {cmd}\n  workdir: {}\n  timeout: {timeout_secs}s",
                    workdir_path.display()
                ),
            };
        }

        // Check for background mode
        if args
            .get("background")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            let registry = global_registry();
            let workdir = args.get("workdir").and_then(|w| w.as_str());
            let timeout = args
                .get("timeout")
                .and_then(|t| t.as_u64())
                .map(|t| std::time::Duration::from_secs(t.min(MAX_BASH_TIMEOUT_SECS)));
            match registry
                .spawn(
                    &cmd,
                    workdir,
                    timeout,
                    &self.deny_list,
                    &self.path_guard,
                    self.bash_sandbox_workdir,
                    Some(&self.sandbox_config),
                    // WO 36.2: attribute the job to the calling task so
                    // task-cancel paths kill exactly the subagent's jobs;
                    // main-session calls carry owner None and are never
                    // touched by cancel_by_owner.
                    ctx.task_owner.as_deref(),
                )
                .await
            {
                Ok(id) => ToolOutcome::Success {
                    content: format!(
                        "Background job #{id} started. Use bash_status(id={id}) to check status."
                    ),
                },
                Err(e) => ToolOutcome::Failure(crate::shared::ToolError::internal(format!(
                    "Failed to start background job: {e}"
                ))),
            }
        } else {
            // WO 47.18: gate the foreground path in the tool itself.
            // pre_run only guards executor-driven calls; direct tool.run()
            // callers (workflow `tool:` steps, plugins, /replay) must not
            // execute model-authored commands unchecked. Same gate the
            // dry-run branch above and run_docker already apply; pre_run
            // stays as fast-fail — double-gating is a cheap pure string
            // check.
            if let Some(denied) = check_bash_command_str(
                &cmd,
                Some(workdir),
                &self.deny_list,
                &self.path_guard,
                self.bash_sandbox_workdir,
            ) {
                return ToolOutcome::Failure(crate::shared::ToolError::AccessDenied {
                    message: denied,
                });
            }

            let interactive = args
                .get("interactive")
                .and_then(|i| i.as_bool())
                .unwrap_or(false);

            #[cfg(feature = "pty")]
            if interactive {
                use crate::shared::shell::run_with_pty;
                return match run_with_pty(&cmd, &workdir_path, 80, 24, ctx.event_tx.clone()) {
                    Ok(pty_result) => {
                        let code = pty_result.exit_code.unwrap_or(-1);
                        if code == 0 {
                            ToolOutcome::Success {
                                content: pty_result.stdout,
                            }
                        } else {
                            ToolOutcome::Failure(ToolError::Execution {
                                message: format!(
                                    "PTY command exited with code {code}\n{}",
                                    pty_result.stdout
                                ),
                                exit_code: Some(code),
                                stderr: String::new(),
                            })
                        }
                    }
                    Err(e) => ToolOutcome::Failure(ToolError::Internal {
                        message: format!("PTY allocation failed: {e}"),
                    }),
                };
            }

            #[cfg(not(feature = "pty"))]
            let _ = interactive;

            // Normal foreground execution — use Docker if configured.
            let result = if self.docker_config.as_ref().map_or(false, |c| c.enabled) {
                match self
                    .run_docker(&cmd, &workdir_path, timeout_secs, &ctx.token)
                    .await
                {
                    Ok((code, stdout, stderr)) => {
                        // Synthesize an ExitStatus from the exit code.
                        // On Unix, ExitStatus can be constructed from a raw code
                        // via std::os::unix::process::ExitStatusExt.
                        let status = if code == 0 {
                            std::process::ExitStatus::default()
                        } else {
                            #[cfg(unix)]
                            {
                                use std::os::unix::process::ExitStatusExt;
                                std::process::ExitStatus::from_raw(code)
                            }
                            #[cfg(not(unix))]
                            {
                                // Windows ExitStatus is a raw u32 wrapper:
                                // success() == (code == 0), code() round-trips.
                                // Using default() here would report failing
                                // containerized commands as Success (WO 44.48).
                                use std::os::windows::process::ExitStatusExt;
                                std::process::ExitStatus::from_raw(code as u32)
                            }
                        };
                        Ok(ShellOutput {
                            status,
                            stdout,
                            stderr,
                        })
                    }
                    Err(e) => Err(e),
                }
            } else {
                run_shell_with_token(
                    &cmd,
                    &workdir_path,
                    timeout_secs,
                    Some(&ctx.token),
                    Some(&self.sandbox_config),
                    &self.landlock_extra_paths,
                )
                .await
            };

            match result {
                Ok(output) => {
                    if output.status.success() {
                        // v1.2 phase 21: if the command was a file-dump
                        // (cat, head, tail, etc.) into a known source file,
                        // route the captured stdout through the same
                        // minifier read_file uses. The cache is keyed on
                        // (path, mtime) so this is essentially free when
                        // the model has already called read_file on the
                        // same path earlier in the session.
                        let content = bash_minify::try_minify_bash_output(
                            &cmd,
                            &output.stdout,
                            &self.path_guard,
                        )
                        .unwrap_or(output.stdout);
                        // v1.2 phase 22: if the command was a build
                        // (cargo build/test/check/clippy, rustc) and
                        // produced the canonical cargo progress + warning
                        // output, collapse the noise (compilation
                        // progress lines, repeated warning suggestion
                        // blocks) while keeping all errors and their
                        // context intact. A 400-line `cargo build` log
                        // can typically be reduced to ~50 lines.
                        let content =
                            bash_minify::try_minify_build_log(&cmd, &content).unwrap_or(content);
                        let content = if output.stderr.is_empty() {
                            content
                        } else {
                            format!("{content}\nstderr:\n{}", output.stderr)
                        };
                        ToolOutcome::Success { content }
                    } else if is_timeout_marker(&output, timeout_secs) {
                        // run_shell reports timeouts as a synthetic killed
                        // status with a leading marker in stdout.
                        ToolOutcome::Failure(crate::shared::ToolError::Timeout {
                            after_secs: timeout_secs,
                        })
                    } else {
                        // Error path: stdout is often the *real* signal on a
                        // failing build (rustc prints diagnostics to stdout
                        // with `--message-format=human`, which is the default).
                        // Route it through the same minifiers the success path
                        // uses — they have the same 20%-savings guard, so a
                        // short error message passes through unchanged. Stderr
                        // stays verbatim: it usually contains raw error text
                        // (`error: command not found`, segfault traces) that's
                        // already small and where minification heuristics are
                        // more likely to drop the wrong line.
                        let minified_stdout = bash_minify::try_minify_bash_output(
                            &cmd,
                            &output.stdout,
                            &self.path_guard,
                        )
                        .unwrap_or_else(|| output.stdout.clone());
                        let minified_stdout =
                            bash_minify::try_minify_build_log(&cmd, &minified_stdout)
                                .unwrap_or(minified_stdout);
                        let stderr = if output.stderr.is_empty() {
                            String::new()
                        } else {
                            format!("\nstderr:\n{}", output.stderr)
                        };
                        let exit_code = output.status.code().unwrap_or(-1);
                        ToolOutcome::Failure(crate::shared::ToolError::Execution {
                            message: format!(
                                "Command exited with code {exit_code}\nstdout:\n{minified_stdout}"
                            ),
                            exit_code: Some(exit_code),
                            stderr,
                        })
                    }
                }
                Err(ShellError::Cancelled) => {
                    ToolOutcome::Failure(crate::shared::ToolError::Cancelled)
                }
                Err(e) => ToolOutcome::Failure(crate::shared::ToolError::Execution {
                    message: format!("Failed to execute command: {e}"),
                    exit_code: None,
                    stderr: String::new(),
                }),
            }
        }
    }
}

pub struct BashCancel;

#[async_trait::async_trait]
impl Tool for BashCancel {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash_cancel",
            description: "Cancel a running background bash job by ID. Completed or already-failed jobs are unaffected.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "integer",
                        "description": "The job ID returned by bash with background=true"
                    }
                },
                "required": ["id"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let job_id = match args.get("id").and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'id' argument"));
            }
        };

        let registry = global_registry();
        if registry.cancel(job_id).await {
            ToolOutcome::Success {
                content: format!("Job #{job_id} cancelled"),
            }
        } else {
            match registry.get(job_id).await {
                Some(job) => ToolOutcome::Failure(ToolError::Execution {
                    message: format!("Job #{} is not running (status: {:?})", job_id, job.status),
                    exit_code: None,
                    stderr: String::new(),
                }),
                None => ToolOutcome::Failure(ToolError::Internal {
                    message: format!("Job #{job_id} not found"),
                }),
            }
        }
    }
}

pub struct BashStatus;

#[async_trait::async_trait]
impl Tool for BashStatus {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash_status",
            description: "Check the status of a background bash job by ID. Returns the job's current status (running/completed/failed/cancelled) and any output captured so far.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "integer",
                        "description": "The job ID returned by bash with background=true"
                    }
                },
                "required": ["id"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let job_id = match args.get("id").and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'id' argument"));
            }
        };

        let registry = global_registry();
        match registry.get(job_id).await {
            Some(job) => {
                let status_label = match &job.status {
                    JobStatus::Running => "running",
                    JobStatus::Completed(code) => {
                        return ToolOutcome::Success {
                            content: format!(
                                "Job #{} completed (exit code {})\nstdout:\n{}\nstderr:\n{}",
                                job.id, code, job.stdout, job.stderr
                            ),
                        };
                    }
                    JobStatus::Failed(e) => {
                        return ToolOutcome::Failure(ToolError::Execution {
                            message: format!("Job #{} failed: {}", job.id, e),
                            exit_code: None,
                            stderr: String::new(),
                        });
                    }
                    JobStatus::Cancelled => "cancelled",
                };
                ToolOutcome::Success {
                    content: format!(
                        "Job #{} is {}\ncommand: {}\n---\nstdout so far:\n{}\nstderr so far:\n{}",
                        job.id, status_label, job.command, job.stdout, job.stderr
                    ),
                }
            }
            None => ToolOutcome::Failure(ToolError::Internal {
                message: format!("Job #{job_id} not found"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::shared::test_util::remove_test_file;

    /// A cancelled foreground `Bash` tool invocation returns a structured
    /// `ToolError::Cancelled` and does not leave a long sleep running.
    #[cfg(unix)]
    // WO 27.2-R2: un-ignored after SandboxConfig::default() fix (838e611)
    #[tokio::test]
    async fn bash_tool_respects_cancellation_token() {
        let tmp = std::env::temp_dir();
        let marker = tmp.join(format!("kf_code_bash_cancel_marker_{}", std::process::id()));
        let ready = tmp.join(format!("kf_code_bash_cancel_ready_{}", std::process::id()));
        let marker_str = marker.to_string_lossy().to_string();
        let ready_str = ready.to_string_lossy().to_string();
        remove_test_file(&marker);
        remove_test_file(&ready);

        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let args = serde_json::json!({
            "command": format!("touch {ready_str}; sleep 30; touch {marker_str}"),
            "timeout": 60,
        });

        // WO 38.12: gated-start — poll for the readiness file before
        // firing cancel. The shell touches `ready` immediately on start,
        // so we know the child is running when the file appears. No
        // production readiness signal exists; the readiness file is a
        // test-only technique that avoids the wall-clock race.
        let token = ctx.token.clone();
        let handle = tokio::spawn(async move { tool.run(&ctx, args).await });

        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < ready_deadline {
            if ready.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            ready.exists(),
            "bash child did not signal readiness within 5s"
        );
        token.cancel();

        let outcome = handle.await.expect("tool task should not panic");
        assert!(
            matches!(
                outcome,
                crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Cancelled)
            ),
            "expected Cancelled error, got {outcome:?}"
        );

        // Poll for the marker's absence with a 1s ceiling. A blind 1s sleep
        // delays failure detection; polling fails fast if a surviving
        // descendant touches the marker, and bounds the wait otherwise.
        let grace_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < grace_deadline {
            assert!(
                !marker.exists(),
                "cancelled shell left a surviving descendant"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            !marker.exists(),
            "cancelled shell left a surviving descendant"
        );
        remove_test_file(&ready);
    }

    /// The Bash tool surfaces internal timeouts as a structured
    /// `ToolError::Timeout` rather than an opaque string.
    #[cfg(unix)]
    // WO 27.2-R2: un-ignored after SandboxConfig::default() fix (838e611)
    #[tokio::test]
    async fn bash_tool_surfaces_structured_timeout() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let args = serde_json::json!({
            "command": "sleep 30",
            "timeout": 1,
        });

        let outcome = tool.run(&ctx, args).await;
        assert!(
            matches!(
                outcome,
                crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Timeout {
                    after_secs: 1
                })
            ),
            "expected Timeout error, got {outcome:?}"
        );
    }

    #[cfg(unix)]
    // WO 27.2-R2: un-ignored after SandboxConfig::default() fix (838e611)
    #[tokio::test]
    async fn bash_timeout_clamped_to_max() {
        let bash = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let tmp = std::env::temp_dir();
        let marker = tmp.join(format!("kf_code_bash_huge_timeout_{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);

        let args = serde_json::json!({
            "command": format!("sleep 2; touch {}", marker.to_string_lossy()),
            "timeout": u64::MAX,
        });

        // Should not panic on Duration overflow; with the clamp it will run
        // for 2 seconds and succeed.
        let ctx = crate::tools::ToolContext::new();
        let result = bash.run(&ctx, args).await;
        assert!(
            matches!(result, crate::shared::ToolOutcome::Success { .. }),
            "huge timeout should be clamped and not panic, got {result:?}"
        );
        assert!(
            marker.exists(),
            "command should have completed and touched marker"
        );
        let _ = std::fs::remove_file(&marker);
    }

    #[tokio::test]
    async fn bash_dry_run_does_not_execute_command() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("marker.txt");
        let marker_str = marker.to_string_lossy().to_string();

        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::with_dry_run(true);
        let args = serde_json::json!({
            "command": format!("touch {marker_str}"),
        });

        let outcome = tool.run(&ctx, args).await;
        assert!(
            matches!(outcome, crate::shared::ToolOutcome::Success { ref content } if content.contains("Dry run") && content.contains("touch")),
            "expected dry-run success, got {outcome:?}"
        );
        assert!(
            !marker.exists(),
            "dry-run bash must not execute the command"
        );
    }

    #[tokio::test]
    async fn bash_dry_run_still_blocks_dangerous_command() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::with_dry_run(true);
        let args = serde_json::json!({
            "command": "rm -rf /",
        });

        let outcome = tool.run(&ctx, args).await;
        assert!(
            matches!(
                outcome,
                crate::shared::ToolOutcome::Failure(crate::shared::ToolError::AccessDenied { .. })
            ),
            "expected dry-run access-denied error, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn bash_dry_run_includes_workdir_and_timeout() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::with_dry_run(true);
        let args = serde_json::json!({
            "command": "echo hello",
            "workdir": ".",
            "timeout": 42,
        });

        let outcome = tool.run(&ctx, args).await;
        let content = match outcome {
            crate::shared::ToolOutcome::Success { content } => content,
            other => panic!("expected dry-run success, got {other:?}"),
        };
        assert!(
            content.contains("workdir:"),
            "dry-run output should include workdir: {content}"
        );
        assert!(
            content.contains("timeout: 42s"),
            "dry-run output should include timeout: {content}"
        );
    }

    // WO 33.14 phase 3: the arg-vector construction path is now unit-tested
    // in-process via the `build_docker_args_*` tests (no Docker daemon
    // needed). This remains the 1 real-Docker smoke test covering the full
    // spawn → container → capture flow; the DockerRunner-trait injection
    // upgrade path is no longer the next step for arg coverage.
    #[ignore = "requires Docker installed and running; real-Docker smoke test"]
    #[tokio::test]
    async fn bash_docker_executes_command_in_container() {
        let docker_cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            Some(docker_cfg),
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let args = serde_json::json!({
            "command": "echo hello",
            "timeout": 30,
        });

        let outcome = tool.run(&ctx, args).await;
        match outcome {
            crate::shared::ToolOutcome::Success { content } => {
                assert!(
                    content.contains("hello"),
                    "docker echo should output 'hello', got: {content}"
                );
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bash_missing_command_arg_is_invalid_args() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let outcome = tool.run(&ctx, serde_json::json!({})).await;
        match outcome {
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::InvalidArgs {
                message,
            }) => assert!(message.contains("Missing 'command'"), "got {message}"),
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    // WO 15.3: the Docker path must route `cmd` through
    // `check_bash_command_str` even though the foreground path already does.
    // A dangerous model-supplied command must be denied before docker spawn.
    #[tokio::test]
    async fn bash_docker_path_blocks_dangerous_command() {
        let docker_cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            Some(docker_cfg),
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        // `rm -rf /` is in DANGEROUS_SHELL_COMMANDS and must be denied before
        // docker is ever spawned — so this test does NOT require Docker to be
        // installed or running.
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({
                    "command": "rm -rf /",
                }),
            )
            .await;
        match outcome {
            // WO 47.18: the foreground-branch gate in Bash::run now fires
            // before run_docker is ever reached, so the denial surfaces as
            // AccessDenied instead of the Execution error run_docker's own
            // gate used to return. Still denied before docker spawn — this
            // test does NOT require Docker to be installed or running.
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::AccessDenied {
                message,
            }) => assert!(
                message.contains("Command blocked") || message.contains("rm -rf"),
                "expected deny-list message, got {message}"
            ),
            other => panic!("expected AccessDenied from denied cmd, got {other:?}"),
        }
    }

    // WO 47.18: the foreground (non-docker, non-background) path must deny
    // dangerous commands inside the tool itself — direct tool.run() callers
    // (workflow `tool:` steps, plugins, /replay) bypass pre_run, which is
    // where this gate used to live exclusively.
    #[tokio::test]
    async fn bash_foreground_path_blocks_dangerous_command() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let outcome = tool
            .run(&ctx, serde_json::json!({"command": "rm -rf /"}))
            .await;
        match outcome {
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::AccessDenied {
                message,
            }) => assert!(
                message.contains("Command blocked") || message.contains("rm -rf"),
                "expected deny-list message, got {message}"
            ),
            other => panic!("expected AccessDenied from denied cmd, got {other:?}"),
        }
    }

    // WO 15.3: the Docker path must reject a workdir containing `:` so the
    // `-v SRC:/work` bind-mount string can't be split into extra mount opts.
    // The workdir is canonicalized first, so we need a real path whose
    // canonical form contains `:` — impossible on Unix. Instead we assert
    // the guard fires on a non-canonicalizable path (the canonicalize-err
    // branch) and on a path that resolves to one containing `:` by pointing
    // at a temp file whose name includes `:` (legal on Unix as a filename).
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_docker_path_rejects_workdir_with_colon() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a directory whose name contains ':' — legal on Unix, and
        // its canonical path string will contain ':' which would inject
        // Docker mount options if not sanitized.
        let evil = tmp.path().join("evil:etc:ro");
        std::fs::create_dir(&evil).unwrap();
        let docker_cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            Some(docker_cfg),
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({
                    "command": "echo hello",
                    "workdir": evil.to_string_lossy(),
                }),
            )
            .await;
        match outcome {
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Execution {
                message,
                ..
            }) => assert!(
                message.contains("docker workdir contains ':'")
                    || message.contains("mount options"),
                "expected colon-injection guard message, got {message}"
            ),
            other => panic!("expected Execution failure for ':' workdir, got {other:?}"),
        }
    }

    // M20: a symlink inside the project workdir pointing outside the project
    // root must be rejected — without this check it would canonicalize to
    // e.g. /etc and get mounted read-write into the container.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_docker_path_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a real directory for the sandbox root.
        let sandbox = tmp.path().join("project");
        std::fs::create_dir(&sandbox).unwrap();
        // Create a symlink inside the sandbox that points outside.
        let link = sandbox.join("escape_link");
        std::os::unix::fs::symlink("/etc", &link).unwrap();

        let docker_cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let guard = PathGuard {
            sandbox_dir: Some(sandbox.clone()),
            ..PathGuard::default()
        };
        let tool = Bash::new(
            DenyList::default(),
            guard,
            false,
            Some(docker_cfg),
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({
                    "command": "echo hello",
                    "workdir": link.to_string_lossy(),
                }),
            )
            .await;
        match outcome {
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Execution {
                message,
                ..
            }) => assert!(
                message.contains("escapes project root") || message.contains("outside"),
                "expected symlink-escape guard message, got {message}"
            ),
            other => panic!("expected Execution failure for symlink escape, got {other:?}"),
        }
    }

    // WO 15.3: a non-existent workdir must be rejected before docker spawn
    // (the canonicalize-err branch of the bind-mount sanitize).
    #[tokio::test]
    async fn bash_docker_path_rejects_unresolvable_workdir() {
        let docker_cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            Some(docker_cfg),
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({
                    "command": "echo hello",
                    "workdir": "/tmp/kf-code-nonexistent-xyz-123/nope",
                }),
            )
            .await;
        match outcome {
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Execution {
                message,
                ..
            }) => assert!(
                message.contains("docker workdir cannot be resolved"),
                "expected canonicalize-err message, got {message}"
            ),
            other => panic!("expected Execution failure for unresolvable workdir, got {other:?}"),
        }
    }

    // WO 15.10 (bucketlist 2.15): `run_docker` previously did
    // `.expect("docker_config is Some")` on the `Option<DockerConfig>`.
    // The only caller guards with `docker_config.as_ref().map_or(false,
    // |c| c.enabled)`, so the expect could not fire in production — but
    // the invariant was implicit and a future caller would panic the
    // runtime. This test calls `run_docker` directly (it's private, so
    // the test lives in the same module) on a `Bash` with
    // `docker_config: None` and asserts an `Err(ShellError::Spawn)`
    // with a "docker_config" message rather than a panic.
    #[tokio::test]
    async fn bash_run_docker_returns_spawn_err_when_docker_config_none() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let token = tokio_util::sync::CancellationToken::new();
        let workdir = std::path::Path::new("/tmp");
        let result = tool.run_docker("echo hello", workdir, 30, &token).await;
        match result {
            Err(ShellError::Spawn(msg)) => assert!(
                msg.contains("docker_config"),
                "expected a docker_config message, got: {msg}"
            ),
            other => panic!("expected Err(ShellError::Spawn), got {other:?}"),
        }
    }

    // mm-H18 (WO 47.32): the docker timeout branch must emit a
    // timeout-shaped result — killed exit code + the exact stdout marker
    // prefix — so Bash::run maps it to ToolError::Timeout via
    // is_timeout_marker, not a generic failure or Cancelled. Pure-fn test:
    // post-spawn docker wiring has no daemon-free seam (the real-Docker
    // smoke test above is #[ignore]d).
    #[cfg(unix)]
    #[test]
    fn docker_timeout_result_is_timeout_marker_shaped() {
        let (code, stdout, stderr) = docker_timeout_result(
            5,
            (b"partial out".to_vec(), 4096),
            (b"partial err".to_vec(), 0),
        );
        assert_eq!(code, 9, "killed exit code expected, got {code}");
        assert!(
            stdout.starts_with("[timed out after 5 seconds]\n"),
            "marker prefix expected, got {stdout:?}"
        );
        assert!(
            stdout.contains("partial out"),
            "capped partial stdout expected, got {stdout:?}"
        );
        assert!(
            stdout.contains("[...truncated: 4096 bytes omitted"),
            "truncation marker expected, got {stdout:?}"
        );
        assert_eq!(stderr, "partial err");
        // End-to-end shape: the ShellOutput Bash::run synthesizes from
        // this tuple must be recognized by the same timeout detector the
        // non-docker path uses.
        use std::os::unix::process::ExitStatusExt;
        let output = ShellOutput {
            status: std::process::ExitStatus::from_raw(code),
            stdout,
            stderr: String::new(),
        };
        assert!(is_timeout_marker(&output, 5));
    }

    #[tokio::test]
    async fn bash_non_string_command_arg_is_invalid_args() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let outcome = tool.run(&ctx, serde_json::json!({"command": 123})).await;
        assert!(
            matches!(
                outcome,
                crate::shared::ToolOutcome::Failure(crate::shared::ToolError::InvalidArgs { .. })
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn bash_timeout_clamped_to_max_value() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::with_dry_run(true);
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({
                    "command": "echo hi",
                    "timeout": u64::MAX,
                }),
            )
            .await;
        let content = match outcome {
            crate::shared::ToolOutcome::Success { content } => content,
            other => panic!("expected dry-run Success, got {other:?}"),
        };
        assert!(
            content.contains(&format!("timeout: {MAX_BASH_TIMEOUT_SECS}s")),
            "expected timeout clamped to max, got: {content}"
        );
    }

    #[tokio::test]
    async fn bash_dry_run_uses_explicit_workdir() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::with_dry_run(true);
        let tmp = tempfile::tempdir().unwrap();
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({
                    "command": "echo hello",
                    "workdir": tmp.path().to_string_lossy(),
                }),
            )
            .await;
        let content = match outcome {
            crate::shared::ToolOutcome::Success { content } => content,
            other => panic!("expected dry-run Success, got {other:?}"),
        };
        assert!(
            content.contains(tmp.path().to_string_lossy().as_ref()),
            "dry-run should include the expanded workdir: {content}"
        );
    }

    #[test]
    fn bash_def_has_correct_name_and_required_command() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let def = tool.def();
        assert_eq!(def.name, "bash");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("command")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_nonexistent_command_returns_execution_failure() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({"command": "this_binary_does_not_exist_xyz"}),
            )
            .await;
        match outcome {
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Execution { .. }) => {}
            other => panic!("expected Execution failure, got {other:?}"),
        }
    }

    #[cfg(unix)]
    // WO 27.2-R2: un-ignored after SandboxConfig::default() fix (838e611)
    #[tokio::test]
    async fn bash_failing_command_reports_nonzero_exit_code() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let outcome = tool
            .run(&ctx, serde_json::json!({"command": "exit 7"}))
            .await;
        match outcome {
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Execution {
                exit_code,
                ..
            }) => assert_eq!(exit_code, Some(7), "got {exit_code:?}"),
            other => panic!("expected Execution failure, got {other:?}"),
        }
    }

    /// When `harden` is true and the child exceeds `cpu_limit_secs`, the
    /// kernel sends SIGXCPU and the bash tool surfaces a non-zero exit.
    ///
    /// We set a 1-second CPU limit and run an infinite loop in the
    /// child shell. The child should be killed by SIGXCPU (which
    /// escalates to SIGKILL after a one-second grace period) within a
    /// few seconds of wall-clock time — well under the 30-second
    /// tool timeout. The test verifies the rlimit fired by checking
    /// that the child did NOT run for the full 30-second tool timeout
    /// (which would mean the rlimit was ignored) AND that the outcome
    /// is a failure.
    ///
    /// The test is `#[ignore]` by default because it relies on
    /// `setrlimit` behaviour that is only meaningful on a real Unix
    /// host with a sane scheduler. Run with `cargo test -- --ignored`
    /// to exercise it.
    #[cfg(unix)]
    #[ignore = "requires setrlimit and a real CPU burn"]
    #[tokio::test]
    async fn bash_harden_kills_cpu_burn_with_sigxcpu() {
        let sandbox = crate::shared::SandboxConfig {
            harden: true,
            no_network: false,
            block_edits: false,
            accept_unsandboxed: false,
            cpu_limit_secs: 1,
            memory_limit_mb: 2048,
            filesize_limit_mb: 512,
        };
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            sandbox,
        );
        let ctx = crate::tools::ToolContext::new();
        let args = serde_json::json!({
            "command": "while :; do :; done",
            "timeout": 30,
        });

        let start = std::time::Instant::now();
        let outcome = tool.run(&ctx, args).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(25),
            "child ran for {elapsed:?} — rlimit did not fire (expected SIGXCPU within ~2s)"
        );

        match outcome {
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Execution {
                exit_code,
                ..
            }) => {
                assert!(
                    exit_code.is_none() || exit_code == Some(-1),
                    "expected signal-killed (None or -1), got {exit_code:?}"
                );
            }
            other => panic!("expected Failure from SIGXCPU, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bash_background_with_denied_command_returns_internal_failure() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let args = serde_json::json!({
            "command": "rm -rf /",
            "background": true,
        });
        let outcome = tool.run(&ctx, args).await;
        match outcome {
            crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Internal { message }) => {
                assert!(
                    message.contains("Failed to start background job"),
                    "got {message}"
                );
            }
            other => panic!("expected Internal failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bash_background_with_safe_command_starts_job() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::new();
        let args = serde_json::json!({
            "command": "echo hello",
            "background": true,
        });
        let outcome = tool.run(&ctx, args).await;
        let content = match outcome {
            crate::shared::ToolOutcome::Success { content } => content,
            other => panic!("expected Success, got {other:?}"),
        };
        assert!(
            content.contains("Background job #") && content.contains("started"),
            "got {content}"
        );
    }

    #[tokio::test]
    async fn bash_dry_run_with_background_flag_does_not_start_job() {
        let tool = Bash::new(
            DenyList::default(),
            PathGuard::default(),
            false,
            None,
            crate::shared::SandboxConfig::default(),
        );
        let ctx = crate::tools::ToolContext::with_dry_run(true);
        let args = serde_json::json!({
            "command": "echo hello",
            "background": true,
        });
        let outcome = tool.run(&ctx, args).await;
        match outcome {
            crate::shared::ToolOutcome::Success { content } => {
                assert!(
                    content.contains("Dry run"),
                    "dry-run should win over background flag, got {content}"
                );
            }
            other => panic!("expected dry-run Success, got {other:?}"),
        }
    }

    // The next six tests pin build_docker_args — the pure arg-vector
    // construction extracted from run_docker. They run in-process with no
    // Docker daemon, closing the gap where the arg vector was previously
    // only exercised by the #[ignore]d real-Docker smoke test.

    #[test]
    fn build_docker_args_includes_image_and_command() {
        let cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let workdir = std::path::Path::new("/tmp/kf-code-test");
        let args = build_docker_args(&cfg, workdir, "echo hello", 30);
        assert!(
            args.iter().any(|a| a == "alpine:latest"),
            "image name should be in the arg vector, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "echo hello"),
            "command should be in the arg vector, got {args:?}"
        );
    }

    #[test]
    fn build_docker_args_includes_memory_and_cpus_limits() {
        let cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let workdir = std::path::Path::new("/tmp/kf-code-test");
        let args = build_docker_args(&cfg, workdir, "echo hi", 30);
        let mem = args.windows(2).find(|w| w[0] == "--memory");
        assert_eq!(
            mem.map(|w| w[1].as_str()),
            Some("512m"),
            "--memory should be followed by the configured limit, got {args:?}"
        );
        let cpus = args.windows(2).find(|w| w[0] == "--cpus");
        assert_eq!(
            cpus.map(|w| w[1].as_str()),
            Some("1"),
            "--cpus should be followed by the configured limit, got {args:?}"
        );
    }

    #[test]
    fn build_docker_args_includes_bind_mount() {
        let cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let workdir = std::path::Path::new("/tmp/kf-code-test");
        let args = build_docker_args(&cfg, workdir, "echo hi", 30);
        let mount = args
            .windows(2)
            .find(|w| w[0] == "-v")
            .map(|w| w[1].as_str());
        assert_eq!(
            mount,
            Some("/tmp/kf-code-test:/work"),
            "-v should bind-mount the host workdir at /work, got {args:?}"
        );
    }

    #[test]
    fn build_docker_args_includes_rm_flag() {
        let cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let workdir = std::path::Path::new("/tmp/kf-code-test");
        let args = build_docker_args(&cfg, workdir, "echo hi", 30);
        assert!(
            args.iter().any(|a| a == "--rm"),
            "--rm (auto-cleanup) should be present, got {args:?}"
        );
    }

    // Pins the timeout contract: the timeout is enforced by run_docker via
    // a tokio::time::sleep select branch, NOT as a Docker flag. If a future
    // change moves the timeout into the Docker arg vector, this test must
    // be updated deliberately — a silent drift would be a regression.
    #[test]
    fn build_docker_args_includes_timeout() {
        let cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        let workdir = std::path::Path::new("/tmp/kf-code-test");
        let args = build_docker_args(&cfg, workdir, "echo hi", 42);
        assert!(
            !args.iter().any(|a| a.contains("42")),
            "timeout_secs must NOT appear as a Docker flag — it is a runtime wrapper, got {args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|a| a == "--timeout" || a.starts_with("--timeout=")),
            "no --timeout flag should be emitted, got {args:?}"
        );
    }

    // Pins that build_docker_args receives the already-canonicalized path
    // and uses it verbatim as the bind-mount source. run_docker canonicalizes
    // before calling; this test ensures the contract holds: a canonical path
    // passed in appears unchanged in the -v arg.
    #[test]
    fn build_docker_args_workdir_is_canonicalized() {
        let cfg = DockerConfig {
            enabled: true,
            image: "alpine:latest".into(),
            memory: "512m".into(),
            cpus: "1".into(),
        };
        // Use a tempdir so the canonical path is real and resolvable.
        let tmp = tempfile::tempdir().unwrap();
        let canon = tmp
            .path()
            .canonicalize()
            .expect("tempdir should canonicalize");
        let args = build_docker_args(&cfg, &canon, "echo hi", 30);
        let expected_mount = format!("{}:/work", canon.to_string_lossy());
        assert!(
            args.iter().any(|a| a == &expected_mount),
            "bind-mount source should be the canonical path verbatim, expected {expected_mount}, got {args:?}"
        );
    }
}
