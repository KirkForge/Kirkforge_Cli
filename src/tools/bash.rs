use crate::session::bash_jobs::global_registry;
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::bash_minify;
use crate::tools::Tool;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Bash;

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

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let cmd = match args.get("command").and_then(|c| c.as_str()) {
            Some(c) => c.to_string(),
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'command' argument".into(),
                }
            }
        };

        // Check for background mode
        if args
            .get("background")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            let registry = global_registry();
            let workdir = args.get("workdir").and_then(|w| w.as_str());
            let timeout = args.get("timeout").and_then(|t| t.as_u64());
            match registry.spawn(&cmd, workdir, timeout).await {
                Ok(id) => ToolOutcome::Success {
                    content: format!("Background job #{} started. Use bash_status(id={}) or bash_output(id={}) to check results.", id, id, id),
                },
                Err(e) => ToolOutcome::Error {
                    message: format!("Failed to start background job: {}", e),
                },
            }
        } else {
            // Normal foreground execution
            let timeout_secs = args.get("timeout").and_then(|t| t.as_u64()).unwrap_or(30);
            let workdir = args.get("workdir").and_then(|w| w.as_str()).unwrap_or(".");

            let workdir_path = PathBuf::from(shellexpand::tilde(workdir).as_ref());

            let result = run_shell(&cmd, &workdir_path, timeout_secs).await;

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
                        let content = bash_minify::try_minify_build_log(&cmd, &content)
                            .unwrap_or(content);
                        ToolOutcome::Success { content }
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
                        let minified_stdout = bash_minify::try_minify_bash_output(&cmd, &output.stdout)
                            .unwrap_or_else(|| output.stdout.clone());
                        let minified_stdout = bash_minify::try_minify_build_log(&cmd, &minified_stdout)
                            .unwrap_or(minified_stdout);
                        let stderr = if output.stderr.is_empty() {
                            String::new()
                        } else {
                            format!("\nstderr:\n{}", output.stderr)
                        };
                        ToolOutcome::Error {
                            message: format!(
                                "Command exited with code {}{}\nstdout:\n{}",
                                output.status.code().unwrap_or(-1),
                                stderr,
                                minified_stdout
                            ),
                        }
                    }
                }
                Err(e) => ToolOutcome::Error {
                    message: format!("Failed to execute command: {}", e),
                },
            }
        }
    }
}

struct ShellOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

/// Run a shell command in the foreground with kill_on_drop and timeout.
/// Uses tokio::process::Command so the child is killed on timeout or drop.
async fn run_shell(cmd: &str, workdir: &Path, timeout_secs: u64) -> Result<ShellOutput, String> {
    let mut proc = tokio::process::Command::new("/bin/sh");
    proc.arg("-c")
        .arg(cmd)
        .current_dir(workdir)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = tokio::time::timeout(Duration::from_secs(timeout_secs), proc.output())
        .await
        .map_err(|_| format!("Command timed out after {} seconds", timeout_secs))?
        .map_err(|e| format!("Failed to execute command: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok(ShellOutput {
        status: output.status,
        stdout,
        stderr,
    })
}
