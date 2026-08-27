//! Turn execution loop and post-turn hook guard.

use crate::adapters::tool_call_markup::extract_dsml_tool_calls;
use crate::session::access::GuardVerdict;
use crate::session::error_recovery::RetryTracker;
use crate::session::hooks::HookRunner;
use crate::shared::metrics::{record, MetricEvent, PlanDecisionKind};
use crate::shared::{
    read_shared_config, Config, Message, Role, StreamEvent, TokenUsage, ToolInvocation, ToolOutcome,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;

use super::cost_tracking;
use super::helpers::*;
use super::stream::StreamPreamble;
use super::types::{IterationOutcome, TurnEvent, PLAN_COMPLETE_MARKER};
use super::{ApprovalRequest, Executor};

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
        // Spawn the hook asynchronously so Drop returns immediately.
        // Inside the spawned task, shell hooks are already fire-and-forget
        // via tokio::spawn; in-process hooks (e.g. DrawPostTurnHook) run
        // as fast Rust calls inside the task, not on the Drop path.
        let runner = self.runner.clone();
        let config = self.config.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    runner.run("post-turn", &[], &config);
                });
            }
            Err(_) => {
                // ponytail: no runtime → skip. Post-turn hooks are best-effort.
                tracing::trace!("no Tokio runtime at Drop; post-turn hook skipped");
            }
        }
    }
}

// Fill the result of the newest same-name RecordedToolCall that has no
// result yet, falling back to the newest same-name call. TurnEvent carries
// no call id, so parallel same-name calls can't pair exactly; first-result-
// fills-next-empty-slot keeps every call's output instead of overwriting
// one slot twice (WO 46.35).
fn fill_recorded_tool_result(
    tool_calls: &mut [crate::session::replay::RecordedToolCall],
    name: &str,
    output: &str,
) {
    let idx = tool_calls
        .iter()
        .rposition(|tc| tc.tool == name && tc.result.is_empty())
        .or_else(|| tool_calls.iter().rposition(|tc| tc.tool == name));
    if let Some(i) = idx {
        tool_calls[i].result = output.to_string();
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
        // WO 30.6: keep the task spawner's parent-approval forwarder pinned
        // to this turn's approval channel. The channel is session-stable,
        // so this is idempotent across turns; setting it here (the common
        // chokepoint for main-loop, subagent, and test turn paths) means a
        // subagent's destructive-tool requests reach the interactive handler.
        self.set_spawner_parent_approval(approval_sender.clone());
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

        // Release the spawner's clone of the approval sender so the
        // channel closes when the caller drops its own sender. Without
        // this, an approval handler that loops on `recv().await` until
        // channel closure blocks forever (the spawner's clone outlives
        // the turn). Subagent approval forwarding is only needed during
        // a turn; re-set at the next turn's start above.
        self.clear_spawner_parent_approval();

        // TurnComplete: emitted exactly once on every Ok exit path from
        // run_turn_inner (normal completion, max-iterations, parse-error
        // exhaustion, cancellation). The TUI relies on this to clear
        // is_generating/streaming unconditionally — decoupled from
        // CostStats, which only fires when the provider supplies usage.
        if result.is_ok() {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::TurnComplete).await,
                "TurnEvent receiver dropped; discarding event"
            );
        }

        // WO 21.6: post-turn memory extraction (best-effort).
        // Rate limit: extract every 3rd turn, or immediately when the
        // user message contains preference/correction keywords.
        const EXTRACT_EVERY_N_TURNS: u64 = 3;
        self.turn_count += 1;
        let should_extract = self.turn_count % EXTRACT_EVERY_N_TURNS == 0
            || crate::session::memory::extract::is_preference_like(user_input);

        if should_extract && read_shared_config(&self.config).tools.memory_auto_populate {
            if let Some(ref store) = self.memory_store {
                let history = self.conversation.all();
                let last_user = history.iter().rev().find(|m| matches!(m.role, Role::User));
                let last_assistant = history
                    .iter()
                    .rev()
                    .find(|m| matches!(m.role, Role::Assistant) && !m.content.is_empty());
                if let (Some(u), Some(a)) = (last_user, last_assistant) {
                    let facts =
                        crate::session::memory::extract::extract_facts(&u.content, &a.content);
                    let count = facts.len();
                    for f in &facts {
                        if let Err(e) = store.upsert(
                            &f.name,
                            &f.description,
                            &f.body,
                            f.metadata
                                .get("type")
                                .map(|s| s.as_str())
                                .unwrap_or("project"),
                        ) {
                            tracing::trace!(error = %e, name = %f.name, "memory extraction upsert failed");
                        }
                    }
                    if count > 0 {
                        let names: Vec<&str> = facts.iter().map(|f| f.name.as_str()).collect();
                        tracing::info!(count, facts = ?names, "auto-remembered facts");
                        // WO 26.7-R3: tell the TUI so the status bar can
                        // update in real-time as memory grows.
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::MemoryExtracted {
                                    count: store.all().len(),
                                    turn: self.turn_count,
                                })
                                .await,
                            "TurnEvent receiver dropped; discarding event"
                        );
                    }
                }
            }
        }

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
                        // Fill in the result of the matching tool call
                        // (WO 46.35: prefer the newest unfilled slot so
                        // parallel same-name calls each keep their output).
                        fill_recorded_tool_result(&mut tool_calls, name, output);
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
                // WO 45.1: thread the canonical run id (the session id) so
                // a replay trace attributes to its run. Subagent executors
                // inherit the parent session's id via `set_session_id`.
                run_id: self.session_id.clone(),
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

        if self.cost.carryover_enabled {
            self.cost.carryover.last_user_message = user_input.to_string();
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
        let mut retry_tracker = RetryTracker::new();
        let turn_start = Instant::now();

        let max_iterations = read_shared_config(&self.config)
            .tools
            .max_tool_calls_per_turn
            .max(1);

        let max_continuation_rounds = read_shared_config(&self.config)
            .tools
            .max_continuation_rounds
            .clamp(0, 50);
        let mut continuation_count: usize = 0;

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
                    if finish_reason == crate::shared::FinishReason::Length {
                        if max_continuation_rounds == 0 {
                            record_turn_metric(
                                &self.model_name,
                                turn_start,
                                tool_calls.len(),
                                &crate::shared::FinishReason::Error,
                            );
                            crate::send_or_warn!(
                                event_tx
                                    .send(TurnEvent::Error(
                                        "Response truncated (max tokens). Continuation disabled (max_continuation_rounds = 0).".into()
                                    ))
                                    .await,
                                "TurnEvent receiver dropped; discarding event"
                            );
                            return Ok(());
                        }
                        continuation_count += 1;
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::ContinuationRound {
                                    round: continuation_count,
                                    max: max_continuation_rounds,
                                })
                                .await,
                            "TurnEvent receiver dropped; discarding event"
                        );
                        if continuation_count > max_continuation_rounds {
                            let msg = format!(
                                "Max continuation rounds reached ({max_continuation_rounds}). \
                                 The response was truncated and could not be completed within \
                                 the allowed rounds."
                            );
                            crate::send_or_warn!(
                                event_tx.send(TurnEvent::Error(msg.clone())).await,
                                "TurnEvent receiver dropped; discarding event"
                            );
                            record_turn_metric(
                                &self.model_name,
                                turn_start,
                                tool_calls.len(),
                                &crate::shared::FinishReason::Error,
                            );
                            return Ok(());
                        }
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::Token(
                                    "\n\u{26a0} Response was truncated (max tokens). Continuing...\n".into()
                                ))
                                .await,
                            "TurnEvent receiver dropped; discarding event"
                        );
                        self.conversation
                            .append_async(Message {
                                role: Role::User,
                                content: "Your previous response was truncated due to length. Continue exactly where you left off, without repeating any content.".into(),
                                content_parts: None,
                                thinking: None,
                                tool_calls: None,
                                tool_call_id: None,
                                tool_name: None,
                                token_count: None,
                            })
                            .await?;
                        continue;
                    }
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
                    if retry_tracker.can_retry() {
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

    /// Phase-3 recorder: apply the mutable side-effects of one completed tool
    /// call in input order. The tool body itself has already run in Phase 2,
    /// so this method only performs stateful checks (read-before-edit gate,
    /// pre-tool hook for file tools) and records the result.
    ///
    /// `resolved_path` carries the path that Phase 1 (`pre_run_verdict`)
    /// already canonicalized and sandbox-checked for file tools. Passing it
    /// in lets Phase 3 reuse that verdict instead of re-running
    /// `path_guard.check_read`/`check_write` (which would spawn a second
    /// `git check-ignore` for writes and open a TOCTOU window where a
    /// parallel tool flips the guard state between Phase 1 and Phase 3).
    /// Non-file tools pass `None`.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn record_tool_result(
        &mut self,
        tc: &mut ToolInvocation,
        _invocation: &ToolInvocation,
        outcome: ToolOutcome,
        resolved_path: Option<&std::path::Path>,
        duration_ms: u64,
        _approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        _cancelled: &AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<()> {
        let should_audit = matches!(
            tc.name.as_str(),
            "write_file" | "edit_file" | "bash" | "read_file"
        );
        let max_tool_result_chars = read_shared_config(&self.config).tools.max_tool_result_chars;

        if matches!(
            tc.name.as_str(),
            "read_file" | "read_image" | "write_file" | "edit_file"
        ) {
            // Phase 1 already resolved and sandbox-checked the path; reuse
            // that verdict here. Falling back to a fresh check only happens
            // when no resolved path was carried in (defensive — should not
            // happen for file tools in the normal flow, but keeps the
            // method self-contained if called directly).
            let resolved = match resolved_path {
                Some(p) => p.to_path_buf(),
                None => {
                    let path_str = tc
                        .arguments
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let path = std::path::Path::new(path_str);
                    let verdict = if tc.name == "read_file" || tc.name == "read_image" {
                        self.sandbox.check_read(path)
                    } else {
                        self.sandbox.check_write(path).await
                    };
                    match verdict {
                        GuardVerdict::Allowed(r) => r,
                        GuardVerdict::Denied(msg) => {
                            let denied = format!("🔒 Access denied: {msg}");
                            if should_audit {
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
            };

            let path = std::path::Path::new(
                tc.arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            );
            // The read-before-edit gate is enforced pre-body in Phase 2.5
            // (dispatch.rs) using pre-run file state. Re-checking it here
            // with post-body state would flip a just-created new file into
            // looking like an unread overwrite (the Phase 2.5 body already
            // ran and created it), denying a write that already happened.
            // Only the defensive fallback (no resolved path carried in,
            // i.e. direct record calls outside the batch flow) re-checks.
            let needs_read_gate = resolved_path.is_none()
                && (tc.name == "edit_file" || (tc.name == "write_file" && path.exists()));
            if needs_read_gate {
                if let GuardVerdict::Denied(msg) = self.sandbox.check_edit(path, &resolved) {
                    let denied = format!("🔒 Access denied: {msg}");
                    if should_audit {
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
            if let Ok(path_obj) = serde_json::to_value(resolved.to_string_lossy().as_ref()) {
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
                if should_audit {
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

            // ToolStart was emitted at dispatch time in spawn_batch (WO 44.38).

            if matches!(tc.name.as_str(), "read_file" | "read_image") {
                self.sandbox.mark_read(&resolved);
            }

            // ponytail: outcome already computed in Phase 2 (where timeout ran); no second timeout here.
            let outcome = truncate_tool_output(outcome, max_tool_result_chars);
            #[cfg(feature = "budget")]
            let outcome = {
                if let (Some(ref budget), Some(ref store)) = (&self.budget, &self.budget_store) {
                    apply_budget_slice(outcome, budget, store, &self.session_id)
                } else {
                    outcome
                }
            };
            #[cfg(not(feature = "budget"))]
            let outcome = outcome;
            let outcome_for_emit = outcome.clone();
            let edit_diff =
                handle_tool_outcome(outcome, tc, event_tx, &mut self.conversation).await?;
            if let Some(outcome) = self.observe_tool_outcome(&tc.name, &outcome_for_emit, event_tx)
            {
                self.conversation
                    .append_async(Message {
                        role: Role::User,
                        content: outcome.hint.clone(),
                        ..Default::default()
                    })
                    .await?;
                match outcome.action {
                    cost_tracking::DoomLoopAction::AutoPlan => {
                        self.set_plan_mode(true);
                        self.conversation.append_async(Message {
                            role: Role::System,
                            content: "[System: doom loop detected — switched to plan mode. Read-only tools only.]".into(),
                            ..Default::default()
                        }).await?;
                    }
                    cost_tracking::DoomLoopAction::Halt => {
                        return Err(anyhow::anyhow!(
                            "doom loop halted: '{}' failed {} times",
                            outcome.tool,
                            outcome.count
                        ));
                    }
                    cost_tracking::DoomLoopAction::WarnOnly => {}
                }
            }
            record(MetricEvent::ToolCall {
                name: tc.name.clone(),
                success: tool_outcome_success(&outcome_for_emit),
                duration_ms,
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

        // Non-file tools already passed their pre-gate hooks and checks; the
        // body ran in Phase 2. Just record its outcome here.
        // ToolStart was emitted at dispatch time in spawn_batch (WO 44.38).

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
        #[cfg(feature = "budget")]
        let outcome = {
            if let (Some(ref budget), Some(ref store)) = (&self.budget, &self.budget_store) {
                apply_budget_slice(outcome, budget, store, &self.session_id)
            } else {
                outcome
            }
        };
        #[cfg(not(feature = "budget"))]
        let outcome = outcome;
        let outcome_for_emit = outcome.clone();
        let edit_diff = handle_tool_outcome(outcome, tc, event_tx, &mut self.conversation).await?;
        if should_audit {
            self.audit_log.log_destructive(
                &tc.name,
                &tc.arguments,
                tool_outcome_success(&outcome_for_emit),
                None,
            );
        }
        if let Some(outcome) = self.observe_tool_outcome(&tc.name, &outcome_for_emit, event_tx) {
            self.conversation
                .append_async(Message {
                    role: Role::User,
                    content: outcome.hint.clone(),
                    ..Default::default()
                })
                .await?;
            match outcome.action {
                cost_tracking::DoomLoopAction::AutoPlan => {
                    self.set_plan_mode(true);
                    self.conversation.append_async(Message {
                        role: Role::System,
                        content: "[System: doom loop detected — switched to plan mode. Read-only tools only.]".into(),
                        ..Default::default()
                    }).await?;
                }
                cost_tracking::DoomLoopAction::Halt => {
                    return Err(anyhow::anyhow!(
                        "doom loop halted: '{}' failed {} times",
                        outcome.tool,
                        outcome.count
                    ));
                }
                cost_tracking::DoomLoopAction::WarnOnly => {}
            }
        }
        record(MetricEvent::ToolCall {
            name: tc.name.clone(),
            success: tool_outcome_success(&outcome_for_emit),
            duration_ms,
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
    /// Thin orchestrator over the three phase methods in `dispatch.rs`:
    /// `prepare_batch` (pre-gate) → `spawn_batch` (run + mid-batch checkpoint)
    /// → `collect_batch` (record in input order). See those methods for the
    /// per-phase invariants (cancellation, read-before-edit gate, mid-batch
    /// checkpoint guarantee, TOCTOU handling).
    async fn dispatch_tool_call_batch(
        &mut self,
        tcs: &mut [ToolInvocation],
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> anyhow::Result<usize> {
        if tcs.is_empty() || cancelled.load(Ordering::SeqCst) {
            return Ok(0);
        }

        let (prepared, skipped) = self
            .prepare_batch(tcs, approval_sender, cancelled, event_tx)
            .await?;
        let (results, recorded) = self
            .spawn_batch(tcs, prepared, approval_sender, cancelled, event_tx)
            .await?;
        self.collect_batch(
            tcs,
            results,
            skipped,
            &recorded,
            cancelled,
            approval_sender,
            event_tx,
        )
        .await
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
        let StreamPreamble {
            messages,
            tool_defs,
            stem_tokens,
        } = self.build_stream_preamble(user_input);

        let mut rx = self.adapter.stream(&messages, &tool_defs).await?;

        let mut assistant_content = String::new();
        let mut assistant_thinking = String::new();
        tool_calls_out.clear();

        let mut had_parse_error = false;

        // WO 36.3: `live_token` is the executor's root cancel token
        // (subagent executors from WO 35.3, parent sessions from WO 36.4).
        // When attached, each next-event await below is raced against it so
        // a stalled provider stream ends at cancel time instead of the next
        // event or the adapter timeout. When absent, the plain await keeps
        // the WO 15.7 semantics byte-identical.
        let live_token = self.cancel_token.clone();

        loop {
            if cancelled.load(Ordering::SeqCst) {
                // The cancel watcher already emitted "Generation
                // cancelled"; flush any partial assistant message
                // and finish the turn.
                self.flush_partial_assistant(
                    event_tx,
                    &assistant_content,
                    &assistant_thinking,
                    tool_calls_out,
                    "cancelled before execution",
                )
                .await?;

                return Ok(IterationOutcome::Finished(
                    crate::shared::FinishReason::Error,
                ));
            }

            // WO 36.3: race the next event against the live cancel token
            // (above). A cancel fires the flag and re-enters the loop head,
            // which flushes the partial message, appends placeholder tool
            // results, and ends the iteration; dropping `event`'s recv here
            // plus leaving the loop drops `rx`, aborting the in-flight
            // request (the adapter producer task ends when its sends fail).
            let event = if let Some(ref token) = live_token {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        cancelled.store(true, Ordering::SeqCst);
                        continue;
                    }
                    ev = rx.recv() => ev,
                }
            } else {
                rx.recv().await
            };
            let Some(event) = event else { break };

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

                    // WO 43.22: providers that omit usage entirely must
                    // not silently read as zero-cost. Estimate from the
                    // local token cache (WO 42.12): completion from the
                    // assistant message just appended (append_async
                    // populated its token_count), prompt from the
                    // request preamble's per-message counts. Estimated
                    // numbers ride the same CostStats/record_turn path
                    // as real ones — CostStats carries plain counts
                    // (same convention as CacheStats' estimated
                    // stem_tokens); the log line is the estimated flag.
                    let usage = match usage {
                        Some(u) => u,
                        None => {
                            let prompt: usize = messages
                                .iter()
                                .map(crate::session::prompt::estimate_message_tokens)
                                .sum();
                            let completion = self
                                .conversation
                                .all()
                                .last()
                                .and_then(|m| m.token_count)
                                .unwrap_or(0);
                            tracing::info!(
                                prompt,
                                completion,
                                "provider omitted usage; reporting estimated token counts"
                            );
                            TokenUsage {
                                prompt_tokens: Some(prompt),
                                completion_tokens: Some(completion),
                                cached_tokens: None,
                                cache_write_tokens: None,
                            }
                        }
                    };
                    let u = &usage;
                    let prompt = u.prompt_tokens.unwrap_or(0);
                    let completion = u.completion_tokens.unwrap_or(0);
                    let cached = u.cached_tokens.unwrap_or(0);
                    // WO 38.5: config-driven [price_overrides] win over
                    // the built-in table (longest prefix), so unmapped
                    // models can be priced without a code change.
                    let cost = {
                        let cfg = read_shared_config(&self.config);
                        let overrides = &cfg.model.price_overrides;
                        crate::shared::calculate_cost_with_overrides(
                            &self.model_name,
                            u,
                            if overrides.is_empty() {
                                None
                            } else {
                                Some(overrides)
                            },
                        )
                    };
                    self.cost.usage.record_turn(prompt, completion, cost);
                    crate::send_or_warn!(
                        event_tx
                            .send(TurnEvent::CostStats {
                                prompt_tokens: prompt,
                                completion_tokens: completion,
                                turn_cost: cost,
                                cumulative_cost: self.cost.usage.cumulative_cost,
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

        // Channel closed without a Done event. WO 38.5: this is
        // truncation (transport drop mid-stream), not success — mirror
        // the cancel path: persist the partial assistant message, append
        // placeholder tool results so the history stays balanced, tell
        // the user, and finish with Error. Previously the partial was
        // discarded entirely and the turn laundered into Finished(Stop).
        if had_parse_error {
            return Ok(IterationOutcome::ParseError);
        }
        self.flush_partial_assistant(
            event_tx,
            &assistant_content,
            &assistant_thinking,
            tool_calls_out,
            "not executed (stream truncated)",
        )
        .await?;
        if !assistant_content.is_empty()
            || !tool_calls_out.is_empty()
            || !assistant_thinking.is_empty()
        {
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::Error(
                        "Model stream ended without completion; partial response saved (truncated)"
                            .into()
                    ))
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
        }
        Ok(IterationOutcome::Finished(
            crate::shared::FinishReason::Error,
        ))
    }

    /// Flush a partial (cancelled or truncated) assistant turn: append the
    /// partial assistant message and placeholder tool results so the
    /// conversation stays balanced and the next request doesn't see
    /// orphaned tool-call ids. Shared by the cancel path and the
    /// channel-close-without-Done truncation path (WO 38.5).
    async fn flush_partial_assistant(
        &mut self,
        event_tx: &mpsc::Sender<TurnEvent>,
        assistant_content: &str,
        assistant_thinking: &str,
        tool_calls: &[ToolInvocation],
        skip_reason: &str,
    ) -> anyhow::Result<()> {
        if !assistant_content.is_empty() || !tool_calls.is_empty() || !assistant_thinking.is_empty()
        {
            let msg = Message {
                role: Role::Assistant,
                content: assistant_content.to_string(),
                thinking: if assistant_thinking.is_empty() {
                    None
                } else {
                    Some(assistant_thinking.to_string())
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls.to_vec())
                },
                ..Default::default()
            };
            self.conversation.append_async(msg).await?;
        }

        for tc in tool_calls.iter() {
            let result = format!("Tool call {} {skip_reason}", tc.id);
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
        Ok(())
    }
}

#[cfg(test)]
mod fill_recorded_tool_result_tests {
    use super::fill_recorded_tool_result;
    use crate::session::replay::RecordedToolCall;

    fn call(tool: &str) -> RecordedToolCall {
        RecordedToolCall {
            tool: tool.into(),
            args: serde_json::json!({}),
            result: String::new(),
            duration_ms: 0,
        }
    }

    // WO 46.35: parallel same-name calls each keep their own output —
    // the second result must not overwrite the first-filled slot.
    // Without call ids the pairing is positional: the first result to
    // arrive fills the newest empty slot.
    #[test]
    fn parallel_same_name_calls_each_keep_their_output() {
        let mut calls = vec![call("bash"), call("bash")];
        fill_recorded_tool_result(&mut calls, "bash", "out-1");
        fill_recorded_tool_result(&mut calls, "bash", "out-2");
        assert_eq!(calls[1].result, "out-1");
        assert_eq!(calls[0].result, "out-2");
    }

    #[test]
    fn fills_only_the_matching_name() {
        let mut calls = vec![call("grep"), call("edit_file")];
        fill_recorded_tool_result(&mut calls, "edit_file", "edited");
        assert_eq!(calls[0].result, "");
        assert_eq!(calls[1].result, "edited");
    }

    // Duplicate results for one call (error paths emit a synthetic
    // ToolResult after the real one) keep the pre-46.35 behavior:
    // newest same-name slot is overwritten.
    #[test]
    fn duplicate_result_overwrites_newest() {
        let mut calls = vec![call("bash")];
        fill_recorded_tool_result(&mut calls, "bash", "first");
        fill_recorded_tool_result(&mut calls, "bash", "second");
        assert_eq!(calls[0].result, "second");
    }
}
