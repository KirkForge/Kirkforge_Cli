//! Turn execution loop and post-turn hook guard.

use crate::adapters::tool_call_markup::extract_dsml_tool_calls;
use crate::session::hooks::HookRunner;
use crate::session::toolset::Toolset;
use crate::shared::{
    read_shared_config, Config, Message, Role, StreamEvent, ToolDef, ToolInvocation,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;

use super::helpers::*;
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

        self.run_turn(user_input, approval_sender, cancelled, &bounded_tx)
            .await?;
        drop(bounded_tx);
        let _ = forwarder.await;

        let mut events = Vec::new();
        while let Ok(ev) = unbounded_rx.try_recv() {
            events.push(ev);
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
        let routing_enabled = read_shared_config(&self.config).routing_enabled;
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
        let turn_start = Instant::now();

        let max_iterations = read_shared_config(&self.config)
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
                    for tc in &mut tcs {
                        if cancelled.load(Ordering::SeqCst) {
                            tracing::debug!("tool batch short-circuited by cancellation");
                            break;
                        }
                        self.dispatch_tool_call(tc, approval_sender, cancelled, event_tx)
                            .await?;
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
            (cfg.memory_enabled, cfg.memory_max_tokens, cfg.memory_top_n)
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
                    }

                    if !tool_calls_out.is_empty() {
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
