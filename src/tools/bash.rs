use crate::session::access::{DenyList, PathGuard};
use crate::session::bash_jobs::global_registry;
use crate::session::bash_runner::{
    check_bash_command_str, is_timeout_marker, run_shell_with_token, ShellError,
};
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::bash_minify;
use crate::tools::{Tool, ToolContext};
use std::path::PathBuf;

pub struct Bash {
    deny_list: DenyList,
    path_guard: PathGuard,
    bash_sandbox_workdir: bool,
}

impl Bash {
    pub fn new(deny_list: DenyList, path_guard: PathGuard, bash_sandbox_workdir: bool) -> Self {
        Self {
            deny_list,
            path_guard,
            bash_sandbox_workdir,
        }
    }
}

#[async_trait::async_trait]
impl Tool for Bash {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash",
            description: "Execute a bash command. Use for running tests, builds, git operations, and file inspection. Output is captured and returned. Set \"background\": true to run long-lived commands in the background.",
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
                        "description": "Run in background. Use bash_status to check and bash_output to retrieve results.",
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

        let timeout_secs = args.get("timeout").and_then(|t| t.as_u64()).unwrap_or(30);
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
            let timeout = args.get("timeout").and_then(|t| t.as_u64());
            match registry
                .spawn(
                    &cmd,
                    workdir,
                    timeout,
                    &self.deny_list,
                    &self.path_guard,
                    self.bash_sandbox_workdir,
                )
                .await
            {
                Ok(id) => ToolOutcome::Success {
                    content: format!("Background job #{id} started. Use bash_status(id={id}) or bash_output(id={id}) to check results."),
                },
                Err(e) => ToolOutcome::Failure(crate::shared::ToolError::internal(format!(
                    "Failed to start background job: {e}"
                ))),
            }
        } else {
            // Normal foreground execution
            let result =
                run_shell_with_token(&cmd, &workdir_path, timeout_secs, Some(&ctx.token)).await;

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
                        let content = bash_minify::try_minify_bash_output(&cmd, &output.stdout)
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
                        let minified_stdout =
                            bash_minify::try_minify_bash_output(&cmd, &output.stdout)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::remove_test_file;

    /// A cancelled foreground `Bash` tool invocation returns a structured
    /// `ToolError::Cancelled` and does not leave a long sleep running.
    #[tokio::test]
    async fn bash_tool_respects_cancellation_token() {
        let tmp = std::env::temp_dir();
        let marker = tmp.join(format!(
            "kirkforge_bash_cancel_marker_{}",
            std::process::id()
        ));
        let marker_str = marker.to_string_lossy().to_string();
        remove_test_file(&marker);

        let tool = Bash::new(DenyList::default(), PathGuard::default(), false);
        let ctx = crate::tools::ToolContext::new();
        let args = serde_json::json!({
            "command": format!("sleep 30; touch {marker_str}"),
            "timeout": 60,
        });

        // Start the tool in a background task and cancel the token shortly
        // after. We don't await the tool directly because we want to drive
        // cancellation from outside.
        let token = ctx.token.clone();
        let handle = tokio::spawn(async move { tool.run(&ctx, args).await });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        token.cancel();

        let outcome = handle.await.expect("tool task should not panic");
        assert!(
            matches!(
                outcome,
                crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Cancelled)
            ),
            "expected Cancelled error, got {outcome:?}"
        );

        // Give any surviving descendant a short window to touch the marker.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        assert!(
            !marker.exists(),
            "cancelled shell left a surviving descendant"
        );
    }

    /// The Bash tool surfaces internal timeouts as a structured
    /// `ToolError::Timeout` rather than an opaque string.
    #[tokio::test]
    async fn bash_tool_surfaces_structured_timeout() {
        let tool = Bash::new(DenyList::default(), PathGuard::default(), false);
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

    #[tokio::test]
    async fn bash_dry_run_does_not_execute_command() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("marker.txt");
        let marker_str = marker.to_string_lossy().to_string();

        let tool = Bash::new(DenyList::default(), PathGuard::default(), false);
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
        let tool = Bash::new(DenyList::default(), PathGuard::default(), false);
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
        let tool = Bash::new(DenyList::default(), PathGuard::default(), false);
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
        assert!(content.contains("workdir:"), "dry-run output should include workdir: {content}");
        assert!(
            content.contains("timeout: 42s"),
            "dry-run output should include timeout: {content}"
        );
    }
}
