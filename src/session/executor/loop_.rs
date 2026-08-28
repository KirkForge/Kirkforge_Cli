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

/// Sliding-window detector for repeated tool errors.
///
/// A doom loop is "the same tool failing with the same error N turns
/// in a row" — the symptom of a model that is stuck trying the same
/// broken approach. Tracking is intentionally narrow: only tool
/// errors count, and the comparison is on a (tool, error) pair so a
/// tool that fails for one reason and then for a different reason
/// is treated as a fresh start.
///
/// The detector is pure and synchronous; the caller passes the result
/// to the event bus and the metrics recorder. The threshold (3 hits
/// within the last 5 observations) is small enough to catch a real
/// loop quickly and large enough to ignore one-off retries.
pub struct DoomLoopTracker {
    /// Sliding window of (tool, error) pairs. Bounded — old entries
    /// are evicted as new ones come in. The bound is `WINDOW` so the
    /// structure is a tiny ring buffer.
    window: Vec<(String, String)>,
    /// Consecutive identical (tool, error) pair count at the tail.
    /// Reset to 0 whenever the latest observation differs from the
    /// previous one. Used to skip recomputing the count on every push.
    run: usize,
    /// Last error text we emitted a doom event for — used to avoid
    /// re-emitting on every identical error after the threshold is
    /// crossed.
    last_emit: Option<String>,
}

impl DoomLoopTracker {
    /// Number of recent tool-error observations kept in the sliding
    /// window. Larger means the threshold scan is slower; 5 is enough
    /// to span the threshold (3) with two slots of context.
    pub const WINDOW: usize = 5;
    /// Number of identical errors in a row required to flag a doom
    /// loop. Empirically: 1 retry is normal, 2 retries is a sign of
    /// confusion, 3 retries is a loop.
    pub const THRESHOLD: usize = 3;
    /// Truncation length for the persisted `last_error` so a long
    /// stack trace does not blow up the metrics log.
    pub const ERROR_TRUNCATE: usize = 200;

    pub fn new() -> Self {
        Self {
            window: Vec::with_capacity(Self::WINDOW),
            run: 0,
            last_emit: None,
        }
    }

    /// Record one tool error observation. Returns `Some(DoomHit)` if
    /// this observation crosses the threshold (i.e. the count is
    /// `>= THRESHOLD` and the latest run is different from the last
    /// one we emitted for, to avoid spamming the same event every
    /// subsequent identical error).
    pub fn observe(&mut self, tool: &str, error: &str) -> Option<DoomHit> {
        let truncated = if error.len() > Self::ERROR_TRUNCATE {
            let mut t = String::with_capacity(Self::ERROR_TRUNCATE + 1);
            // Walk back to a UTF-8 char boundary so a multibyte char
            // straddling the cut point doesn't panic the turn (WO 43.25).
            let mut end = Self::ERROR_TRUNCATE;
            while !error.is_char_boundary(end) {
                end -= 1;
            }
            t.push_str(&error[..end]);
            t.push('…');
            t
        } else {
            error.to_string()
        };

        // Update the consecutive-run count.
        match self.window.last() {
            Some((last_tool, last_err)) if last_tool == tool && last_err == &truncated => {
                self.run = self.run.saturating_add(1);
            }
            _ => {
                self.run = 1;
            }
        }

        // Slide the window.
        if self.window.len() == Self::WINDOW {
            self.window.remove(0);
        }
        self.window.push((tool.to_string(), truncated.clone()));

        // We only consider the LATEST run length (consecutive identical
        // errors). A long-ago identical error is not a doom loop — it
        // could be two completely separate failures that happen to use
        // the same tool/error text.
        if self.run >= Self::THRESHOLD {
            if self.last_emit.as_deref() == Some(truncated.as_str()) {
                return None;
            }
            self.last_emit = Some(truncated.clone());
            return Some(DoomHit {
                count: self.run,
                tool: tool.to_string(),
                last_error: truncated,
            });
        }
        None
    }

    /// Reset the tracker (e.g. on a successful tool call, or on
    /// user-initiated break). Called by the executor when the model
    /// produces a non-error outcome so the next failure starts
    /// fresh.
    pub fn reset(&mut self) {
        self.window.clear();
        self.run = 0;
        self.last_emit = None;
    }
}

impl Default for DoomLoopTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// A confirmed doom loop crossing. Returned by
/// [`DoomLoopTracker::observe`] and consumed by the executor (which
/// emits the `TurnEvent::DoomLoopDetected` event and the
/// `MetricEvent::DoomLoop` metric).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoomHit {
    pub count: usize,
    pub tool: String,
    pub last_error: String,
}

impl Executor {
    // reason: each arg is a distinct mpsc channel end; grouping would obscure the wiring.
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
        mut plugin_reload_rx: mpsc::UnboundedReceiver<kf_plugin_host::PluginRegistry>,
    ) -> anyhow::Result<()> {
        let cancelled = Arc::new(AtomicBool::new(false));

        // WO 36.4: the parent session's live cancel token. The slot holds
        // the CURRENT turn's token; cancelled alongside the flag so an Esc
        // aborts in-flight streams (WO 36.3) and cascades into live per-tool
        // child tokens, instead of the flag-only snapshot-at-dispatch
        // semantics (WO 15.7). Tokens are one-shot, so every new turn
        // installs a fresh one below.
        let turn_cancel = Arc::new(std::sync::Mutex::new(
            tokio_util::sync::CancellationToken::new(),
        ));

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
                            cfg.model.summarize_enabled,
                            cfg.model.summarize_model.clone(),
                            cfg.model.ollama_host.clone(),
                            cfg.session.preserve_recent_messages,
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

                    // Pin unresolved verifier findings in compaction tail (WO 22.6-R6).
                    if let Some(findings) =
                        crate::session::prompt::compaction::extract_unresolved_verifier_findings(&history)
                    {
                        self.conversation
                            .append_async(Message {
                                role: Role::System,
                                content: findings,
                                ..Default::default()
                            })
                            .await?;
                    }

                    // Notify lifecycle hooks that compaction finished.
                    if let Some(stats) = compact_stats {
                        self.run_compact_hook("post-compact", stats);
                    }
                }
                Some(input) = input_rx.recv() => {
                    // WO 38.5 / WO 38.4 #3 (drain-before-install): an Esc
                    // queued before this input can only refer to a turn
                    // that no longer exists. The old independent watcher
                    // could process it after the fresh token install and
                    // kill the new turn instantly; draining here makes
                    // stale Escs deterministic no-ops. Only Escs arriving
                    // while the turn is live (the select below) cancel.
                    while cancel_rx.try_recv().is_ok() {}
                    cancelled.store(false, Ordering::SeqCst);
                    // WO 36.4: install a fresh per-turn token (one-shot,
                    // so the previous turn's cancel must not leak into
                    // this one). Attached via `set_cancel_token`, the
                    // stream-await race (WO 36.3) and per-tool child
                    // tokens are live for this turn. An Esc racing this
                    // swap still exits promptly: the select below polls
                    // `cancel_rx` while the turn streams, sets the flag,
                    // and cancels the slot token.
                    let turn_token = {
                        let mut slot =
                            turn_cancel.lock().unwrap_or_else(|e| e.into_inner());
                        *slot = tokio_util::sync::CancellationToken::new();
                        slot.clone()
                    };
                    self.set_cancel_token(Some(turn_token));
                    // Events stream live into `event_tx` during the turn;
                    // no batch to forward afterwards. The turn is raced
                    // against `cancel_rx` so a mid-stream Esc aborts
                    // in-flight work (WO 36.3/36.4) without an independent
                    // watcher task. `biased` toward the turn arm: when the
                    // turn has ALREADY completed, a queued Esc belongs to
                    // the next turn's drain, not to a spurious
                    // "Generation cancelled" message.
                    let result = {
                        let mut turn = std::pin::pin!(self.run_turn(
                            &input,
                            &approval_tx,
                            &cancelled,
                            &event_tx
                        ));
                        loop {
                            tokio::select! {
                                biased;
                                r = &mut turn => break r,
                                Some(()) = cancel_rx.recv() => {
                                    cancelled.store(true, Ordering::SeqCst);
                                    turn_cancel
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .cancel();
                                    crate::emit!(event_tx, TurnEvent::Token( "\n⚠️ Generation cancelled\n".into() ));
                                }
                            }
                        }
                    };
                    self.set_cancel_token(None);
                    if let Err(e) = result {
                        // WO 38.5 P0 (also TUI audit P1-1): a turn-fatal
                        // error (429/5xx past retries, 401/403/404, missing
                        // key, checkpoint IO) costs ONE turn, not the
                        // session. Emit the error, let the TUI clear its
                        // busy state, and keep the loop alive so the user
                        // can retry. Exit is reserved for channel closure
                        // (the `else => break` arm below).
                        crate::emit!(event_tx, TurnEvent::Error(format!("Turn failed: {e}")));
                        crate::emit!(event_tx, TurnEvent::TurnComplete);
                    }
                }
                else => break,
            }
        }
        self.flush_carryover();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three identical tool errors in a row should fire exactly
    /// once. The `run` count should climb to 3 and the returned
    /// `DoomHit` should reflect that.
    #[test]
    fn doom_loop_fires_on_three_identical_errors() {
        let mut t = DoomLoopTracker::new();
        assert!(t.observe("bash", "boom").is_none());
        assert!(t.observe("bash", "boom").is_none());
        let hit = t
            .observe("bash", "boom")
            .expect("third identical error should fire");
        assert_eq!(hit.count, 3);
        assert_eq!(hit.tool, "bash");
        assert!(hit.last_error.starts_with("boom"));
    }

    /// A different error resets the consecutive run, so the next
    /// identical error starts the count over. A doom loop is about
    /// repetition, not about the same tool failing for different
    /// reasons.
    #[test]
    fn doom_loop_resets_on_different_error() {
        let mut t = DoomLoopTracker::new();
        t.observe("bash", "boom");
        t.observe("bash", "boom");
        // Different error text breaks the run — count drops back to 1,
        // so the threshold is not crossed.
        assert!(t.observe("bash", "different").is_none());
        // Two more identical errors in a row DO cross the threshold,
        // proving the run counter restarted.
        t.observe("bash", "different");
        let hit = t.observe("bash", "different").expect("third after reset");
        assert_eq!(
            hit.count, 3,
            "run length restarts at 1 after a different error"
        );
    }

    /// A different tool also resets the run.
    #[test]
    fn doom_loop_resets_on_different_tool() {
        let mut t = DoomLoopTracker::new();
        t.observe("bash", "boom");
        t.observe("bash", "boom");
        assert!(t.observe("grep", "boom").is_none());
        t.observe("grep", "boom");
        let hit = t.observe("grep", "boom").expect("third after reset");
        assert_eq!(hit.count, 3);
    }

    /// A successful tool call (caller invokes `reset()`) clears the
    /// tracker so the next failure starts fresh.
    #[test]
    fn doom_loop_resets_on_success() {
        let mut t = DoomLoopTracker::new();
        t.observe("bash", "boom");
        t.observe("bash", "boom");
        t.reset();
        // After reset, the first observation re-starts the run at 1.
        assert!(t.observe("bash", "boom").is_none());
        t.observe("bash", "boom");
        let hit = t.observe("bash", "boom").expect("third after reset");
        assert_eq!(hit.count, 3);
    }

    /// After the threshold is crossed, the tracker suppresses
    /// subsequent identical errors so the TUI / metrics log are
    /// not spammed. The suppression is keyed on the error text, so
    /// a new error can re-fire immediately.
    #[test]
    fn doom_loop_does_not_respam_same_error() {
        let mut t = DoomLoopTracker::new();
        t.observe("bash", "boom");
        t.observe("bash", "boom");
        let hit = t
            .observe("bash", "boom")
            .expect("third identical error should fire");
        assert_eq!(hit.count, 3);
        // Fourth identical error: no new hit, because we already
        // emitted for this text.
        assert!(t.observe("bash", "boom").is_none());
        // A new error text starts a fresh run.
        assert!(t.observe("bash", "bam").is_none());
        t.observe("bash", "bam");
        let hit = t.observe("bash", "bam").expect("third after reset");
        assert_eq!(hit.count, 3);
    }

    /// A long error message is truncated to keep the metrics log
    /// line and the TUI banner readable.
    #[test]
    fn doom_loop_truncates_long_error() {
        let mut t = DoomLoopTracker::new();
        let long = "x".repeat(500);
        t.observe("bash", &long);
        t.observe("bash", &long);
        let hit = t
            .observe("bash", &long)
            .expect("third identical error should fire");
        // `DoomLoopTracker::ERROR_TRUNCATE + 1` for the trailing `…`.
        let expected_len = DoomLoopTracker::ERROR_TRUNCATE + "…".len();
        assert_eq!(hit.last_error.len(), expected_len);
        assert!(hit.last_error.ends_with('…'));
    }

    /// A non-ASCII error whose cut point lands inside a multibyte
    /// char must not panic and must produce char-aligned output
    /// (WO 43.25). Before the fix, `&error[..ERROR_TRUNCATE]`
    /// panicked because the byte index split a 4-byte char.
    #[test]
    fn doom_loop_truncates_non_ascii_without_panic() {
        let mut t = DoomLoopTracker::new();
        // `🎉` is 4 bytes; place it so the 200-byte cut lands mid-char.
        let prefix = "a".repeat(197);
        let error = format!("{prefix}🎉bcdef");
        assert!(error.len() > DoomLoopTracker::ERROR_TRUNCATE);
        assert!(!error.is_char_boundary(DoomLoopTracker::ERROR_TRUNCATE));
        // Must not panic.
        t.observe("bash", &error);
        t.observe("bash", &error);
        let hit = t
            .observe("bash", &error)
            .expect("third identical error should fire");
        // Output is valid UTF-8, ends with the ellipsis, and the
        // body before it is char-aligned (no sliced char).
        assert!(hit.last_error.ends_with('…'));
        let body = &hit.last_error[..hit.last_error.len() - "…".len()];
        assert!(body.is_char_boundary(body.len()));
        assert!(body.starts_with(&prefix));
    }
}
