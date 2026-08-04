//! Tool-call dispatch and verifier correction emission.

use crate::session::verifier::CorrectionResult;
use crate::shared::{ToolInvocation, ToolOutcome};

use super::Executor;

impl Executor {
    // reason: bash metrics (exit/stdout/stderr) + edit diff are independent optional payloads.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn emit_tool_event_and_correct(
        &self,
        _tc: &ToolInvocation,
        tool_name: &str,
        args: &serde_json::Value,
        outcome: &ToolOutcome,
        real_exit_code: Option<i32>,
        real_stdout_len: Option<usize>,
        real_stderr_len: Option<usize>,
        edit_diff: Option<String>,
    ) -> Vec<CorrectionResult> {
        use crate::session::verifier::types::*;

        let bus_event = match tool_name {
            "read_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(BusEvent::FileRead(FileReadEvent {
                    path: std::path::PathBuf::from(&path),
                    size_bytes: 0,
                    truncated: false,
                }))
            }
            "write_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                content.hash(&mut hasher);
                Some(BusEvent::FileWrite(FileWriteEvent {
                    path: std::path::PathBuf::from(&path),
                    content_length: content.len(),
                    content_hash: hasher.finish(),
                }))
            }
            "edit_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let diff = edit_diff.unwrap_or_else(|| {
                    args.get("old_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                });
                Some(BusEvent::Edit(EditEvent {
                    path: std::path::PathBuf::from(&path),
                    diff,
                }))
            }
            "bash" => {
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let workdir = args
                    .get("workdir")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from);
                Some(BusEvent::BashExec(BashExecEvent {
                    command,
                    exit_code: real_exit_code.unwrap_or(0),
                    stdout_len: real_stdout_len.unwrap_or(0),
                    stderr_len: real_stderr_len.unwrap_or(0),
                    workdir,
                }))
            }
            _ => None,
        };

        let error_event = match outcome {
            ToolOutcome::Error { message } => Some(BusEvent::ToolError(ToolErrorEvent {
                tool: tool_name.to_string(),
                error: message.clone(),
            })),
            ToolOutcome::Failure(err) => Some(BusEvent::ToolError(ToolErrorEvent {
                tool: tool_name.to_string(),
                error: err.to_user_message(),
            })),
            _ => None,
        };

        let mut corrections = Vec::new();

        if let Some(ref event) = bus_event {
            if let Some(ref correction_loop) = self.correction_loop {
                corrections.extend(correction_loop.run(event).await);
            }
        }

        if let Some(ref event) = error_event {
            if let Some(ref correction_loop) = self.correction_loop {
                corrections.extend(correction_loop.run(event).await);
            }
        }

        // Run the unified verifier bus after file-modifying tool calls.
        let is_file_modification = matches!(tool_name, "write_file" | "edit_file");
        if is_file_modification {
            if let Some(ref bus_lock) = self.verifier_bus {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let ctx = crate::session::verifier::bus::VerifyContext {
                    sandbox_dir: self.path_guard.sandbox_dir.clone().unwrap_or_default(),
                    changed_files: vec![std::path::PathBuf::from(path)],
                };
                if let Ok(mut bus) = bus_lock.lock() {
                    bus.run(&ctx);
                    for entry in bus.verdicts() {
                        if entry.severity == crate::session::verifier::bus::Severity::Error {
                            corrections.push(CorrectionResult {
                                verifier: format!("{}", entry.source),
                                success: false,
                                message: format!("[{}] {}", entry.source, entry.message),
                                fix: None,
                            });
                        }
                    }
                    bus.clear();
                }
            }
        }

        corrections
    }
}
