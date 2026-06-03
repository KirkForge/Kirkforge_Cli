use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::Tool;
use std::time::Duration;
use std::path::PathBuf;

pub struct Bash;

#[async_trait::async_trait]
impl Tool for Bash {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash",
            description: "Execute a bash command. Use for running tests, builds, git operations, and file inspection. Output is captured and returned.",
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
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let cmd = match args.get("command").and_then(|c| c.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutcome::Error { message: "Missing 'command' argument".into() },
        };

        let timeout_secs = args.get("timeout").and_then(|t| t.as_u64()).unwrap_or(30);
        let workdir = args.get("workdir").and_then(|w| w.as_str()).unwrap_or(".");

        let workdir_path = PathBuf::from(shellexpand::tilde(workdir).as_ref());

        // Determine if this is likely read-only
        let is_readonly = is_readonly_command(&cmd);

        // Run the command
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            run_shell(&cmd, &workdir_path),
        ).await;

        match result {
            Ok(Ok(output)) => {
                if output.status.success() {
                    ToolOutcome::Success { content: output.stdout }
                } else {
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
                            output.stdout
                        ),
                    }
                }
            }
            Ok(Err(e)) => ToolOutcome::Error {
                message: format!("Failed to execute command: {}", e),
            },
            Err(_) => ToolOutcome::Error {
                message: format!("Command timed out after {} seconds", timeout_secs),
            },
        }
    }
}

struct ShellOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

async fn run_shell(cmd: &str, workdir: &PathBuf) -> std::io::Result<ShellOutput> {
    // Spawn on a blocking thread to avoid blocking the async runtime
    let cmd = cmd.to_string();
    let workdir = workdir.clone();

    tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(&workdir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        Ok(ShellOutput {
            status: output.status,
            stdout,
            stderr,
        })
    }).await.expect("spawn_blocking panicked")
}

/// Heuristic: detect read-only commands so the approval gate can auto-pass.
/// This is not a security boundary — it's a UX optimization.
fn is_readonly_command(cmd: &str) -> bool {
    let trimmed = cmd.trim();

    let read_only_prefixes = [
        "ls ", "cat ", "head ", "tail ", "echo ", "printf ", "pwd",
        "git status", "git log", "git diff", "git show", "git branch",
        "grep ", "find ", "wc ", "sort ", "uniq ", "which ",
        "cargo check", "cargo test", "npm test", "npm run",
    ];

    for prefix in read_only_prefixes {
        if trimmed.starts_with(prefix) {
            return true;
        }
    }

    false
}