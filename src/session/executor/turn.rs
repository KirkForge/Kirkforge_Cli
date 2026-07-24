//! Turn execution loop and post-turn hook guard.

use crate::adapters::tool_call_markup::extract_dsml_tool_calls;
use crate::session::access::GuardVerdict;
use crate::session::bash_runner::check_bash_command_str;
use crate::session::error_recovery::RetryTracker;
use crate::session::hooks::HookRunner;
use crate::session::toolset::Toolset;
use crate::shared::metrics::{record, MetricEvent, PlanDecisionKind};
use crate::shared::permission::{evaluate, PermissionAction};
use crate::shared::{
    read_shared_config, Config, Message, Role, StreamEvent, ToolDef, ToolInvocation, ToolOutcome,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

use super::helpers::*;
use super::types::{ApprovalDecision, IterationOutcome, TurnEvent, PLAN_COMPLETE_MARKER};
use super::{ApprovalRequest, Executor};

type RunningTask = (
    usize,
    tokio::task::JoinHandle<Option<(ToolInvocation, ToolOutcome)>>,
);

pub struct PostTurnHookGuard {
    runner: HookRunner,
    config: Config,
}

impl PostTurnHookGuard {
    pub fn new(runner: HookRunner, config: Config) -> Self {
        Self { runner, config }
    }
}

impl Drop for PostTurnHookGuard {
    fn drop(&mut self) {
        // No-op if the hook script doesn't exist; otherwise spawns
        // a tokio task that runs `bash <hooks_dir>/post-turn.sh`
        // with a 5s timeout. Drop completes in microseconds.
        self.runner.run("post-turn", &[], &self.config);
    }
}

impl Executor {
    pub async fn run_turn(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<()> {
        // Post-turn hook: fires on every exit path (Ok / Err / panic /
        // cancel / max-iterations / parse-error second retry) via the
        // `PostTurnHookGuard` constructed on the stack below. The guard
        // owns a cloned `HookRunner`, so it can outlive the `&mut self`
        // borrows inside `run_turn_inner` and fire on Drop without
        // aliasing.
        let _hook_guard = PostTurnHookGuard::new(
            self.hook_runner.clone(),
            crate::shared::read_shared_config(&self.config).clone(),
        );
        let result = self
            .run_turn_inner(user_input, approval_sender, cancelled, event_tx)
            .await;
        if result.is_ok() {
            if let Err(e) = self.conversation.checkpoint_async().await {
                tracing::warn!(error = %e, "post-turn checkpoint failed");
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::Error(format!("Checkpoint failed: {e}")))
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
            }
        }
        result
    }

    /// Batched wrapper: run a turn into a private channel and return every
    /// event as a `Vec`. Keeps the old `run_turn` return shape for callers
    /// that want a slice (tests, non-interactive line mode, persona runner).
    pub async fn run_turn_collecting(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<Vec<TurnEvent>> {
        // `run_turn` is the only producer and there is no concurrent
        // consumer. A plain bounded channel would deadlock once it fills:
        // `run_turn` blocks on `send().await`, but the receiver cannot
        // drain until `run_turn` returns. We keep a bounded channel at the
        // producer boundary for backpressure during normal operation, and
        // spawn a forwarding task that drains it into an unbounded channel.
        let (bounded_tx, mut bounded_rx) = mpsc::channel::<TurnEvent>(10_000);
        let (unbounded_tx, mut unbounded_rx) = mpsc::unbounded_channel::<TurnEvent>();

        let forwarder = tokio::spawn(async move {
            while let Some(ev) = bounded_rx.recv().await {
                if unbounded_tx.send(ev).is_err() {
                    break;
                }
            }
        });

        let turn_start = std::time::Instant::now();
        self.run_turn(user_input, approval_sender, cancelled, &bounded_tx)
            .await?;
        drop(bounded_tx);
        let _ = forwarder.await;

        let mut events = Vec::new();
        while let Ok(ev) = unbounded_rx.try_recv() {
            events.push(ev);
        }

        // ── Trace recording ──
        // If a trace recorder is attached, serialize the turn's events
        // into a TurnRecord and append it to the trace file.
        if let Some(trace) = &self.trace {
            let mut tokens_in: u64 = 0;
            let mut tokens_out: u64 = 0;
            let mut cost_usd: f64 = 0.0;
            let mut tool_calls: Vec<crate::session::replay::RecordedToolCall> = Vec::new();
            let mut model_response = String::new();

            for ev in &events {
                match ev {
                    TurnEvent::CostStats {
                        prompt_tokens,
                        completion_tokens,
                        turn_cost,
                        ..
                    } => {
                        tokens_in += *prompt_tokens as u64;
                        tokens_out += *completion_tokens as u64;
                        cost_usd += turn_cost;
                    }
                    TurnEvent::ToolStart { name, args } => {
                        // We don't have the result or duration yet at
                        // ToolStart time, so record with placeholder
                        // values. ToolResult carries the detail.
                        tool_calls.push(crate::session::replay::RecordedToolCall {
                            tool: name.clone(),
                            args: args.clone(),
                            result: String::new(),
                            duration_ms: 0,
                        });
                    }
                    TurnEvent::ToolResult {
                        name,
                        output,
                        success: _,
                    } => {
                        // Fill in the result of the most recent matching
                        // tool call. In the common case (one tool call per
                        // name per turn), this is correct.
                        if let Some(tc) = tool_calls.iter_mut().rev().find(|tc| tc.tool == *name) {
                            tc.result = output.clone();
                        }
                    }
                    TurnEvent::Token(s) => {
                        model_response.push_str(s);
                    }
                    _ => {}
                }
            }

            let prompt_messages: Vec<crate::session::replay::RecordedMessage> = self
                .conversation
                .all()
                .iter()
                .map(|m| crate::session::replay::RecordedMessage {
                    role: format!("{:?}", m.role).to_lowercase(),
                    content: m.content.clone(),
                })
                .collect();

            let outcome = crate::session::replay::TurnOutcome::Success;
            let duration_ms = turn_start.elapsed().as_millis() as u64;

            let record = crate::session::replay::TurnRecord {
                turn: 0, // TraceRecorder assigns this
                timestamp: chrono::Utc::now().to_rfc3339(),
                prompt_messages,
                model_response,
                tool_calls,
                outcome,
                tokens_in,
                tokens_out,
                duration_ms,
            };

            if let Ok(mut guard) = trace.lock() {
                if let Err(e) = guard.record(record) {
                    tracing::warn!(error = %e, "failed to write trace record");
                }
            }
        }

        Ok(events)
    }

    async fn run_turn_inner(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<()> {
        // --- adapter hot-swap via smart routing ---
        let routing_enabled = read_shared_config(&self.config).model.routing_enabled;
        if routing_enabled {
            // Clone the config for the swap check so we don't hold the
            // read guard across the mutable adapter borrow.
            let cfg_snapshot = read_shared_config(&self.config).clone();
            let swapped =
                self.adapter_swap
                    .maybe_swap(&cfg_snapshot, &mut self.adapter, user_input);
            if let Some(new_model) = swapped {
                self.model_name = new_model.clone();
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::Token(format!("🔀 Switched to {new_model}\n")))
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
            }
        }

        self.conversation
            .append_async(Message {
                role: Role::User,
                content: user_input.to_string(),
                content_parts: None,
                thinking: None,
                tool_calls: None,
                tool_call_id: None,
                tool_name: None,
                token_count: None,
            })
            .await?;

        if self.carryover_enabled {
            self.carryover.last_user_message = user_input.to_string();
        }

        // If this session was recovered from a checkpoint, tell the user
        // once before any model output appears.
        if let Some(count) = self.recovered_messages.take() {
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::Recovered { messages: count })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
        }

        let mut tool_calls: Vec<ToolInvocation> = Vec::new();
        let mut already_retried_parse = false;
        let mut retry_tracker = RetryTracker::new();
        let turn_start = Instant::now();

        let max_iterations = read_shared_config(&self.config)
            .tools
            .max_tool_calls_per_turn
            .max(1);

        for iteration in 0..max_iterations {
            if cancelled.load(Ordering::SeqCst) {
                // The cancel watcher already emitted "Generation
                // cancelled"; just return — events were already sent live.
                record_turn_metric(
                    &self.model_name,
                    turn_start,
                    tool_calls.len(),
                    &crate::shared::FinishReason::Error,
                );
                return Ok(());
            }

            let outcome = self
                .stream_iteration(
                    user_input,
                    approval_sender,
                    cancelled,
                    event_tx,
                    &mut tool_calls,
                )
                .await?;

            match outcome {
                IterationOutcome::Finished(finish_reason) => {
                    record_turn_metric(
                        &self.model_name,
                        turn_start,
                        tool_calls.len(),
                        &finish_reason,
                    );
                    return Ok(());
                }
                IterationOutcome::ToolCalls(mut tcs) => {
                    // Dispatch all requested tool calls in parallel while
                    // preserving input-order conversation semantics. The
                    // prepare/run/record split is documented in ADR-020.
                    let cancelled_idx = self
                        .dispatch_tool_call_batch(&mut tcs, approval_sender, cancelled, event_tx)
                        .await?;

                    // Cancellation may have left requested tool calls without
                    // recorded results. Append placeholder tool-result messages
                    // so the conversation stays consistent and the next model
                    // turn doesn't see orphaned tool-call ids.
                    for skipped in &tcs[cancelled_idx..] {
                        let msg = format!("Tool call {} cancelled before execution", skipped.id);
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::ToolResult {
                                    name: skipped.name.clone(),
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
                                tool_call_id: Some(skipped.id.clone()),
                                tool_name: Some(skipped.name.clone()),
                                ..Default::default()
                            })
                            .await?;
                    }

                    if cancelled_idx < tcs.len() {
                        // The turn was cancelled; do not continue to another
                        // model iteration. Returning Ok lets `run_turn` run
                        // the post-turn hook and checkpoint as usual.
                        record_turn_metric(
                            &self.model_name,
                            turn_start,
                            tool_calls.len(),
                            &crate::shared::FinishReason::Error,
                        );
                        return Ok(());
                    }

                    // Checkpoint after a completed tool batch so a crash
                    // before the next assistant response loses less work.
                    if let Err(e) = self.conversation.checkpoint_async().await {
                        tracing::warn!(error = %e, "post-tool-batch checkpoint failed");
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::Error(format!("Checkpoint failed: {e}")))
                                .await,
                            "TurnEvent receiver dropped; discarding event"
                        );
                    }
                }
                IterationOutcome::ParseError => {
                    if !already_retried_parse {
                        already_retried_parse = true;

                        retry_tracker.wait_before_retry().await;
                        retry_tracker.record_retry();

                        let retry_msg = "Your previous response contained a tool call with malformed JSON arguments. Re-emit ONLY the tool call with the corrected JSON — no additional text, no explanation.";
                        self.conversation
                            .append_async(Message {
                                role: Role::User,
                                content: retry_msg.into(),
                                content_parts: None,
                                thinking: None,
                                tool_calls: None,
                                tool_call_id: None,
                                tool_name: None,
                                token_count: None,
                            })
                            .await?;
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::Token("(JSON parse error, retrying…)\n".into()))
                                .await,
                            "TurnEvent receiver dropped; discarding event"
                        );
                    } else {
                        record_turn_metric(
                            &self.model_name,
                            turn_start,
                            tool_calls.len(),
                            &crate::shared::FinishReason::Error,
                        );
                        return Ok(());
                    }
                }
            }

            if iteration + 1 >= max_iterations {
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::Error("Tool call loop limit reached".into()))
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
                record_turn_metric(
                    &self.model_name,
                    turn_start,
                    tool_calls.len(),
                    &crate::shared::FinishReason::Length,
                );
                return Ok(());
            }
        }

        // Post-turn hook fires from the public `run_turn` wrapper
        // after this inner function returns. Do NOT add an explicit
        // `self.run_hook("post-turn", ...)` here — that double-fires
        // the hook on the natural completion path.
        record_turn_metric(
            &self.model_name,
            turn_start,
            tool_calls.len(),
            &crate::shared::FinishReason::Stop,
        );
        Ok(())
    }

    /// Phase-1 pre-gate: decide whether a tool call should be spawned in
    /// parallel or skipped entirely because it failed a read-only safety
    /// check (unknown tool, plan mode, schema, permission rule, approval,
    /// deny list, URL deny list, bash command check, search-path check, or
    /// pre-tool hook).
    ///
    /// For file tools the path guard is also applied here (so oversized reads
    /// etc. never reach the tool body), but the read-before-edit gate is
    /// deferred to Phase 3 so `[read_file(X), edit_file(X)]` in the same batch
    /// can pass.
    async fn pre_run_verdict(
        &mut self,
        tc: &ToolInvocation,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
    ) -> anyhow::Result<PreRunVerdict> {
        let tool = match self.tools.resolve(&tc.name) {
            Some(t) => t,
            None => {
                return Ok(PreRunVerdict::Skip {
                    events: vec![TurnEvent::Error(format!("Unknown tool: {}", tc.name))],
                    message: format!("Unknown tool: {}", tc.name),
                });
            }
        };

        // Plan-mode enforcement: only read-only discovery tools may run.
        if self.plan_mode {
            let allowed = match tc.name.as_str() {
                "read_file" | "read_image" | "grep" | "glob" => true,
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
                return Ok(PreRunVerdict::Skip {
                    events: vec![TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: format!(
                            "📐 Plan mode blocked {}: only read-only discovery tools are allowed until you type /implement.",
                            tc.name
                        ),
                        success: false,
                    }],
                    message: format!(
                        "📐 Plan mode blocked {}: only read-only discovery tools are allowed until you type /implement.",
                        tc.name
                    ),
                });
            }
        }

        if let Some(reason) = validate_args_against_schema(&tc.arguments, &tool.def().parameters) {
            let err = format!("❌ Invalid arguments for {}: {reason}", tc.name);
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: err.clone(),
                    success: false,
                }],
                message: err,
            });
        }

        let (auto_approve, permission_rules) = {
            let cfg = read_shared_config(&self.config);
            (
                cfg.security.auto_approve,
                cfg.security.permission_rules.clone(),
            )
        };
        let is_destructive = matches!(tc.name.as_str(), "write_file" | "edit_file" | "bash");
        let is_read_only_bash_call = tc.name == "bash"
            && tc
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .map(is_read_only_bash)
                .unwrap_or(false);

        let default_action = if !is_destructive || is_read_only_bash_call {
            PermissionAction::Allow
        } else if auto_approve {
            if tc.name == "bash" {
                PermissionAction::Ask
            } else {
                PermissionAction::Allow
            }
        } else {
            PermissionAction::Ask
        };
        let action = evaluate(&permission_rules, &tc.name, &tc.arguments, default_action);

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
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: reason.clone(),
                    success: false,
                }],
                message: reason,
            });
        }

        if matches!(action, PermissionAction::Ask) {
            match self.run_approval_flow(tc, approval_sender).await? {
                ApprovalDecision::Approved | ApprovalDecision::AlwaysApproved => {}
                ApprovalDecision::Denied { reason } => {
                    let msg = format!("❌ Approval denied: {reason}");
                    return Ok(PreRunVerdict::Skip {
                        events: vec![TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: msg.clone(),
                            success: false,
                        }],
                        message: msg,
                    });
                }
            }
        }

        if let Some(denied) = check_url_in_args(&tc.arguments, &self.deny_list) {
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
                    success: false,
                }],
                message: denied,
            });
        }

        if let Some(denied) = check_deny_list(&self.deny_list, &tc.name, &tc.arguments) {
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
                    success: false,
                }],
                message: denied,
            });
        }

        if tc.name == "bash" {
            let bash_cmd = tc
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let bash_workdir = tc.arguments.get("workdir").and_then(|v| v.as_str());
            let bash_sandbox_workdir = read_shared_config(&self.config)
                .security
                .bash_sandbox_workdir;
            if let Some(denied) = check_bash_command_str(
                bash_cmd,
                bash_workdir,
                &self.deny_list,
                &self.path_guard,
                bash_sandbox_workdir,
            ) {
                return Ok(PreRunVerdict::Skip {
                    events: vec![TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: denied.clone(),
                        success: false,
                    }],
                    message: denied,
                });
            }
        }

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
                return Ok(PreRunVerdict::Skip {
                    events: vec![TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: format!("🔒 Access denied: {msg}"),
                        success: false,
                    }],
                    message: format!("🔒 Access denied: {msg}"),
                });
            }
        }

        // File tools: run path guard here so oversized reads never reach the
        // tool body. Return the resolved path so Phase 3 can check the
        // read-before-edit gate and mark reads without re-resolving.
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
            } else {
                self.path_guard.check_write(path).await
            };
            match verdict {
                GuardVerdict::Allowed(resolved) => {
                    return Ok(PreRunVerdict::Spawn(tool, Some(resolved)));
                }
                GuardVerdict::Denied(msg) => {
                    return Ok(PreRunVerdict::Skip {
                        events: vec![TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: format!("🔒 Access denied: {msg}"),
                            success: false,
                        }],
                        message: format!("🔒 Access denied: {msg}"),
                    });
                }
            }
        }

        // Pre-tool hook for non-file tools. File-tool hooks run after path
        // resolution in `record_tool_result` so they see resolved paths.
        let args_json = serde_json::to_string(&tc.arguments).unwrap_or_default();
        if let Some(reason) = self
            .run_pre_tool_hook(
                &format!("pre-tool-{}", tc.name),
                Some(&tc.name),
                Some(&args_json),
            )
            .await
        {
            let denied = format!("❌ Hook denied {}: {}", tc.name, reason);
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
                    success: false,
                }],
                message: denied,
            });
        }

        Ok(PreRunVerdict::Spawn(tool, None))
    }

    /// Phase-3 recorder: apply the mutable side-effects of one completed tool
    /// call in input order. The tool body itself has already run in Phase 2,
    /// so this method only performs stateful checks (path guard, read-before-edit
    /// gate, pre-tool hook for file tools) and records the result.
    async fn record_tool_result(
        &mut self,
        tc: &mut ToolInvocation,
        _invocation: &ToolInvocation,
        outcome: ToolOutcome,
        _approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        _cancelled: &AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<()> {
        let is_destructive = matches!(tc.name.as_str(), "write_file" | "edit_file" | "bash");
        let max_tool_result_chars = read_shared_config(&self.config).tools.max_tool_result_chars;

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
            } else {
                self.path_guard.check_write(path).await
            };

            match verdict {
                GuardVerdict::Allowed(resolved) => {
                    let needs_read_gate =
                        tc.name == "edit_file" || (tc.name == "write_file" && path.exists());
                    if needs_read_gate {
                        if let GuardVerdict::Denied(msg) =
                            self.read_gate.check_edit(path, &resolved)
                        {
                            let denied = format!("🔒 Access denied: {msg}");
                            if is_destructive {
                                self.audit_log.log_destructive(
                                    &tc.name,
                                    &tc.arguments,
                                    false,
                                    Some(&denied),
                                );
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
                    }

                    let mut run_args = tc.arguments.clone();
                    if let Ok(path_obj) = serde_json::to_value(resolved.to_string_lossy().as_ref())
                    {
                        if let Some(obj) = run_args.as_object_mut() {
                            obj.insert("path".into(), path_obj);
                        }
                    }

                    // Pre-tool hook for file tools now that paths are resolved.
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
                        if is_destructive {
                            self.audit_log.log_destructive(
                                &tc.name,
                                &tc.arguments,
                                false,
                                Some(&denied),
                            );
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

                    crate::send_or_warn!(
                        event_tx
                            .send(TurnEvent::ToolStart {
                                name: tc.name.clone(),
                                args: run_args.clone(),
                            })
                            .await,
                        "TurnEvent receiver dropped; discarding event"
                    );

                    if matches!(tc.name.as_str(), "read_file" | "read_image") {
                        self.read_gate.mark_read(&resolved);
                    }

                    let tool_start = Instant::now();
                    let outcome =
                        tokio::time::timeout(self.tool_call_timeout(), std::future::ready(outcome))
                            .await
                            .unwrap_or(ToolOutcome::Failure(crate::shared::ToolError::Timeout {
                                after_secs: self.tool_call_timeout().as_secs(),
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

                    let result_text = outcome_for_emit.text_content();
                    self.run_hook_with_result(
                        &format!("post-tool-{}", tc.name),
                        Some(&tc.name),
                        Some(&args_json),
                        Some(&result_text),
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
                    if is_destructive {
                        self.audit_log.log_destructive(
                            &tc.name,
                            &tc.arguments,
                            false,
                            Some(&denied),
                        );
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
            }
        }

        // Non-file tools already passed their pre-gate hooks and checks; the
        // body ran in Phase 2. Just record its outcome here.
        crate::send_or_warn!(
            event_tx
                .send(TurnEvent::ToolStart {
                    name: tc.name.clone(),
                    args: tc.arguments.clone(),
                })
                .await,
            "TurnEvent receiver dropped; discarding event"
        );

        let args_json = serde_json::to_string(&tc.arguments).unwrap_or_default();

        let (real_exit_code, real_stdout_len, real_stderr_len) = if tc.name == "bash" {
            extract_bash_metrics(&outcome)
        } else {
            (None, None, None)
        };
        let outcome = if tc.name == "bash" || max_tool_result_chars > 0 {
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
            duration_ms: 0, // body ran in Phase 2; duration recorded there via metrics proxy
            error_kind: tool_error_kind(&outcome_for_emit).map(String::from),
        });

        let result_text = outcome_for_emit.text_content();
        self.run_hook_with_result(
            &format!("post-tool-{}", tc.name),
            Some(&tc.name),
            Some(&args_json),
            Some(&result_text),
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

    /// Dispatch a batch of tool calls in parallel while preserving input-order
    /// conversation semantics. Returns the index of the first call that was not
    /// recorded because cancellation fired during Phase 3 (or `tcs.len()` if the
    /// whole batch was recorded).
    ///
    /// The implementation is split into three phases:
    ///
    /// 1. **Prepare / pre-gate** — for each call, clone the inputs that the
    ///    spawned task needs (`ToolInvocation`, `Arc<dyn Tool>`, cancel token)
    ///    and run all read-only safety checks that can block the call *before*
    ///    the tool body runs: schema validation, plan-mode enforcement, permission
    ///    rules, deny list, path guard (without the read-before-edit gate), and
    ///    pre-tool hooks. Denied calls are not spawned; their failure events are
    ///    buffered and recorded in input order during Phase 3.
    /// 2. **Run** — `tokio::spawn` one task per allowed call. Each task only runs
    ///    the tool's `run()` async method and returns `(ToolInvocation,
    ///    ToolOutcome)`. This is where concurrency happens.
    /// 3. **Record** — sequentially, in input order, apply post-tool mutations
    ///    to the Executor: emit `ToolStart`/`ToolResult` events, append the
    ///    `ToolResult` message to the conversation, update the read gate for
    ///    completed reads, append the audit entry, update carryover, and fire hooks.
    ///
    /// Cancellation is checked before each Phase 3 record. If cancelled mid-
    /// batch, the remaining completed calls are not recorded, and the caller
    /// appends placeholder `ToolResult` messages for them.
    ///
    /// The read-before-edit check runs in Phase 3 against the *completed*
    /// reads in this batch, so `[read_file(X), edit_file(X)]` executed in the
    /// same batch passes: the read task completes and marks the file before
    /// the edit task's record phase checks the gate.
    async fn dispatch_tool_call_batch(
        &mut self,
        tcs: &mut [ToolInvocation],
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<usize> {
        if tcs.is_empty() {
            return Ok(0);
        }

        // Short-circuit the whole batch when cancellation is already set.
        if cancelled.load(Ordering::SeqCst) {
            return Ok(0);
        }

        // Phase 1 — Prepare + pre-gate. Determine, for each call, whether it
        // should be spawned or skipped with a buffered failure. This phase does
        // not mutate Executor state (it only reads config/tools/guards).
        let mut prepared: Vec<PreparedCall> = Vec::with_capacity(tcs.len());
        let mut skipped: Vec<(usize, ToolInvocation, Vec<TurnEvent>, String)> = Vec::new();
        for (idx, tc) in tcs.iter().enumerate() {
            match self.pre_run_verdict(tc, approval_sender).await? {
                PreRunVerdict::Spawn(tool, resolved) => {
                    prepared.push(PreparedCall {
                        idx,
                        invocation: tc.clone(),
                        tool,
                        cancel_token: tool_cancel_token(cancelled),
                        resolved_path: resolved,
                        timeout: self.tool_call_timeout(),
                    });
                }
                PreRunVerdict::Skip { events, message } => {
                    skipped.push((idx, tc.clone(), events, message));
                }
            }
        }

        // Phase 2 — Run: spawn one task per non-file call. File calls are
        // run sequentially in Phase 3 so the read-before-edit gate can observe
        // reads before edits in the same batch.
        //
        // When deterministic mode is active (--seed), skip tokio::spawn and
        // run all calls sequentially to eliminate nondeterminism from task
        // scheduling. The tool-call *sequence* is what matters for regression
        // testing; the model's output content may still vary by provider.
        let mut running: Vec<RunningTask> = Vec::with_capacity(prepared.len());
        let mut deferred_file_calls: Vec<PreparedCall> = Vec::new();
        let mut results: std::collections::HashMap<usize, (ToolInvocation, ToolOutcome)> =
            std::collections::HashMap::with_capacity(prepared.len());
        let deterministic = self.is_deterministic();
        for prep in prepared {
            if prep.resolved_path.is_some() {
                deferred_file_calls.push(prep);
                continue;
            }
            let idx = prep.idx;
            if deterministic {
                // Run sequentially — no tokio::spawn, no concurrency.
                let outcome = run_prepared_call(prep).await;
                if let Some((invocation, result)) = outcome {
                    results.insert(idx, (invocation, result));
                }
            } else {
                let handle = tokio::spawn(run_prepared_call(prep));
                running.push((idx, handle));
            }
            // Yield so the just-spawned task can start. If the user cancelled
            // while we were pre-gating, the next iteration sees the flag and
            // stops spawning remaining calls. This preserves the sequential
            // cancellation semantics tests rely on without losing concurrency
            // among the calls that were already spawned.
            tokio::task::yield_now().await;
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
        }

        // Collect completed non-file results and record them incrementally.
        //
        // Recording is interleaved with collection (not deferred to Phase 3):
        // each completed tool result is appended to the conversation and
        // checkpointed before the next handle is awaited. This is the
        // "mid-batch checkpoint" guarantee — a crash or cancellation while a
        // later, slower tool is still in-flight does not lose the results
        // that already finished. Without this, a turn aborted while the
        // collect loop is blocked on a slow tool would persist nothing,
        // because Phase 3 (which appends to the conversation) only runs
        // after the whole collect loop completes.
        //
        // Handles are awaited in input order so the conversation still
        // records tool results in the order the model requested them.
        // Cancellation is checked after each record: once cancelled, the
        // remaining in-flight handles are dropped (their tasks finish
        // detached but their results are never recorded) and Phase 3 / the
        // caller append placeholder tool-result messages for them.
        //
        // In deterministic mode (--seed), non-file tools ran sequentially
        // in Phase 2 and their results are already in `results`. The
        // `running` vec is empty, so this loop is a no-op.
        let mut recorded: std::collections::HashSet<usize> =
            std::collections::HashSet::with_capacity(running.len());
        for (idx, handle) in running {
            let pair = if let Ok(Some(p)) = handle.await {
                p
            } else {
                // Join error (task panicked/cancelled): leave unrecorded so
                // Phase 3 emits a placeholder for this index.
                continue;
            };
            let tc = &mut tcs[idx];
            let (invocation, outcome) = pair;
            self.record_tool_result(
                tc,
                &invocation,
                outcome,
                approval_sender,
                cancelled,
                event_tx,
            )
            .await?;
            if let Err(e) = self.conversation.checkpoint_async().await {
                tracing::warn!(error = %e, "mid-batch checkpoint failed after tool {}", tc.id);
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::Error(format!("Checkpoint failed: {e}")))
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
            }
            recorded.insert(idx);
            // Stop launching further awaits once cancelled; the in-flight
            // task we just awaited is recorded, later ones get placeholders.
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
        }

        // Phase 2.5 — Run file tools sequentially in input order. Cancellation
        // is checked before each call. The read-before-edit gate is checked
        // before running a write/edit body so unread existing files are never
        // touched; reads earlier in the same batch have already been marked at
        // the end of their body, so `[read_file(X), write_file(X)]` passes.
        for prep in deferred_file_calls {
            if cancelled.load(Ordering::SeqCst) {
                // The remaining deferred file calls won't be recorded;
                // Phase 3 will append placeholders for them.
                break;
            }
            let idx = prep.idx;
            let name = prep.invocation.name.clone();
            let path = prep
                .resolved_path
                .as_ref()
                .expect("file call has resolved path")
                .clone();

            let path_arg = prep
                .invocation
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let needs_read_gate = name == "edit_file" || (name == "write_file" && path.exists());
            if needs_read_gate {
                if let GuardVerdict::Denied(msg) = self
                    .read_gate
                    .check_edit(std::path::Path::new(path_arg), &path)
                {
                    let denied = format!("🔒 Access denied: {msg}");
                    let invocation = prep.invocation.clone();
                    results.insert(
                        idx,
                        (
                            invocation,
                            ToolOutcome::Failure(crate::shared::ToolError::AccessDenied {
                                message: denied,
                            }),
                        ),
                    );
                    continue;
                }
            }

            let invocation = prep.invocation.clone();
            let outcome = run_prepared_call(prep).await.map(|(_, o)| o);
            if let Some(ref o) = outcome {
                // Mark reads immediately so later writes in the same batch
                // see them when their read-before-edit gate runs.
                if name == "read_file" || name == "read_image" {
                    self.read_gate.mark_read(&path);
                }
                results.insert(idx, (invocation, o.clone()));
            }
        }

        // Phase 3 — Record: walk input order, replay skipped/denied calls,
        // then record each completed file-tool result in order. Non-file
        // results were already recorded incrementally in the collect loop
        // above (so a mid-batch crash persists them); skip those indices
        // here. Cancellation is checked only when a result is missing, so
        // already-completed tool bodies are still recorded and earlier calls
        // in the batch don't become placeholders.
        for (idx, tc) in tcs.iter_mut().enumerate() {
            if recorded.contains(&idx) {
                // Already recorded and checkpointed in the collect loop.
                continue;
            }
            let has_result = results.contains_key(&idx);
            let has_skip = skipped.iter().any(|(i, _, _, _)| *i == idx);
            if !has_result && !has_skip && cancelled.load(Ordering::SeqCst) {
                tracing::debug!("tool batch short-circuited by cancellation at record phase");
                return Ok(idx);
            }

            if let Some(pos) = skipped.iter().position(|(i, _, _, _)| *i == idx) {
                let (_, _inv, events, msg) = skipped.swap_remove(pos);
                for ev in events {
                    crate::send_or_warn!(
                        event_tx.send(ev).await,
                        "TurnEvent receiver dropped; discarding event"
                    );
                }
                self.conversation
                    .append_async(Message {
                        role: Role::Tool,
                        content: msg,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })
                    .await?;
                continue;
            }

            let Some((invocation, outcome)) = results.remove(&idx) else {
                let err = format!("Tool call {} did not return an outcome", tc.id);
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
                continue;
            };

            self.record_tool_result(
                tc,
                &invocation,
                outcome,
                approval_sender,
                cancelled,
                event_tx,
            )
            .await?;

            // Persist after each recorded result so a crash before the next
            // tool starts does not lose in-flight progress.
            if let Err(e) = self.conversation.checkpoint_async().await {
                tracing::warn!(error = %e, "mid-batch checkpoint failed after tool {}", tc.id);
                crate::send_or_warn!(
                    event_tx
                        .send(TurnEvent::Error(format!("Checkpoint failed: {e}")))
                        .await,
                    "TurnEvent receiver dropped; discarding event"
                );
            }
        }

        Ok(tcs.len())
    }

    #[allow(unused_variables)]
    async fn stream_iteration(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
        tool_calls_out: &mut Vec<ToolInvocation>,
    ) -> anyhow::Result<IterationOutcome> {
        let model_info = self.adapter.model_info();
        let tool_defs: Vec<ToolDef> = self.tools.definitions();
        let tool_names: Vec<&str> = tool_defs.iter().map(|t| t.name).collect();

        let carryover_block = if self.carryover_enabled {
            let block = self.carryover.to_prompt_block();
            if block.is_empty() {
                None
            } else {
                Some(block)
            }
        } else {
            None
        };

        // Snapshot memory knobs so we don't hold the config lock across
        // the prompt-builder memory lookup.
        let (memory_enabled, memory_max_tokens, memory_top_n) = {
            let cfg = read_shared_config(&self.config);
            (
                cfg.display.memory_enabled,
                cfg.display.memory_max_tokens,
                cfg.display.memory_top_n,
            )
        };

        // Build a richer memory context from the current user turn plus
        // the most recent assistant message, if any.
        let memory_context = {
            let history = self.conversation.all();
            let mut ctx = String::from(user_input);
            if let Some(last_assistant) = history
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::Assistant) && !m.content.is_empty())
            {
                ctx.push(' ');
                ctx.push_str(&last_assistant.content);
            }
            if ctx.trim().is_empty() {
                None
            } else {
                Some(ctx)
            }
        };

        let system = self.prompt_builder.build(
            &model_info.name,
            model_info.supports_thinking,
            &tool_names,
            carryover_block.as_deref(),
            memory_context.as_deref(),
            memory_enabled,
            memory_max_tokens,
            memory_top_n,
        );

        let history = self.conversation.all();
        let tool_results: Vec<Message> = Vec::new(); // sent as part of history

        let messages = self.prompt_builder.build_messages(
            system,
            history,
            model_info.max_context_tokens,
            &tool_results,
        );

        // Snapshot the stable prompt-cache stem size for this turn so we
        // can verify KV-cache reuse against the adapter usage stats.
        let stem_tokens = self.prompt_builder.estimate_stem_tokens(
            &model_info.name,
            model_info.supports_thinking,
            &tool_names,
        );

        let mut rx = self.adapter.stream(&messages, &tool_defs).await?;

        let mut assistant_content = String::new();
        let mut assistant_thinking = String::new();
        tool_calls_out.clear();

        let mut had_parse_error = false;

        while let Some(event) = rx.recv().await {
            if cancelled.load(Ordering::SeqCst) {
                // The cancel watcher already emitted "Generation
                // cancelled"; flush any partial assistant message
                // and finish the turn.
                if !assistant_content.is_empty()
                    || !tool_calls_out.is_empty()
                    || !assistant_thinking.is_empty()
                {
                    let msg = Message {
                        role: Role::Assistant,
                        content: assistant_content.clone(),
                        thinking: if assistant_thinking.is_empty() {
                            None
                        } else {
                            Some(assistant_thinking.clone())
                        },
                        tool_calls: if tool_calls_out.is_empty() {
                            None
                        } else {
                            Some(tool_calls_out.clone())
                        },
                        ..Default::default()
                    };
                    self.conversation.append_async(msg).await?;
                }

                // If the assistant had emitted tool calls before the user
                // cancelled, append placeholder results so the conversation
                // history stays balanced and the next turn doesn't see
                // orphaned tool-call ids.
                for tc in tool_calls_out.iter() {
                    let result = format!("Tool call {} cancelled before execution", tc.id);
                    crate::send_or_warn!(
                        event_tx
                            .send(TurnEvent::ToolResult {
                                name: tc.name.clone(),
                                output: result.clone(),
                                success: false,
                            })
                            .await,
                        "TurnEvent receiver dropped; discarding event"
                    );
                    self.conversation
                        .append_async(Message {
                            role: Role::Tool,
                            content: result,
                            tool_call_id: Some(tc.id.clone()),
                            tool_name: Some(tc.name.clone()),
                            ..Default::default()
                        })
                        .await?;
                }

                return Ok(IterationOutcome::Finished(
                    crate::shared::FinishReason::Error,
                ));
            }

            match event {
                StreamEvent::Text(t) => {
                    assistant_content.push_str(&t);
                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::Token(t)).await,
                        "TurnEvent receiver dropped; discarding event"
                    );
                }
                StreamEvent::Thinking(t) => {
                    assistant_thinking.push_str(&t);
                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::Thinking(t)).await,
                        "TurnEvent receiver dropped; discarding event"
                    );
                }
                StreamEvent::ToolCall(tc) => {
                    tool_calls_out.push(tc);
                }
                StreamEvent::Error(e) => {
                    if e.contains("parse") || e.contains("parseable") {
                        had_parse_error = true;
                    }
                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::Error(e)).await,
                        "TurnEvent receiver dropped; discarding event"
                    );
                }
                StreamEvent::Done {
                    finish_reason,
                    usage,
                } => {
                    // Fallback: some models (notably DeepSeek cloud through
                    // Ollama's /api/chat proxy) emit native DSML markup in
                    // the content stream instead of a valid tool_calls JSON
                    // array. If the adapter delivered no tool calls but the
                    // assistant content contains DSML blocks, extract them,
                    // strip the markup from the persisted message, and treat
                    // the turn as a tool-call turn.
                    if tool_calls_out.is_empty() {
                        let (cleaned, dsml_calls) = extract_dsml_tool_calls(&assistant_content);
                        if !dsml_calls.is_empty() {
                            assistant_content = cleaned;
                            tool_calls_out.extend(dsml_calls);
                        }
                    }

                    let msg = Message {
                        role: Role::Assistant,
                        content: assistant_content.clone(),
                        content_parts: None,
                        thinking: if assistant_thinking.is_empty() {
                            None
                        } else {
                            Some(assistant_thinking.clone())
                        },
                        tool_calls: if tool_calls_out.is_empty() {
                            None
                        } else {
                            Some(tool_calls_out.clone())
                        },
                        tool_call_id: None,
                        tool_name: None,
                        token_count: usage.as_ref().and_then(|u| u.completion_tokens),
                    };
                    self.conversation.append_async(msg).await?;

                    // If we're in plan mode and the assistant signalled
                    // completion, surface a PlanComplete event so the TUI
                    // can ask the user to approve implementation.
                    if self.plan_mode && assistant_content.contains(PLAN_COMPLETE_MARKER) {
                        crate::send_or_warn!(
                            event_tx.send(TurnEvent::PlanComplete).await,
                            "TurnEvent receiver dropped; discarding event"
                        );
                    }

                    if let Some(ref u) = usage {
                        let prompt = u.prompt_tokens.unwrap_or(0);
                        let completion = u.completion_tokens.unwrap_or(0);
                        let cached = u.cached_tokens.unwrap_or(0);
                        let cost = crate::shared::calculate_cost(&self.model_name, u);
                        self.cost_tracking.record_turn(prompt, completion, cost);
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::CostStats {
                                    prompt_tokens: prompt,
                                    completion_tokens: completion,
                                    turn_cost: cost,
                                    cumulative_cost: self.cost_tracking.cumulative_cost,
                                })
                                .await,
                            "TurnEvent receiver dropped; discarding event"
                        );
                        // Emit cache stats whenever the provider reports
                        // cache-read tokens. The stem size is the stable
                        // prefix the adapter should be reusing; a positive
                        // cached count is the KV-cache hit verification.
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::CacheStats {
                                    cached_tokens: cached,
                                    prompt_tokens: prompt,
                                    stem_tokens,
                                })
                                .await,
                            "TurnEvent receiver dropped; discarding event"
                        );
                    }

                    if !tool_calls_out.is_empty() {
                        for tc in tool_calls_out.iter() {
                            let reason = if assistant_thinking.is_empty() {
                                "model-emitted tool call".to_string()
                            } else {
                                assistant_thinking.clone()
                            };
                            record(MetricEvent::PlanReason {
                                decision_kind: PlanDecisionKind::ToolSelect,
                                reason,
                                related_id: Some(tc.id.clone()),
                                confidence: 1.0,
                            });
                        }
                        return Ok(IterationOutcome::ToolCalls(tool_calls_out.clone()));
                    }

                    return Ok(if had_parse_error {
                        IterationOutcome::ParseError
                    } else {
                        IterationOutcome::Finished(finish_reason)
                    });
                }
            }
        }

        if had_parse_error {
            Ok(IterationOutcome::ParseError)
        } else {
            Ok(IterationOutcome::Finished(
                crate::shared::FinishReason::Stop,
            ))
        }
    }
}
/// Verdict produced by the Phase 1 pre-gate.  means the call is allowed
/// to run in parallel; `Skip` means it was denied before the tool body and the
/// supplied events/message should be recorded in input order during Phase 3.
enum PreRunVerdict {
    Spawn(Arc<dyn crate::tools::Tool>, Option<std::path::PathBuf>),
    Skip {
        events: Vec<TurnEvent>,
        message: String,
    },
}

/// Inputs cloned for a single prepared tool call. Owned so the call can be
/// moved into a spawned task without borrowing `Executor` state.
struct PreparedCall {
    idx: usize,
    invocation: ToolInvocation,
    tool: Arc<dyn crate::tools::Tool>,
    cancel_token: tokio_util::sync::CancellationToken,
    resolved_path: Option<std::path::PathBuf>,
    timeout: std::time::Duration,
}

/// Run only the tool body for a prepared call, returning the original
/// invocation and the tool outcome.
///
/// This function deliberately does not touch `Executor` state; it is the
/// concurrency boundary where tool I/O may run in parallel. It checks the
/// shared cancellation flag after yielding so tasks spawned just before a
/// cancellation get a chance to short-circuit before invoking the tool body.
async fn run_prepared_call(prep: PreparedCall) -> Option<(ToolInvocation, ToolOutcome)> {
    let ctx = crate::tools::ToolContext {
        token: prep.cancel_token,
        dry_run: false,
        task_spawner: None,
    };
    let outcome = tokio::time::timeout(
        prep.timeout,
        prep.tool.run(&ctx, prep.invocation.arguments.clone()),
    )
    .await
    .unwrap_or(ToolOutcome::Failure(crate::shared::ToolError::Timeout {
        after_secs: prep.timeout.as_secs(),
    }));
    Some((prep.invocation, outcome))
}
