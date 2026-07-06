//! Turn event dispatch.
//!
//! Pure (non-I/O, non-async) handlers for the events the executor
//! sends to the TUI. Split out of `mod.rs` so each variant can be
//! unit-tested in isolation and so `mod.rs` can stay focused on
//! orchestration (event loop, render, key dispatch).
//!
//! All functions here take `&mut AppState` and update it. The TUI
//! never owns its own data — every visible effect of an event is a
//! mutation of `state`.
//!
//! Public entry points:
//!   - [`dispatch_turn_event`] — apply a single `TurnEvent` to `state`.
//!   - [`drain_turn_events`]   — pull every event currently queued
//!     on the executor's unbounded channel and dispatch each one.
//!     The TUI's event loop calls this once per render tick.
//!   - [`drain_approval_requests`] — same pattern for the approval
//!     channel. If a new request arrives while one is pending, the
//!     old one is **denied** before being replaced — otherwise its
//!     oneshot sender is dropped without sending, and the executor
//!     hangs forever waiting for an answer.
//!
//! Extracted in heartbeat cycle 08:36 (B2.5). The `run_event_loop`
//! match arm in the previous `mod.rs` was ~120 lines and
//! essentially untestable in place.

use crate::session::executor::{ApprovalRequest, ApprovalResponse, TurnEvent};
use crate::shared::Role;
use crate::tui::app::{AppState, ConversationEntry, PendingApproval};
use tokio::sync::mpsc;

/// Apply a single executor event to the TUI state.
///
/// This is the function the TUI's render-tick loop calls per event.
/// It is a pure mutation of `state` — no I/O, no async, no
/// channel sends. That makes every variant trivially unit-testable
/// (see `tests` below).
///
/// Mapping (event → visible effect):
/// - `Token(t)` — append to last assistant entry, or open a new one
/// - `Thinking(t)` — append to the thinking buffer (panel collapsed by default)
/// - `ToolStart { name }` — push a "🔧 name ..." entry
/// - `ToolResult { name, output }` — push a collapsed tool entry with
///   full output in the sidecar
/// - `Verification { .. }` — push a "🔍/⚠️ message" system entry
/// - `Error(e)` — push a "Error: e" system entry
/// - `CostStats { .. }` — update tokens/cost/last-turn-prompt
/// - `CompactionReport { .. }` — rebuild messages from `new_messages`
pub fn dispatch_turn_event(state: &mut AppState, ev: TurnEvent) {
    match ev {
        TurnEvent::Token(t) => {
            state.is_generating = true; // got first token — turn off spinner
                                        // Accumulate into the last assistant entry, or create one
            let role_str = "assistant".to_string();
            if let Some(last) = state.messages.last_mut() {
                if last.role == role_str {
                    last.content.push_str(&t);
                } else {
                    state.messages.push(ConversationEntry::new("assistant", t));
                }
            } else {
                state.messages.push(ConversationEntry::new("assistant", t));
            }
        }
        TurnEvent::Thinking(t) => {
            state.thinking_buffer.push(t);
        }
        TurnEvent::ToolStart { name, args: _ } => {
            state.is_generating = false; // turn ended (tool call)
            state.turn_tool_calls += 1;
            state
                .messages
                .push(ConversationEntry::new("tool", format!("🔧 {name} ...")));
        }
        TurnEvent::ToolResult { name, output, .. } => {
            // Tool outputs are stored FULL in a sidecar and shown
            // as a one-line summary by default. Ctrl+T toggles
            // collapse; per-index expansion is in state.expanded_tools.
            let (lines, bytes) = AppState::tool_output_metrics(&output, 80);
            let summary =
                format!("🔧 {name} (done) — {lines} lines, {bytes} bytes [Enter or Tab to expand]");
            // Avoid two entries per tool call: if the most recent message
            // is the matching "🔧 name ..." placeholder, replace it.
            if let Some(last) = state.messages.last() {
                if last.role == "tool" && last.content == format!("🔧 {name} ...") {
                    state.messages.pop();
                }
            }
            state
                .messages
                .push(ConversationEntry::tool(summary, output));
        }
        TurnEvent::Verification { message, success } => {
            let prefix = if success { "🔍" } else { "⚠️" };
            state.messages.push(ConversationEntry::new(
                "system",
                format!("{prefix} {message}"),
            ));
        }
        TurnEvent::Error(e) => {
            state.is_generating = false;
            state
                .messages
                .push(ConversationEntry::new("system", format!("Error: {e}")));
        }
        TurnEvent::CostStats {
            prompt_tokens,
            completion_tokens,
            turn_cost,
            cumulative_cost,
        } => {
            state.is_generating = false;
            state.turn_tool_calls = 0; // reset for next turn
            state.tokens_sent = state.tokens_sent.wrapping_add(prompt_tokens);
            state.tokens_received = state.tokens_received.wrapping_add(completion_tokens);
            state.turn_cost = turn_cost;
            state.cumulative_cost = cumulative_cost;
            // v1.2-p6: mirror the per-turn prompt size into
            // AppState so the status bar can compute the
            // budget-pressure percentage against
            // `model_info.max_context_tokens`. This is the
            // per-turn value (the API reports prompt_tokens
            // per response), not a running sum — the model
            // sees the whole conversation on every turn, so
            // the most recent prompt size is the right
            // "current context pressure" signal.
            state.last_turn_prompt_tokens = prompt_tokens;
        }
        TurnEvent::PlanComplete => {
            state.is_generating = false;
            state.messages.push(ConversationEntry::new(
                "system",
                "📐 Plan complete. The model has finished exploring and designed an implementation plan. Type /implement to allow edits and continue.".to_string(),
            ));
        }
        TurnEvent::Recovered { messages } => {
            state.messages.push(ConversationEntry::new(
                "system",
                format!("🛟 Restored {messages} message(s) from checkpoint after a corrupt session log was detected."),
            ));
        }
        TurnEvent::PullProgress {
            status,
            completed,
            total,
        } => {
            state.pull_progress = Some(crate::tui::app::PullProgress {
                status,
                completed,
                total,
            });
            state.mark_dirty();
        }
        TurnEvent::CompactionReport {
            new_messages,
            dropped_tool_results,
            condensed_assistant_turns,
            original_count,
            compacted_count,
            tokens_before,
            tokens_after,
        } => {
            // Rebuild the TUI's display list from the new
            // executor-side history. The executor is already
            // pointing at this new list; we just need to
            // mirror it in `state.messages` so the user sees
            // the compacted view.
            //
            // Mapping `Message` -> `ConversationEntry`:
            // - User/Assistant: verbatim content
            // - Tool: content is the stub marker; tool_output
            //   is None (the full output was on the prior
            //   entry, but we can't recover it from the
            //   compacted list — the TUI sidecar is per-entry
            //   and the prior entries are now gone from the
            //   message list).
            //
            // expanded_tools indices are now meaningless (the
            // message list has been re-indexed), so we clear
            // the set. The user can re-expand any entry they
            // care about with Enter / Tab.
            let mut rebuilt: Vec<ConversationEntry> = Vec::with_capacity(new_messages.len() + 1);
            for msg in &new_messages {
                let role_str = match msg.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                    Role::System => "system",
                };
                // If the message has a `tool_name`, show a
                // brief "🔧 name — marker" line so the
                // user can still see which tool produced
                // the stub.
                let content = if matches!(msg.role, Role::Tool) {
                    if let Some(name) = &msg.tool_name {
                        format!("🔧 {} — {}", name, msg.content)
                    } else {
                        format!("🔧 {}", msg.content)
                    }
                } else {
                    msg.content.clone()
                };
                rebuilt.push(ConversationEntry::new(role_str, content));
            }
            // Append a status message describing what happened
            rebuilt.push(ConversationEntry::new(
                "system",
                format!(
                    "🧹 Compacted: {original_count} → {compacted_count} messages ({tokens_before} → {tokens_after} tokens), dropped {dropped_tool_results} tool result(s), condensed {condensed_assistant_turns} assistant turn(s)."
                ),
            ));
            state.messages = rebuilt;
            state.expanded_tools.clear();
            // Search match indices are also tied to the old message
            // list; clear them so a committed search doesn't jump to
            // a stale or non-existent index after compaction.
            state.search_matches.clear();
            state.search_match_idx = 0;
            // Scroll back to the bottom so the user sees the
            // status message and the last few kept turns.
            state.auto_scroll = true;
            state.scroll_offset = 0;
            // Recompute the context-pressure estimate from the
            // post-compact message list. Without this, the status
            // bar would keep showing the PRE-compact pressure
            // (e.g. ↑120K/128K red) until the next turn's
            // CostStats event overwrote it, which can be many
            // seconds of user staring at a misleading number
            // after they explicitly asked to reduce context.
            //
            // The next CostStats will overwrite this with the
            // executor's canonical value, so the TUI never
            // disagrees with the model for long.
            state.last_turn_prompt_tokens = estimate_messages_tokens(&new_messages);
        }
    }
}

/// Local best-effort token estimate for a message list. Mirrors the
/// logic in `session::prompt::estimate_tokens` (B1.6): content
/// counted as bytes/4, tool_calls JSON-serialised and divided by 4.
/// Falls back to a small per-call constant if serialisation fails
/// (which would be a `serde_json` bug, but never panic in TUI code).
///
/// This is intentionally a local helper rather than importing the
/// closure from `prompt::build_messages` — the closure is local to
/// that function and exposing it as `pub` would leak prompt-builder
/// internals to every consumer. The values agree to within rounding
/// (both use the same 4-chars-per-token heuristic), and the next
/// `TurnEvent::CostStats` always provides the canonical value.
fn estimate_messages_tokens(messages: &[crate::shared::Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_tokens = m.content.len() / 4;
            let tool_call_tokens = m
                .tool_calls
                .as_ref()
                .map(|calls| {
                    let json = serde_json::to_string(calls).unwrap_or_default();
                    let json_tokens = json.len() / 4;
                    // Add at least 8 tokens per call as a baseline
                    // for the model to "see" the call even if the
                    // serialised JSON is empty for some reason.
                    json_tokens.max(calls.len() * 8)
                })
                .unwrap_or(0);
            content_tokens + tool_call_tokens
        })
        .sum()
}

/// Drain every event currently queued on the executor's channel
/// and dispatch each one. Non-blocking — returns when the channel
/// is empty for this tick.
///
/// The TUI calls this once per render frame so the chat panel
/// stays in sync with whatever the model is producing.
/// Hard cap on the TUI display list. Beyond this, the oldest entries are
/// evicted to prevent render perf degradation in very long sessions.
const MAX_DISPLAY_MESSAGES: usize = 2000;
/// How many messages to retain after a prune (keeps the most recent ones).
const KEEP_DISPLAY_MESSAGES: usize = 1500;

pub fn drain_turn_events(state: &mut AppState, event_rx: &mut mpsc::UnboundedReceiver<TurnEvent>) {
    let mut any = false;
    while let Ok(ev) = event_rx.try_recv() {
        dispatch_turn_event(state, ev);
        any = true;
    }
    if any {
        prune_display_messages(state);
        // Frame-pacing v2: tell the event loop that a redraw is
        // now required. We only call this when at least one event
        // was actually applied — an empty channel should not
        // mark dirty (the event loop is the one that polls the
        // channel and would otherwise needlessly keep state dirty
        // when nothing is happening).
        state.mark_dirty();
    }
}

/// Evict the oldest display messages when the list exceeds MAX_DISPLAY_MESSAGES.
///
/// Adjusts all index-based state (collapsed_messages, expanded_tools) so
/// existing UI state stays consistent. Clears search results — they'll be
/// recomputed on the next search keystroke.
fn prune_display_messages(state: &mut AppState) {
    if state.messages.len() <= MAX_DISPLAY_MESSAGES {
        return;
    }
    let n_drop = state.messages.len() - KEEP_DISPLAY_MESSAGES;
    state.messages.drain(0..n_drop);
    // Insert a sentinel so the user knows old entries were trimmed.
    state.messages.insert(
        0,
        ConversationEntry::new(
            "system",
            format!("[{n_drop} older messages pruned from display — use /save to preserve the full session]"),
        ),
    );
    // The sentinel is now at [0]; kept messages shifted by (1 - n_drop).
    // Re-map: old_idx → new_idx = old_idx - n_drop + 1  (only for old_idx >= n_drop)
    let remap = |i: usize| -> Option<usize> { i.checked_sub(n_drop).map(|x| x + 1) };
    state.collapsed_messages = state
        .collapsed_messages
        .iter()
        .filter_map(|&i| remap(i))
        .collect();
    state.expanded_tools = state
        .expanded_tools
        .iter()
        .filter_map(|&i| remap(i))
        .collect();
    // Search indices reference old message positions — clear and let next search recompute.
    state.search_matches.clear();
    state.search_match_idx = 0;
}

/// Drain every approval request currently queued. If a new request
/// arrives while one is pending, the **old** one is denied first —
/// dropping the old oneshot sender without sending would hang the
/// executor forever (it would block on `response_rx.await`).
///
/// Also clears any pending bang-approval gate, so a model approval
/// and a bang approval cannot be open at the same time (the render
/// path and the key handler otherwise disagree on which to show).
pub fn drain_approval_requests(
    state: &mut AppState,
    approval_rx: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
) {
    let mut any = false;
    while let Ok(req) = approval_rx.try_recv() {
        // Deny any existing pending approval first. With the
        // `ApprovalResponder` drop-guard, simply dropping the old
        // responder would also send `Denied`; we send explicitly here so
        // the log records why.
        if let Some(old) = state.pending_approval.take() {
            if let Some(tx) = old.responder {
                if let Err(e) = tx.send(ApprovalResponse::Denied) {
                    tracing::warn!(
                        tool = "superseded approval",
                        error = ?e,
                        "approval responder dropped before superseded-send"
                    );
                }
            }
        }
        // A model approval supersedes any pending bang gate. Without
        // this, both dialogs could be `Some` simultaneously and the
        // render path prefers one while the key handler prefers the
        // other, leaving one orphaned.
        if state.pending_bang.is_some() {
            state.pending_bang = None;
        }
        state.pending_approval = Some(PendingApproval {
            tool_name: req.tool_name.clone(),
            args: req.args.clone(),
            responder: Some(req.response),
        });
        // Reset approval scroll for each new request — a fresh dialog
        // starts at the top, regardless of where the previous one was.
        state.approval_scroll = 0;
        state.approval_max_scroll = 0;
        any = true;
    }
    if any {
        // A new approval (or a new approval replacing an old one)
        // appeared. The dialog overlay must be drawn on the next
        // frame, so mark the state dirty.
        state.mark_dirty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::executor::ApprovalResponder;
    use crate::shared::{Config, Message, Role};
    use crate::tui::app::AppState;
    use tokio::sync::mpsc;

    fn make_state() -> AppState {
        AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            Config::default(),
        )))
    }

    /// Helper to build a minimal `Message` for the compaction test.
    /// `Message` has many `Option` fields with `skip_serializing_if` —
    /// rather than re-implement its full default, we use `..Default::default()`
    /// to fill in the rest.
    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            ..Default::default()
        }
    }

    /// `Token` on an empty state creates a new assistant entry
    /// containing the token text. The first token also flips
    /// `is_generating` so the spinner stops.
    #[test]
    fn token_on_empty_creates_assistant_entry() {
        let mut s = make_state();
        assert!(!s.is_generating);
        dispatch_turn_event(&mut s, TurnEvent::Token("hi".into()));
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].role, "assistant");
        assert_eq!(s.messages[0].content, "hi");
        assert!(s.is_generating);
    }

    /// Subsequent `Token` events append to the *last* assistant
    /// entry — that's how streaming chat looks (one growing entry,
    /// not a new entry per delta).
    #[test]
    fn token_appends_to_last_assistant_entry() {
        let mut s = make_state();
        dispatch_turn_event(&mut s, TurnEvent::Token("foo".into()));
        dispatch_turn_event(&mut s, TurnEvent::Token("bar".into()));
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].content, "foobar");
    }

    /// `Thinking` accumulates into the thinking buffer. The TUI
    /// renders it on demand when the user toggles the thinking
    /// panel (Esc). One push per delta.
    #[test]
    fn thinking_appends_to_buffer() {
        let mut s = make_state();
        dispatch_turn_event(&mut s, TurnEvent::Thinking("a".into()));
        dispatch_turn_event(&mut s, TurnEvent::Thinking("b".into()));
        assert_eq!(s.thinking_buffer, vec!["a".to_string(), "b".to_string()]);
    }

    /// `ToolStart` creates a "🔧 name ..." entry and flips
    /// `is_generating` to false (the model has paused to call a tool).
    #[test]
    fn toolstart_creates_running_entry() {
        let mut s = make_state();
        dispatch_turn_event(&mut s, TurnEvent::Token("hmm".into()));
        assert!(s.is_generating);
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({"cmd": "ls"}),
            },
        );
        assert!(!s.is_generating);
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[1].role, "tool");
        assert!(s.messages[1].content.contains("bash"));
        assert!(s.messages[1].content.contains("..."));
    }

    /// `ToolResult` is the v1.1 contract: full output goes into
    /// the sidecar, the visible `content` is a one-line summary
    /// with the byte/line count. This is what makes Ctrl+T flood
    /// control possible.
    #[test]
    fn toolresult_stores_full_output_in_sidecar() {
        let mut s = make_state();
        let full = "line 1\nline 2\nline 3\n".to_string();
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: full.clone(),
                success: true,
            },
        );
        assert_eq!(s.messages.len(), 1);
        let entry = &s.messages[0];
        assert_eq!(entry.role, "tool");
        // Visible summary contains the byte count and expand hint
        assert!(entry.content.contains("bash"));
        assert!(entry.content.contains("bytes"));
        assert!(entry.content.contains("Enter or Tab to expand"));
        // Sidecar holds the full output verbatim
        assert_eq!(entry.tool_output.as_deref(), Some(full.as_str()));
    }

    /// `Verification` prefixes with 🔍 on success and ⚠️ on failure.
    /// The same code path handles both — only the prefix and the
    /// `success` flag differ.
    #[test]
    fn verification_prefixes_reflect_success() {
        let mut s = make_state();
        dispatch_turn_event(
            &mut s,
            TurnEvent::Verification {
                message: "lint clean".into(),
                success: true,
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::Verification {
                message: "found 2 warnings".into(),
                success: false,
            },
        );
        assert!(s.messages[0].content.starts_with("🔍"));
        assert!(s.messages[0].content.contains("lint clean"));
        assert!(s.messages[1].content.starts_with("⚠️"));
        assert!(s.messages[1].content.contains("found 2 warnings"));
    }

    /// `Error` is a plain "Error: ..." system message. The model
    /// saw a transport or parse failure and the turn ended.
    #[test]
    fn error_pushes_system_message_and_stops_generation() {
        let mut s = make_state();
        dispatch_turn_event(&mut s, TurnEvent::Token("partial".into()));
        assert!(s.is_generating);
        dispatch_turn_event(&mut s, TurnEvent::Error("timeout".into()));
        assert!(!s.is_generating);
        assert_eq!(s.messages.last().unwrap().role, "system");
        assert!(s.messages.last().unwrap().content.contains("timeout"));
    }

    /// `CostStats` accumulates the **cumulative** token counters
    /// (sent/received) and overwrites the per-turn cost fields.
    /// Also mirrors the per-turn `prompt_tokens` into
    /// `last_turn_prompt_tokens` so the status bar can show
    /// context pressure.
    #[test]
    fn coststats_accumulates_and_mirrors_last_turn() {
        let mut s = make_state();
        // First turn: 100 prompt, 50 completion, $0.001 / $0.001
        dispatch_turn_event(
            &mut s,
            TurnEvent::CostStats {
                prompt_tokens: 100,
                completion_tokens: 50,
                turn_cost: 0.001,
                cumulative_cost: 0.001,
            },
        );
        assert_eq!(s.tokens_sent, 100);
        assert_eq!(s.tokens_received, 50);
        assert_eq!(s.turn_cost, 0.001);
        assert_eq!(s.cumulative_cost, 0.001);
        assert_eq!(s.last_turn_prompt_tokens, 100);
        // Second turn: API reports *per-response* prompt_tokens
        // (the whole conversation as the model saw it). We
        // accumulate, but last_turn_prompt_tokens tracks the
        // most recent value (not the sum).
        dispatch_turn_event(
            &mut s,
            TurnEvent::CostStats {
                prompt_tokens: 200,
                completion_tokens: 80,
                turn_cost: 0.002,
                cumulative_cost: 0.003,
            },
        );
        assert_eq!(s.tokens_sent, 300);
        assert_eq!(s.tokens_received, 130);
        assert_eq!(s.last_turn_prompt_tokens, 200);
    }

    /// `CompactionReport` rebuilds `messages` from `new_messages`,
    /// appends a status line, clears `expanded_tools` (indices
    /// are now meaningless), and resets scroll to the bottom.
    #[test]
    fn compaction_rebuilds_messages_and_resets_scroll() {
        let mut s = make_state();
        // Pre-existing tool expansion that references index 0 —
        // must be cleared, not silently re-applied to the wrong
        // entry after the rebuild.
        s.expanded_tools.insert(0);
        s.scroll_offset = 42;
        s.auto_scroll = false;

        let new_messages = vec![msg(Role::User, "hi"), msg(Role::Assistant, "hello")];
        dispatch_turn_event(
            &mut s,
            TurnEvent::CompactionReport {
                new_messages,
                dropped_tool_results: 3,
                condensed_assistant_turns: 2,
                original_count: 10,
                compacted_count: 4,
                tokens_before: 100,
                tokens_after: 20,
            },
        );
        // The two kept messages plus the status line
        assert_eq!(s.messages.len(), 3);
        assert_eq!(s.messages[0].role, "user");
        assert_eq!(s.messages[0].content, "hi");
        assert_eq!(s.messages[1].role, "assistant");
        assert_eq!(s.messages[1].content, "hello");
        assert_eq!(s.messages[2].role, "system");
        assert!(s.messages[2].content.contains("10 → 4"));
        assert!(s.messages[2].content.contains("dropped 3"));
        assert!(s.messages[2].content.contains("condensed 2"));
        // Per-index expansion cleared (stale indices)
        assert!(s.expanded_tools.is_empty());
        // Scroll reset to bottom so the user sees the status line
        assert!(s.auto_scroll);
        assert_eq!(s.scroll_offset, 0);
    }

    /// `CompactionReport` also recomputes `last_turn_prompt_tokens`
    /// from the post-compact message list. Without this, the
    /// status bar would keep showing the PRE-compact context
    /// pressure (e.g. ↑120K/128K red) until the next turn's
    /// CostStats event overwrote it. The user explicitly asked
    /// for relief — they need to see the new pressure, not the
    /// old one.
    #[test]
    fn compaction_resets_last_turn_prompt_tokens_to_post_compact_estimate() {
        let mut s = make_state();
        // Pre-compact: a 30K token context (the kind of pressure
        // /compact exists to relieve).
        s.last_turn_prompt_tokens = 30_000;
        // Post-compact: just two short messages, ~5 tokens total.
        let new_messages = vec![msg(Role::User, "hi"), msg(Role::Assistant, "hello")];
        dispatch_turn_event(
            &mut s,
            TurnEvent::CompactionReport {
                new_messages,
                dropped_tool_results: 0,
                condensed_assistant_turns: 0,
                original_count: 10,
                compacted_count: 2,
                tokens_before: 100,
                tokens_after: 2,
            },
        );
        // "hi" (2) + "hello" (5) = 7 chars, /4 = 1 token each, + 1 = 2.
        // The exact number isn't load-bearing — what matters is
        // that it dropped from 30_000 to something much smaller.
        assert!(
            s.last_turn_prompt_tokens < 1_000,
            "post-compact estimate should be near-zero, got {}",
            s.last_turn_prompt_tokens
        );
    }

    /// The post-compact estimate must count `tool_calls` JSON.
    /// A 50k-char `old_string` in an `edit_file` call (serialised
    /// as JSON) is what the model sees on the wire — ignoring it
    /// would re-introduce the B1.6 lie that this whole family of
    /// fixes exists to prevent.
    #[test]
    fn compaction_estimate_counts_tool_calls() {
        use crate::shared::ToolInvocation;
        let mut s = make_state();
        s.last_turn_prompt_tokens = 0;
        // An assistant message with a 4000-char tool call
        // (4k chars / 4 = 1k tokens for the call alone).
        let long_args = serde_json::json!({
            "old_string": "x".repeat(4000),
            "new_string": "y".repeat(4000),
        });
        let big_message = Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ToolInvocation {
                id: "call_1".to_string(),
                name: "edit_file".to_string(),
                arguments: long_args,
            }]),
            ..Default::default()
        };
        dispatch_turn_event(
            &mut s,
            TurnEvent::CompactionReport {
                new_messages: vec![big_message],
                dropped_tool_results: 0,
                condensed_assistant_turns: 0,
                original_count: 1,
                compacted_count: 1,
                tokens_before: 2000,
                tokens_after: 2000,
            },
        );
        // The tool call alone is ~2k tokens (8k chars / 4).
        // A 0-token estimate would mean we ignored tool_calls.
        assert!(
            s.last_turn_prompt_tokens > 1_000,
            "post-compact estimate must count tool_calls, got {}",
            s.last_turn_prompt_tokens
        );
    }

    /// The post-compact estimate is strictly the *post*-compact
    /// value, never the pre. This is the regression guard for the
    /// exact bug: a user at 110K/128K (red) issues `/compact` and
    /// expects to see the green/lower number, not the red one.
    #[test]
    fn compaction_estimate_uses_post_compact_size_not_pre() {
        let mut s = make_state();
        // Pretend we were at 110K (deep red).
        s.last_turn_prompt_tokens = 110_000;
        // Post-compact: 4 messages, 200 chars each = ~200 tokens.
        let new_messages = vec![
            msg(Role::User, "a".repeat(200).as_str()),
            msg(Role::Assistant, "b".repeat(200).as_str()),
            msg(Role::User, "c".repeat(200).as_str()),
            msg(Role::Assistant, "d".repeat(200).as_str()),
        ];
        dispatch_turn_event(
            &mut s,
            TurnEvent::CompactionReport {
                new_messages,
                dropped_tool_results: 20,
                condensed_assistant_turns: 5,
                original_count: 50,
                compacted_count: 4,
                tokens_before: 110_000,
                tokens_after: 200,
            },
        );
        // 4 messages * 200 chars / 4 = 200 tokens total.
        // The pre-compact 110K must NOT survive.
        assert!(
            s.last_turn_prompt_tokens < 1_000,
            "post-compact estimate leaked the pre-compact value: {}",
            s.last_turn_prompt_tokens
        );
    }

    /// `drain_turn_events` pulls every event in queue order and
    /// applies each one. After the call the channel is empty.
    #[test]
    fn drain_turn_events_pulls_all() {
        let mut s = make_state();
        let (tx, mut rx) = mpsc::unbounded_channel::<TurnEvent>();
        tx.send(TurnEvent::Token("a".into())).unwrap();
        tx.send(TurnEvent::Token("b".into())).unwrap();
        tx.send(TurnEvent::Token("c".into())).unwrap();
        drain_turn_events(&mut s, &mut rx);
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].content, "abc");
        // Channel is drained — next call is a no-op
        drain_turn_events(&mut s, &mut rx);
        assert_eq!(s.messages.len(), 1);
    }

    /// `drain_approval_requests` replaces the pending approval
    /// when a new one arrives, but **denies the old one first** —
    /// the previous audit found that dropping the old oneshot
    /// sender hangs the executor forever.
    #[tokio::test]
    async fn drain_replaces_pending_and_denies_old() {
        let mut s = make_state();
        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

        // First request: responder is the oneshot that the
        // executor is awaiting. If we drop it without sending,
        // the executor hangs forever.
        let (old_tx, mut old_rx) = tokio::sync::oneshot::channel::<ApprovalResponse>();
        approval_tx
            .send(ApprovalRequest {
                tool_name: "bash".into(),
                args: serde_json::json!({"cmd": "rm -rf /"}),
                response: ApprovalResponder::new(old_tx),
            })
            .unwrap();

        // Second request comes in while the first is still pending
        let (new_tx, _new_rx) = tokio::sync::oneshot::channel::<ApprovalResponse>();
        approval_tx
            .send(ApprovalRequest {
                tool_name: "edit_file".into(),
                args: serde_json::json!({"path": "/etc/passwd"}),
                response: ApprovalResponder::new(new_tx),
            })
            .unwrap();

        drain_approval_requests(&mut s, &mut approval_rx);

        // Old responder received Denied before being dropped.
        let old_answer: Option<ApprovalResponse> = old_rx.try_recv().ok();
        assert_eq!(old_answer, Some(ApprovalResponse::Denied));
        // The pending approval is now the new one
        assert!(s.pending_approval.is_some());
        assert_eq!(s.pending_approval.as_ref().unwrap().tool_name, "edit_file");
    }

    /// `PullProgress` updates `state.pull_progress` and marks state dirty
    /// so the TUI re-renders the progress bar.
    #[test]
    fn pull_progress_updates_state_and_marks_dirty() {
        let mut s = make_state();
        s.dirty = false;
        dispatch_turn_event(
            &mut s,
            TurnEvent::PullProgress {
                status: "pulling manifest".into(),
                completed: None,
                total: None,
            },
        );
        let p = s.pull_progress.as_ref().expect("pull_progress set");
        assert_eq!(p.status, "pulling manifest");
        assert!(p.completed.is_none());
        assert!(p.total.is_none());
        assert!(s.dirty, "progress event should mark state dirty");

        // A later progress event overwrites the snapshot.
        dispatch_turn_event(
            &mut s,
            TurnEvent::PullProgress {
                status: "downloading".into(),
                completed: Some(128 * 1024 * 1024),
                total: Some(512 * 1024 * 1024),
            },
        );
        let p = s.pull_progress.as_ref().expect("pull_progress still set");
        assert_eq!(p.status, "downloading");
        assert_eq!(p.completed, Some(128 * 1024 * 1024));
        assert_eq!(p.total, Some(512 * 1024 * 1024));
    }
}
