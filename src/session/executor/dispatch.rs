//! Tool-call dispatch and verifier correction emission.

use crate::session::access::GuardVerdict;
use crate::session::bash_runner::check_bash_command_str;
use crate::session::event_bus::BusEvent;
use crate::session::toolset::Toolset;
use crate::session::verifier::CorrectionResult;
use crate::shared::metrics::{record, MetricEvent};
use crate::shared::permission::{evaluate, PermissionAction};
use crate::shared::{read_shared_config, Message, Role, ToolInvocation, ToolOutcome};
use std::time::Instant;
use tokio::sync::mpsc;

use super::helpers::*;
use super::types::{ApprovalDecision, TurnEvent};
use super::{ApprovalRequest, Executor};

impl Executor {
    pub(crate) async fn dispatch_tool_call(
        &mut self,
        tc: &mut ToolInvocation,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &std::sync::atomic::AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<()> {
        let tool = match self.tools.resolve(&tc.name) {
            Some(t) => t,
            None => {
                let err = format!("Unknown tool: {}", tc.name);
                crate::send_or_warn!(
                    event_tx.send(TurnEvent::Error(err.clone())).await,
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation
                    .append_async(Message {
                        role: Role::Tool,
                        content: err,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })
                    .await?;
                return Ok(());
            }
        };

        // Plan-mode enforcement: only read-only discovery tools may run.
        // This is a hard guard independent of permission rules so the
        // model cannot mutate code while it is still "thinking".
        if self.plan_mode {
            let allowed = match tc.name.as_str() {
                "read_file" | "read_image" | "grep" | "glob" => true,
                // Job-status queries are read-only and useful while planning.
                "bash_status" | "bash_cancel" => true,
                "bash" => tc
                    .arguments
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(is_read_only_bash)
                    .unwrap_or(false),
                _ => false,
            };
            if !allowed {
                let reason = format!(
                    "📐 Plan mode blocked {}: only read-only discovery tools are allowed until you type /implement.",
                    tc.name
                );
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: reason.clone(),
                            success: false,
                        })
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation
                    .append_async(Message {
                        role: Role::Tool,
                        content: reason,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })
                    .await?;
                return Ok(());
            }
        }

        // Pre-flight schema validation: reject malformed calls before they
        // reach approval or the tool implementation. This is especially
        // important for plugin/MCP tools whose scripts would otherwise fail
        // with obscure shell errors.
        if let Some(reason) = validate_args_against_schema(&tc.arguments, &tool.def().parameters) {
            let err = format!("❌ Invalid arguments for {}: {reason}", tc.name);
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: err.clone(),
                        success: false,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            self.conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: err,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
            return Ok(());
        }

        // Snapshot the permission config so we don't hold the read
        // guard across the mutable self borrows below.
        let (auto_approve, permission_rules) = {
            let cfg = read_shared_config(&self.config);
            (cfg.auto_approve, cfg.permission_rules.clone())
        };
        let is_destructive = matches!(tc.name.as_str(), "write_file" | "edit_file" | "bash");

        // Whether THIS specific bash call is read-only discovery
        // (ls/cat/grep/…). Only meaningful for bash; false otherwise.
        let is_read_only_bash_call = tc.name == "bash"
            && tc
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .map(is_read_only_bash)
                .unwrap_or(false);

        // The DEFAULT action — used ONLY when no permission rule matches.
        // The read-only / auto_approve heuristics live HERE, on the
        // default, so they can never override an explicit user rule.
        let default_action = if !is_destructive || is_read_only_bash_call {
            // Non-destructive tools (read_file/grep/glob/read_image) and
            // read-only discovery bash are governed by the path guard and
            // deny-list, not the approval dialog. They don't prompt by
            // default. An explicit `deny`/`ask` rule (below) still applies.
            PermissionAction::Allow
        } else if auto_approve {
            // auto_approve clears writes/edits, but is NOT a blank cheque
            // for non-read-only bash — that still asks by default.
            if tc.name == "bash" {
                PermissionAction::Ask
            } else {
                PermissionAction::Allow
            }
        } else {
            PermissionAction::Ask
        };

        // First-match-wins rules override the default. An explicit `allow`
        // (e.g. one written by the `[A]lways` key) is honored as-is — it is
        // no longer silently downgraded back to Ask under auto_approve.
        let action = evaluate(&permission_rules, &tc.name, &tc.arguments, default_action);

        // Enforce the decision uniformly for EVERY tool. Previously the
        // checks below were gated on `is_destructive`, which meant `deny`
        // rules on read_file/grep/etc. were silently ignored and `ask`
        // rules never prompted. `default_action` already encodes the safe
        // per-tool defaults, so gate purely on `action`.
        let needs_approval = matches!(action, PermissionAction::Ask);

        if matches!(action, PermissionAction::Deny) {
            let reason = format!(
                "❌ Permission rule denied {}:{}={}",
                tc.name,
                tc.arguments
                    .as_object()
                    .and_then(|o| o.keys().next().map(|s| s.as_str()))
                    .unwrap_or(""),
                tc.arguments
                    .as_object()
                    .and_then(|o| o.values().next())
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            );
            if is_destructive {
                self.audit_log
                    .log_destructive(&tc.name, &tc.arguments, false, Some(&reason));
            }
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: reason.clone(),
                        success: false,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            self.conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: reason,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
            return Ok(());
        }

        if needs_approval {
            match self.run_approval_flow(tc, approval_sender).await? {
                ApprovalDecision::Approved | ApprovalDecision::AlwaysApproved => {}
                ApprovalDecision::Denied { reason } => {
                    let msg = format!("❌ Approval denied: {reason}");
                    if is_destructive {
                        self.audit_log
                            .log_destructive(&tc.name, &tc.arguments, false, Some(&msg));
                    }
                    crate::send_or_warn!(
                        event_tx
                            .send(TurnEvent::ToolResult {
                                name: tc.name.clone(),
                                output: msg.clone(),
                                success: false,
                            })
                            .await,
                        "TurnEvent receiver dropped; discarding event"
                    );
                    self.conversation
                        .append_async(Message {
                            role: Role::Tool,
                            content: msg,
                            tool_call_id: Some(tc.id.clone()),
                            tool_name: Some(tc.name.clone()),
                            ..Default::default()
                        })
                        .await?;
                    return Ok(());
                }
            }
        }

        if let Some(denied) = check_url_in_args(&tc.arguments, &self.deny_list) {
            if is_destructive {
                self.audit_log
                    .log_destructive(&tc.name, &tc.arguments, false, Some(&denied));
            }
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: denied.clone(),
                        success: false,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            self.conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: denied,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
            return Ok(());
        }

        if let Some(denied) = check_deny_list(&self.deny_list, &tc.name, &tc.arguments) {
            if is_destructive {
                self.audit_log
                    .log_destructive(&tc.name, &tc.arguments, false, Some(&denied));
            }
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: denied.clone(),
                        success: false,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            self.conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: denied,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
            return Ok(());
        }

        if matches!(
            tc.name.as_str(),
            "read_file" | "read_image" | "write_file" | "edit_file"
        ) {
            let path_str = tc
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let path = std::path::Path::new(path_str);

            let verdict = if tc.name == "read_file" || tc.name == "read_image" {
                self.path_guard.check_read(path)
            } else if tc.name == "write_file" || tc.name == "edit_file" {
                self.path_guard.check_write(path).await
            } else {
                let denied = format!("🔒 Access denied: unsupported file tool '{}'", tc.name);
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: denied.clone(),
                            success: false,
                        })
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation
                    .append_async(Message {
                        role: Role::Tool,
                        content: denied,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })
                    .await?;
                return Ok(());
            };

            match verdict {
                GuardVerdict::Allowed(resolved) => {
                    // Read-before-edit gate. `edit_file` always needs a prior
                    // read. `write_file` only needs one when it overwrites an
                    // existing file — a brand-new file can't have been read.
                    // Without this, write_file could blindly clobber a file
                    // the model never inspected (review.md High finding).
                    let needs_read_gate =
                        tc.name == "edit_file" || (tc.name == "write_file" && path.exists());
                    if needs_read_gate {
                        if let GuardVerdict::Denied(msg) =
                            self.read_gate.check_edit(path, &resolved)
                        {
                            let denied = format!("🔒 Access denied: {msg}");
                            crate::send_or_warn!(
                                event_tx
                                    .send(TurnEvent::ToolResult {
                                        name: tc.name.clone(),
                                        output: denied.clone(),
                                        success: false,
                                    })
                                    .await,
                                "TurnEvent receiver dropped; discarding event"
                            );
                            self.conversation
                                .append_async(Message {
                                    role: Role::Tool,
                                    content: denied,
                                    tool_call_id: Some(tc.id.clone()),
                                    tool_name: Some(tc.name.clone()),
                                    ..Default::default()
                                })
                                .await?;
                            return Ok(());
                        }
                    }

                    if matches!(tc.name.as_str(), "read_file" | "read_image") {
                        self.read_gate.mark_read(&resolved);
                    }

                    let mut run_args = tc.arguments.clone();
                    if let Ok(path_obj) = serde_json::to_value(resolved.to_string_lossy().as_ref())
                    {
                        if let Some(obj) = run_args.as_object_mut() {
                            obj.insert("path".into(), path_obj);
                        }
                    }

                    crate::send_or_warn!(
                        event_tx
                            .send(TurnEvent::ToolStart {
                                name: tc.name.clone(),
                                args: run_args.clone(),
                            })
                            .await,
                        "TurnEvent receiver dropped; discarding event"
                    );

                    // Pre-tool hook (may deny the operation).
                    let args_json = serde_json::to_string(&run_args).unwrap_or_default();
                    if let Some(reason) = self
                        .run_pre_tool_hook(
                            &format!("pre-tool-{}", tc.name),
                            Some(&tc.name),
                            Some(&args_json),
                        )
                        .await
                    {
                        let denied = format!("❌ Hook denied {}: {}", tc.name, reason);
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::ToolResult {
                                    name: tc.name.clone(),
                                    output: denied.clone(),
                                    success: false,
                                })
                                .await,
                            "TurnEvent receiver dropped; discarding event"
                        );
                        self.conversation
                            .append_async(Message {
                                role: Role::Tool,
                                content: denied,
                                tool_call_id: Some(tc.id.clone()),
                                tool_name: Some(tc.name.clone()),
                                ..Default::default()
                            })
                            .await?;
                        return Ok(());
                    }

                    let ctx = self.tool_context_for_call(cancelled);
                    let timeout = self.tool_call_timeout();
                    let tool_start = Instant::now();
                    let outcome = tokio::time::timeout(timeout, tool.run(&ctx, run_args.clone()))
                        .await
                        .unwrap_or(ToolOutcome::Failure(crate::shared::ToolError::Timeout {
                            after_secs: timeout.as_secs(),
                        }));
                    let tool_duration = tool_start.elapsed();
                    let outcome_for_emit = outcome.clone();
                    let edit_diff =
                        handle_tool_outcome(outcome, tc, event_tx, &mut self.conversation).await?;
                    record(MetricEvent::ToolCall {
                        name: tc.name.clone(),
                        success: tool_outcome_success(&outcome_for_emit),
                        duration_ms: tool_duration.as_millis() as u64,
                        error_kind: tool_error_kind(&outcome_for_emit).map(String::from),
                    });

                    // Post-tool hook
                    self.run_hook(
                        &format!("post-tool-{}", tc.name),
                        Some(&tc.name),
                        Some(&args_json),
                    );

                    let crs = self
                        .emit_tool_event_and_correct(
                            tc,
                            &tc.name,
                            &run_args,
                            &outcome_for_emit,
                            None,
                            None,
                            None,
                            edit_diff,
                        )
                        .await;
                    self.collect_carryover(tc, &crs);
                    emit_correction_results(crs, tc, event_tx, &mut self.conversation).await?;
                    return Ok(());
                }
                GuardVerdict::Denied(msg) => {
                    let denied = format!("🔒 Access denied: {msg}");
                    self.audit_log
                        .log_destructive(&tc.name, &tc.arguments, false, Some(&denied));
                    crate::send_or_warn!(
                        event_tx
                            .send(TurnEvent::ToolResult {
                                name: tc.name.clone(),
                                output: denied.clone(),
                                success: false,
                            })
                            .await,
                        "TurnEvent receiver dropped; discarding event"
                    );
                    self.conversation
                        .append_async(Message {
                            role: Role::Tool,
                            content: denied,
                            tool_call_id: Some(tc.id.clone()),
                            tool_name: Some(tc.name.clone()),
                            ..Default::default()
                        })
                        .await?;
                    return Ok(());
                }
            }
        }

        if tc.name == "bash" {
            // Pre-process: if `bash_sandbox_workdir` is enabled, force
            // the workdir to the sandbox when the model didn't pass one.
            // We mutate `tc.arguments` in place so the actual `tool.run`
            // call (and the pre/post tool hooks) see the override. The
            // check function below rejects an explicit workdir that
            // points outside the sandbox.
            let bash_sandbox_workdir = read_shared_config(&self.config).bash_sandbox_workdir;
            if bash_sandbox_workdir
                && self.path_guard.sandbox_dir.is_some()
                && tc
                    .arguments
                    .get("workdir")
                    .and_then(|w| w.as_str())
                    .map(|s| s.is_empty())
                    .unwrap_or(true)
            {
                if let Some(obj) = tc.arguments.as_object_mut() {
                    if let Some(ref sandbox) = self.path_guard.sandbox_dir {
                        obj.insert(
                            "workdir".into(),
                            serde_json::Value::String(sandbox.to_string_lossy().to_string()),
                        );
                    }
                }
            }

            let bash_cmd = tc
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let bash_workdir = tc.arguments.get("workdir").and_then(|v| v.as_str());
            if let Some(denied) = check_bash_command_str(
                bash_cmd,
                bash_workdir,
                &self.deny_list,
                &self.path_guard,
                bash_sandbox_workdir,
            ) {
                self.audit_log
                    .log_destructive(&tc.name, &tc.arguments, false, Some(&denied));
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: denied.clone(),
                            success: false,
                        })
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation
                    .append_async(Message {
                        role: Role::Tool,
                        content: denied,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })
                    .await?;
                return Ok(());
            }
        }

        // grep/glob: apply the same PathGuard containment that
        // read_file/write_file/edit_file get. Without this, the model
        // could enumerate or search outside the sandbox via grep/glob
        // even when file reads/writes are guarded. See `check_search_path`
        // for why we use a separate check rather than `check_read`.
        if matches!(tc.name.as_str(), "grep" | "glob") {
            let path_str = match tc.name.as_str() {
                "glob" => tc
                    .arguments
                    .get("base_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("."),
                _ => tc
                    .arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("."),
            };
            let path = std::path::Path::new(path_str);
            if let GuardVerdict::Denied(msg) = check_search_path(&self.path_guard, path) {
                let denied = format!("🔒 Access denied: {msg}");
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: denied.clone(),
                            success: false,
                        })
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation
                    .append_async(Message {
                        role: Role::Tool,
                        content: denied,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })
                    .await?;
                return Ok(());
            }
        }

        crate::send_or_warn!(
            event_tx
                .send(TurnEvent::ToolStart {
                    name: tc.name.clone(),
                    args: tc.arguments.clone(),
                })
                .await,
            "TurnEvent receiver dropped; discarding event"
        );

        // Pre-tool hook: gating hooks may deny the call with exit code 2.
        let args_json = serde_json::to_string(&tc.arguments).unwrap_or_default();
        if let Some(reason) = self
            .run_pre_tool_hook(
                &format!("pre-tool-{}", tc.name),
                Some(&tc.name),
                Some(&args_json),
            )
            .await
        {
            if is_destructive {
                self.audit_log
                    .log_destructive(&tc.name, &tc.arguments, false, Some(&reason));
            }
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: reason.clone(),
                        success: false,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            self.conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: reason,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
            return Ok(());
        }

        let ctx = self.tool_context_for_call(cancelled);
        let timeout = self.tool_call_timeout();
        let tool_start = Instant::now();
        let outcome = tokio::time::timeout(timeout, tool.run(&ctx, tc.arguments.clone()))
            .await
            .unwrap_or(ToolOutcome::Failure(crate::shared::ToolError::Timeout {
                after_secs: timeout.as_secs(),
            }));
        let tool_duration = tool_start.elapsed();

        let (real_exit_code, real_stdout_len, real_stderr_len) = if tc.name == "bash" {
            extract_bash_metrics(&outcome)
        } else {
            (None, None, None)
        };
        let max_tool_result_chars = read_shared_config(&self.config).max_tool_result_chars;
        let outcome = if tc.name == "bash" {
            truncate_tool_output(outcome, max_tool_result_chars)
        } else {
            outcome
        };
        let outcome_for_emit = outcome.clone();
        let edit_diff = handle_tool_outcome(outcome, tc, event_tx, &mut self.conversation).await?;
        if is_destructive {
            self.audit_log.log_destructive(
                &tc.name,
                &tc.arguments,
                tool_outcome_success(&outcome_for_emit),
                None,
            );
        }
        record(MetricEvent::ToolCall {
            name: tc.name.clone(),
            success: tool_outcome_success(&outcome_for_emit),
            duration_ms: tool_duration.as_millis() as u64,
            error_kind: tool_error_kind(&outcome_for_emit).map(String::from),
        });

        // Post-tool hook
        self.run_hook(
            &format!("post-tool-{}", tc.name),
            Some(&tc.name),
            Some(&args_json),
        );

        let crs = self
            .emit_tool_event_and_correct(
                tc,
                &tc.name,
                &tc.arguments,
                &outcome_for_emit,
                real_exit_code,
                real_stdout_len,
                real_stderr_len,
                edit_diff,
            )
            .await;
        self.collect_carryover(tc, &crs);
        emit_correction_results(crs, tc, event_tx, &mut self.conversation).await?;
        Ok(())
    }

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
                Some(BusEvent::FileWrite(
                    crate::session::event_bus::FileWriteEvent {
                        path: std::path::PathBuf::from(&path),
                        content_length: content.len(),
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

        corrections
    }
}
