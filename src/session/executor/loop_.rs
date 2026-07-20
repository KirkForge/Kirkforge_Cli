//! Long-running executor control loop.

use crate::session::conversation::ConversationLog;
use crate::session::prompt::CompactRequest;
use crate::shared::metrics::{record, MetricEvent, PlanDecisionKind};
use crate::shared::{read_shared_config, Config, Message, Role};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::types::CompactHookStats;
use super::TurnEvent;
use super::{ApprovalRequest, Executor};

impl Executor {
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &mut self,
        mut input_rx: mpsc::UnboundedReceiver<String>,
        event_tx: mpsc::Sender<TurnEvent>,
        approval_tx: mpsc::UnboundedSender<ApprovalRequest>,
        mut cancel_rx: mpsc::UnboundedReceiver<()>,
        mut resume_rx: mpsc::UnboundedReceiver<ConversationLog>,
        mut compact_rx: mpsc::UnboundedReceiver<CompactRequest>,
        mut model_rx: mpsc::UnboundedReceiver<String>,
        mut undo_rx: mpsc::UnboundedReceiver<()>,
        mut config_rx: mpsc::UnboundedReceiver<Config>,
        mut plan_rx: mpsc::UnboundedReceiver<bool>,
        mut plugin_reload_rx: mpsc::UnboundedReceiver<kirkforge_plugin_host::PluginRegistry>,
    ) -> anyhow::Result<()> {
        let cancelled = Arc::new(AtomicBool::new(false));

        // Cancel watcher: drains the cancel channel and sets the
        // shared flag so that an in-flight turn can observe
        // cancellation without waiting for the outer `select!` to
        // poll `cancel_rx`. Previously `run_turn(...).await` was
        // awaited directly in the `input_rx` arm, so `cancel_rx` was
        // not polled while a turn streamed.
        let cancel_event_tx = event_tx.clone();
        let cancel_watcher_cancelled = cancelled.clone();
        tokio::spawn(async move {
            while cancel_rx.recv().await.is_some() {
                cancel_watcher_cancelled.store(true, Ordering::SeqCst);
                if cancel_event_tx
                    .send(TurnEvent::Token("\n⚠️ Generation cancelled\n".into()))
                    .await
                    .is_err()
                {
                    tracing::warn!("TUI event receiver dropped; cancel watcher exiting");
                    break;
                }
            }
        });

        // Fire session-start hook (fire-and-forget, best-effort)
        self.run_hook("session-start", None, None);

        loop {
            tokio::select! {
                biased; // check control channels first, then input

                // Review.md gap #7 — in-app undo. The TUI sends a
                // signal over `undo_rx`; we pop the executor's undo
                // stack and emit the result as a system token.
                Some(()) = undo_rx.recv() => {
                    let msg = if let Some(ref stack) = self.undo_stack {
                        match stack.lock() {
                            Ok(mut s) => match s.pop() {
                                Ok(Some(r)) => format!(
                                    "↶ Undo: {} ({})",
                                    if r.prev_existed {
                                        format!("restored {}", r.path.display())
                                    } else {
                                        format!("removed newly-created {}", r.path.display())
                                    },
                                    r.kind.as_str()
                                ),
                                Ok(None) => "Nothing to undo.".to_string(),
                                Err(e) => format!("Undo failed: {e}"),
                            },
                            Err(e) => format!("Undo stack mutex poisoned: {e}"),
                        }
                    } else {
                        "Undo unavailable: no undo stack for this session.".to_string()
                    };
                    if event_tx.send(TurnEvent::Token(msg)).await.is_err() {
                        tracing::warn!("TUI event receiver dropped during /undo; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                // Review.md gap #5 — mid-session model swap. The TUI
                // forwards `/model <name>` here; we install the named
                // adapter via `AdapterSwap::force_swap` (which
                // bypasses the smart-router) and emit a confirmation
                // token so the user sees the swap land. The next turn
                // will use the new adapter.
                Some(model_name) = model_rx.recv() => {
                    let cfg_snapshot = read_shared_config(&self.config).clone();
                    let new_name = self
                        .adapter_swap
                        .force_swap(&model_name, &mut self.adapter, &cfg_snapshot);
                    self.model_name = new_name.clone();
                    if event_tx
                        .send(TurnEvent::Token(format!(
                            "🔀 Switched to {new_name}\n"
                        )))
                        .await
                        .is_err()
                    {
                        tracing::warn!(
                            "TUI event receiver dropped while reporting model swap; exiting"
                        );
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(enable) = plan_rx.recv() => {
                    self.set_plan_mode(enable);
                    let msg = if enable {
                        "📐 Plan mode enabled — only read-only tools are permitted. Type /implement when ready.\n".to_string()
                    } else {
                        match self.exit_plan_mode().await {
                            Ok(m) => format!("✅ {m}\n"),
                            Err(e) => {
                                tracing::warn!("exit_plan_mode failed: {}", e);
                                format!("⚠️ Could not exit plan mode: {e}\n")
                            }
                        }
                    };
                    if event_tx.send(TurnEvent::Token(msg)).await.is_err() {
                        tracing::warn!("TUI event receiver dropped during plan-mode toggle; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(new_config) = config_rx.recv() => {
                    let diff_summary = self.reload_config(new_config);
                    let msg = if diff_summary.is_empty() {
                        "🔄 Reloaded config (no changes)\n".to_string()
                    } else {
                        format!("🔄 Reloaded config: {diff_summary}\n")
                    };
                    if event_tx.send(TurnEvent::Token(msg)).await.is_err() {
                        tracing::warn!("TUI event receiver dropped during config reload; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(registry) = plugin_reload_rx.recv() => {
                    let summary = self.reload_plugins(&registry);
                    if event_tx
                        .send(TurnEvent::Token(format!("🔌 {summary}\n")))
                        .await
                        .is_err()
                    {
                        tracing::warn!("TUI event receiver dropped during plugin reload; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(new_log) = resume_rx.recv() => {

                    self.replace_conversation(new_log);
                    if event_tx.send(TurnEvent::Token("✅ Resumed from fork\n".into())).await.is_err() {
                        tracing::warn!("TUI event receiver dropped during /resume; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(req) = compact_rx.recv() => {
                    let history = self.conversation.all().to_vec();

                    // Snapshot the config fields we need; the guard must
                    // drop before we mutate `self.conversation` below.
                    let (summarize_enabled, summarize_model, ollama_host, preserve_recent) = {
                        let cfg = read_shared_config(&self.config);
                        (
                            cfg.summarize_enabled,
                            cfg.summarize_model.clone(),
                            cfg.ollama_host.clone(),
                            cfg.preserve_recent_messages,
                        )
                    };
                    let keep = req.keep.unwrap_or(preserve_recent).max(1);
                    let original_tokens = crate::session::prompt::estimate_tokens(&history
                    );

                    // Notify lifecycle hooks that compaction is starting.
                    self.run_compact_hook(
                        "pre-compact",
                        CompactHookStats {
                            message_count: history.len(),
                            preserve_recent: keep,
                            original_count: history.len(),
                            result_count: history.len(),
                            dropped_tool_results: 0,
                            condensed_assistant_turns: 0,
                            summarised_messages: 0,
                            tokens_before: original_tokens,
                            tokens_after: original_tokens,
                            strategy: "pending",
                        },
                    );

                    // Record the decision that triggered compaction.
                    let budget_threshold = self.adapter.model_info().max_context_tokens * 9 / 10;
                    record(MetricEvent::PlanReason {
                        decision_kind: PlanDecisionKind::CompactionTrigger,
                        reason: format!("budget exceeded at {original_tokens} tokens (threshold {budget_threshold})"),
                        related_id: None,
                        confidence: 1.0,
                    });

                    let mut did_summarize = false;
                    let mut compact_stats = None;

                    // Try LLM-based summarization if enabled
                    if summarize_enabled && history.len() > 2 {
                        // Preserve the system anchor and `keep` recent messages.
                        let working_set_size = keep;
                        let anchor = if !history.is_empty()
                            && matches!(history[0].role, Role::System)
                        {
                            1
                        } else {
                            0
                        };

                        let summarize_from = anchor;
                        let summarize_to = history.len().saturating_sub(working_set_size);
                        if summarize_to > summarize_from + 6
                        {
                            let to_summarize: Vec<Message> = history[summarize_from..summarize_to]
                                .to_vec();
                            if !to_summarize.is_empty() {
                                let summarizer_config = crate::session::prompt::summarizer::SummarizerConfig {
                                    model: summarize_model.clone(),
                                    max_summary_tokens: 500,
                                    min_turns_for_summary: 4,
                                    min_compression_ratio: 0.4,
                                };

                                let result = crate::session::prompt::summarizer::summarize_conversation(
                                    &summarizer_config,
                                    &to_summarize,
                                    &ollama_host,
                                )
                                .await;

                                if let Some(ref summary) = result.summary {
                                    let mut new_msgs = Vec::new();
                                    // Keep the anchor
                                    if anchor > 0 {
                                        new_msgs.push(history[0].clone());
                                    }
                                    // Insert summary as system message
                                    new_msgs.push(Message {
                                        role: Role::System,
                                        content: format!(
                                            "[Context summary — {} messages compressed]\n{}",
                                            result.summarised_messages, summary
                                        ),
                                        ..Default::default()
                                    });
                                    // Append working set
                                    for msg in &history[summarize_to..] {
                                        new_msgs.push(msg.clone());
                                    }

                                    let tokens_after = crate::session::prompt::compaction::estimate_tokens(
                                        &new_msgs
                                    );

                                    if let Err(e) = self.conversation.replace_all_async(new_msgs.clone()).await
                                    {
                                        if event_tx
                                            .send(TurnEvent::Error(format!(
                                                "Summarization failed: {e}"
                                            )))
                                            .await
                                            .is_err()
                                        {
                                            self.flush_carryover();
                                            return Ok(());
                                        }
                                    } else {
                                        did_summarize = true;
                                        compact_stats = Some(CompactHookStats {
                                            message_count: history.len(),
                                            preserve_recent: keep,
                                            original_count: history.len(),
                                            result_count: new_msgs.len(),
                                            dropped_tool_results: 0,
                                            condensed_assistant_turns: 0,
                                            summarised_messages: result.summarised_messages,
                                            tokens_before: original_tokens,
                                            tokens_after,
                                            strategy: "summarize",
                                        });
                                        let report = TurnEvent::Token(format!(
                                            "🧠 Summarised {}→{} messages ({}→{} tokens, {:.0}% compression)\n",
                                            result.summarised_messages,
                                            if anchor > 0 { 1 + history.len() - summarize_to } else { history.len() - summarize_to },
                                            result.tokens_before,
                                            result.tokens_after,
                                            (1.0 - result.tokens_after as f64 / result.tokens_before.max(1) as f64) * 100.0,
                                        ));
                                        if event_tx.send(report).await.is_err() {
                                            self.flush_carryover();
                                            return Ok(());
                                        }
                                    }
                                } else if let Some(ref err) = result.error {
                                    // Summarization failed — log and fall through to truncation
                                    tracing::info!(
                                        "Summarization skipped: {} — falling back to truncation",
                                        err
                                    );
                                }
                            }
                        }
                    }

                    // Fall back to naive truncation if summarization didn't run or failed
                    if !did_summarize {
                        let history = self.conversation.all();
                        let target_budget = self.adapter.model_info().max_context_tokens * 9 / 10;
                        let result = crate::session::prompt::compact_to_budget(
                            history,
                            keep,
                            Some(target_budget),
                        );
                        compact_stats = Some(CompactHookStats {
                            message_count: history.len(),
                            preserve_recent: keep,
                            original_count: result.original_count,
                            result_count: result.compacted_count,
                            dropped_tool_results: result.dropped_tool_results,
                            condensed_assistant_turns: result.condensed_assistant_turns,
                            summarised_messages: 0,
                            tokens_before: result.tokens_before,
                            tokens_after: result.tokens_after,
                            strategy: "naive",
                        });
                        let report = if let Err(e) = self.conversation.replace_all_async(result.new_messages.clone()).await {
                            TurnEvent::Error(format!("Compaction failed: {e}"))
                        } else {
                            TurnEvent::CompactionReport {
                                new_messages: result.new_messages.clone(),
                                dropped_tool_results: result.dropped_tool_results,
                                condensed_assistant_turns: result.condensed_assistant_turns,
                                original_count: result.original_count,
                                compacted_count: result.compacted_count,
                                tokens_before: result.tokens_before,
                                tokens_after: result.tokens_after,
                            }
                        };
                        if event_tx.send(report).await.is_err() {
                            tracing::warn!("TUI event receiver dropped during /compact; exiting");
                            self.flush_carryover();
                            return Ok(());
                        }
                    }

                    // Notify lifecycle hooks that compaction finished.
                    if let Some(stats) = compact_stats {
                        self.run_compact_hook("post-compact", stats);
                    }
                }
                Some(input) = input_rx.recv() => {
                    cancelled.store(false, Ordering::SeqCst);
                    // Events stream live into `event_tx` during the turn;
                    // no batch to forward afterwards. A send failure inside
                    // the turn means the TUI dropped its receiver — flush
                    // and exit (the run loop's `input_rx.recv()` arm would
                    // otherwise spin on a closed channel anyway).
                    let result = self.run_turn(&input, &approval_tx, &cancelled, &event_tx).await;
                    if let Err(e) = result {
                        crate::send_or_warn!(event_tx.send(TurnEvent::Error(format!("Turn failed: {e}"))).await, "TurnEvent receiver dropped; discarding event");
                        tracing::warn!(
                            error = %e,
                            "TUI event receiver may be dropped while reporting turn-failure event"
                        );
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                else => break,
            }
        }
        self.flush_carryover();
        Ok(())
    }
}
