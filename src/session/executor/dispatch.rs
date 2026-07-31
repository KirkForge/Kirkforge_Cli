//! Tool-call dispatch and verifier correction emission.

use crate::session::event_bus::BusEvent;
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
        // The rendered diff from the edit_file tool, when the call
        // succeeded. Used as the `EditEvent.diff` payload so downstream
        // consumers (event-bus handlers, correction loop) see the
        // real unified diff rather than the user's `old_string`
        // (which was what the old code passed — see GPT 5.5
        // review finding #9). `None` for any other tool or for a
        // failed edit; the `args.old_string` fallback inside the
        // match keeps the event populated for the failure case.
        edit_diff: Option<String>,
    ) -> Vec<CorrectionResult> {
        let bus_event = match tool_name {
            "read_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(BusEvent::FileRead(
                    crate::session::event_bus::FileReadEvent {
                        path: std::path::PathBuf::from(&path),
                        size_bytes: 0,
                        truncated: false,
                    },
                ))
            }
            "write_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                // Content-address the write so two same-length writes to the
                // same path (a real re-write vs. a duplicate dispatch) don't
                // share an idem key (WO 15.8 / 2.6).
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                content.hash(&mut hasher);
                Some(BusEvent::FileWrite(
                    crate::session::event_bus::FileWriteEvent {
                        path: std::path::PathBuf::from(&path),
                        content_length: content.len(),
                        content_hash: hasher.finish(),
                    },
                ))
            }
            "edit_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Prefer the rendered diff returned by the tool (the
                // "happy path"); fall back to the user's old_string
                // when the edit failed (no real diff exists) so the
                // event still carries something useful for debugging.
                let diff = edit_diff.unwrap_or_else(|| {
                    args.get("old_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                });
                Some(BusEvent::Edit(crate::session::event_bus::EditEvent {
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
                Some(BusEvent::BashExec(
                    crate::session::event_bus::BashExecEvent {
                        command,
                        exit_code: real_exit_code.unwrap_or(0),
                        stdout_len: real_stdout_len.unwrap_or(0),
                        stderr_len: real_stderr_len.unwrap_or(0),
                        workdir,
                    },
                ))
            }
            _ => None,
        };

        let error_event = match outcome {
            ToolOutcome::Error { message } => Some(BusEvent::ToolError(
                crate::session::event_bus::ToolErrorEvent {
                    tool: tool_name.to_string(),
                    error: message.clone(),
                },
            )),
            ToolOutcome::Failure(err) => Some(BusEvent::ToolError(
                crate::session::event_bus::ToolErrorEvent {
                    tool: tool_name.to_string(),
                    error: err.to_user_message(),
                },
            )),
            _ => None,
        };

        let mut corrections = Vec::new();

        if let Some(ref event) = bus_event {
            let handler_results = self.event_bus.dispatch(event).await;
            for r in handler_results {
                if !r.success {
                    tracing::warn!(handler = %r.handler_id, message = %r.message, "event handler failed");
                }
            }
            if let Some(ref correction_loop) = self.correction_loop {
                corrections.extend(correction_loop.run(event).await);
            }
        }

        if let Some(ref event) = error_event {
            let handler_results = self.event_bus.dispatch(event).await;
            for r in handler_results {
                if !r.success {
                    tracing::warn!(handler = %r.handler_id, message = %r.message, "event handler failed");
                }
            }
            if let Some(ref correction_loop) = self.correction_loop {
                corrections.extend(correction_loop.run(event).await);
            }
        }

        // Run the unified verifier bus after file-modifying tool calls.
        // Collect structured VerdictEntrys and inject error verdicts into
        // the correction results so the model sees them.
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
