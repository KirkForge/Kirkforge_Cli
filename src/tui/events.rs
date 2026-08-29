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
//!     on the executor's channel and dispatch each one, including the
//!     event consumed by the `select!` arm (passed as `first`). The
//!     TUI's event loop calls this once per render tick.
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
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use std::collections::VecDeque;
use tokio::sync::mpsc;

/// Apply a crossterm mouse event to the TUI state.
///
/// Pure mutation of `state` — no I/O, no async — so it is unit-testable
/// in isolation (see `tests` below). Extracted from the inline handler
/// that previously lived in `tui::dispatch_kb_events` (WO 27.7).
///
/// Behavior:
/// - `ScrollDown` / `ScrollUp` — scroll the chat view by 3 rows
///   (unchanged from the prior inline handler). ScrollUp also turns
///   auto-follow off so the view sticks where the user scrolled.
/// - `Down(Left)` — click. WO 34.1 removed the top tab bar, so row 0
///   is now the header (not click-to-switch-tab); a click anywhere
///   outside the input rect "grabs" the chat for drag-scroll:
///   auto-follow is turned off and the row is recorded as the drag
///   baseline. A click inside the input rect positions the text cursor.
/// - `Drag(Left)` — drag-scroll the chat by the row delta since the
///   last event. Natural scrolling: drag down moves content down
///   (offset shrinks), drag up reveals later content.
/// - `Up(Left)` — end drag (clears the baseline).
///
/// ponytail: click-to-position the text cursor inside the prompt is
/// implemented via `last_input_rect` stored on `UiState` during render
/// (WO 32.12). The handler hit-tests the click against the stored rect,
/// maps the click column (minus the 1-char left border) to a char index
/// on the current line, and calls `set_cursor_line_col`. Clicks outside
/// the input rect keep the existing behavior (drag-scroll).
/// Ceiling: the mapping assumes the click column maps 1:1 to a char
/// column (no horizontal scroll offset); the input box does not
/// currently scroll horizontally, so this holds.
pub fn handle_mouse_event(state: &mut AppState, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            state.conversation.scroll_offset =
                (state.conversation.scroll_offset + 3).min(state.conversation.max_scroll);
        }
        MouseEventKind::ScrollUp => {
            state.conversation.auto_scroll = false;
            state.conversation.scroll_offset = state.conversation.scroll_offset.saturating_sub(3);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(rect) = state.ui.last_input_rect {
                if mouse.row >= rect.y && mouse.row < rect.y + rect.height {
                    let line = (mouse.row - rect.y).saturating_sub(1) as usize;
                    let col = (mouse.column as usize).saturating_sub(rect.x as usize + 1);
                    state.set_cursor_line_col(line, col);
                    state.mark_dirty();
                    return;
                }
            }
            // No tab bar anymore (WO 34.1) — every non-input click grabs
            // the chat for drag-scroll, including row 0 (the header).
            state.conversation.auto_scroll = false;
            state.ui.mouse_drag_row = Some(mouse.row);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(last) = state.ui.mouse_drag_row {
                let delta = mouse.row as isize - last as isize;
                state.conversation.auto_scroll = false;
                let off = state.conversation.scroll_offset as isize;
                // Natural scroll: content follows the drag direction.
                let new_off = off - delta;
                let clamped = new_off.max(0).min(state.conversation.max_scroll as isize) as usize;
                state.conversation.scroll_offset = clamped;
            }
            state.ui.mouse_drag_row = Some(mouse.row);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            state.ui.mouse_drag_row = None;
        }
        _ => {}
    }
    state.mark_dirty();
}

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
/// - `TurnComplete` — clear is_generating/streaming/continuation (terminal)
/// - `CompactionReport { .. }` — rebuild messages from `new_messages`
pub fn dispatch_turn_event(state: &mut AppState, ev: TurnEvent) {
    match ev {
        TurnEvent::Token(t) => {
            state.generation.is_generating = true;
            // A new turn is running — late-chunk drop no longer
            // applies (WO 48.43).
            state.generation.turn_finished = false;
            // Append to the LAST ASSISTANT entry in the conversation,
            // even if tool entries were inserted after it. This prevents
            // tool calls from splitting the assistant's text into
            // fragments — all text from one turn stays in ONE message.
            // Tool entries appear between the message and the next one
            // in the data model, but the text content is unified.
            //
            // "Current turn" detection: the assistant entry must still be
            // `streaming` (set when the first token of the turn arrived,
            // cleared by `TurnComplete` at turn end). This is the
            // within-turn marker — it stays true across ToolStart/
            // ToolResult (which don't clear it), so a "text → tool →
            // more text" turn keeps appending to the same entry. After
            // `TurnComplete` clears `streaming`, a new turn's first
            // Token correctly opens a fresh entry instead of bleeding
            // into the prior turn's assistant message. The old heuristic
            // (all entries after the assistant are tool/system) caused
            // cross-turn bleed: a new turn's text was appended to the
            // prior turn's assistant because the tool entries from the
            // prior turn were still the last entries.
            const ASSISTANT: &str = "assistant";
            let last_assistant_idx = state
                .conversation
                .messages
                .iter()
                .rposition(|m| m.role == ASSISTANT);
            if let Some(idx) = last_assistant_idx {
                let is_current_turn = state.conversation.messages[idx].streaming;
                if is_current_turn {
                    state.conversation.messages[idx].content.push_str(&t);
                    state.conversation.messages[idx].streaming = true;
                    state.conversation.messages[idx].bump_version();
                } else {
                    let mut entry = ConversationEntry::new(ASSISTANT, t);
                    entry.streaming = true;
                    state.conversation.messages.push_back(entry);
                }
            } else {
                let mut entry = ConversationEntry::new(ASSISTANT, t);
                entry.streaming = true;
                state.conversation.messages.push_back(entry);
            }
        }
        TurnEvent::Thinking(t) => {
            state.generation.turn_finished = false;
            state.generation.thinking_buffer.push(t);
            // Bound the buffer: long reasoning-model sessions emit
            // thousands of chunks and the render path joins + re-wraps
            // the whole thing every frame. Drop the oldest chunks
            // until the joined byte length is under the tail budget.
            // The latest reasoning is what the user wants visible;
            // the head is stale context (WO 38.11).
            trim_thinking_buffer_tail(&mut state.generation.thinking_buffer);
        }
        TurnEvent::ToolStart {
            name,
            args: _,
            call_id,
        } => {
            state.generation.is_generating = false; // turn ended (tool call)
            state.generation.turn_tool_calls += 1;
            // A tool call means an executor turn is running — clear
            // the turn boundary (WO 48.43).
            state.generation.turn_finished = false;
            // WO 48.31: if chunks for this call_id already created a
            // card (they raced ahead of ToolStart), keep it — pushing a
            // second would strand the first as a ghost spinner.
            if !call_id.is_empty() {
                if let Some(&idx) = state.conversation.streaming_tool_index.get(&call_id) {
                    if state
                        .conversation
                        .messages
                        .get(idx)
                        .is_some_and(|m| m.role == "tool")
                    {
                        if let Some(m) = state.conversation.messages.get_mut(idx) {
                            m.streaming = true;
                            m.bump_version();
                        }
                        state.mark_dirty();
                        return;
                    }
                }
            }
            // The placeholder is a streaming card: the tool is in-flight
            // and PTY chunks may append to it before ToolResult finalizes
            // (WO 44.38). Marking it streaming lets BashPartialOutput's
            // "append to last streaming tool card" path find it.
            let mut entry = ConversationEntry::new("tool", format!("🔧 {name} ..."));
            entry.streaming = true;
            state.conversation.messages.push_back(entry);
            // WO 48.31: register the card under its call_id so PTY
            // chunks and the final ToolResult route exactly — parallel
            // same-name calls keep separate cards.
            if !call_id.is_empty() {
                let idx = state.conversation.messages.len() - 1;
                state.conversation.streaming_tool_index.insert(call_id, idx);
            }
        }
        TurnEvent::ToolResult {
            name,
            output,
            call_id,
            ..
        } => {
            // Tool outputs are stored FULL in a sidecar and shown
            // as a one-line summary by default. Ctrl+T toggles
            // collapse; per-index expansion is in state.conversation.expanded_tools.
            let (lines, bytes) = AppState::tool_output_metrics(&output, 80);
            let summary =
                format!("🔧 {name} (done) — {lines} lines, {bytes} bytes [Enter or Tab to expand]");
            // Avoid two entries per tool call: replace this tool's
            // placeholder card. Exact call_id pairing (WO 48.31); the
            // legacy name-based path remains for events with an empty
            // call_id (old replay traces) — guarded to never touch
            // id-registered cards (WO 48.43).
            finalize_streaming_placeholder(state, &name, &call_id);
            state
                .conversation
                .messages
                .push_back(ConversationEntry::tool(summary, output));
        }
        TurnEvent::Verification {
            message,
            outcome,
            file,
            line,
        } => {
            // WO 45.36: `outcome` carries the typed Verdict discriminant.
            // The icon preserves the prior success/failure split: 🔍 for
            // non-failure (Clean/Skipped/Fixed/Suggestion), ⚠️ for Failed.
            let prefix = if outcome.is_success() {
                "🔍"
            } else {
                "⚠️"
            };
            let loc = match (file, line) {
                (Some(f), Some(l)) => format!(" {}:{}:", f.display(), l),
                (Some(f), None) => format!(" {}:", f.display()),
                _ => String::new(),
            };
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new(
                    "system",
                    format!("{prefix}{loc} {message}"),
                ));
        }
        TurnEvent::Error(e) => {
            // A transport/parse failure ends the turn just like
            // TurnComplete, but the prior arm only cleared
            // `is_generating`. That left `continuation` stuck in the
            // status bar ("⟳ 3/5" after the round that errored) and
            // the `streaming` flag lit on the partial assistant
            // entry, so the card kept rendering a spinner forever.
            // Mirror TurnComplete's cleanup so the UI reflects the
            // real terminal state (WO 38.11).
            state.generation.is_generating = false;
            state.generation.turn_tool_calls = 0;
            state.generation.continuation = None;
            for msg in &mut state.conversation.messages {
                if msg.role == "assistant" {
                    msg.streaming = false;
                    msg.bump_version();
                }
            }
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("system", format!("Error: {e}")));
        }
        TurnEvent::CostStats {
            prompt_tokens,
            completion_tokens,
            turn_cost,
            cumulative_cost,
        } => {
            // Budget accounting only. Turn finalization (clearing
            // is_generating / streaming) is handled by TurnComplete, which
            // fires on every turn end regardless of whether the provider
            // supplied usage data. Coupling finalization to CostStats left
            // the TUI stuck "generating" for providers that emit
            // `Done { usage: None }` (e.g. Anthropic SSE fallback).
            state.budget.tokens_sent = state.budget.tokens_sent.wrapping_add(prompt_tokens);
            state.budget.tokens_received =
                state.budget.tokens_received.wrapping_add(completion_tokens);
            state.budget.turn_cost = turn_cost;
            state.budget.cumulative_cost = cumulative_cost;
            // v1.2-p6: mirror the per-turn prompt size into
            // AppState so the status bar can compute the
            // budget-pressure percentage against
            // `model_info.max_context_tokens`. This is the
            // per-turn value (the API reports prompt_tokens per
            // response), not a running sum — the model sees the
            // whole conversation on every turn, so the most recent
            // prompt size is the right "current context pressure"
            // signal.
            state.budget.last_turn_prompt_tokens = prompt_tokens;
        }
        TurnEvent::CacheStats {
            cached_tokens,
            prompt_tokens,
            stem_tokens,
        } => {
            state.budget.cached_tokens = state.budget.cached_tokens.wrapping_add(cached_tokens);
            state.budget.stem_tokens = stem_tokens;
            // Mirror the latest cache ratio for the status bar. If the
            // provider reports no prompt tokens, treat the turn as zero
            // cache hit to avoid division by zero.
            state.budget.cache_hit_ratio = if prompt_tokens > 0 {
                cached_tokens as f64 / prompt_tokens as f64
            } else {
                0.0
            };
        }
        TurnEvent::PlanComplete => {
            state.generation.is_generating = false;
            state.conversation.messages.push_back(ConversationEntry::new(
                "system",
                "📐 Plan complete. The model has finished exploring and designed an implementation plan. Type /implement to allow edits and continue.".to_string(),
            ));
        }
        TurnEvent::Recovered { messages } => {
            state.conversation.messages.push_back(ConversationEntry::new(
                "system",
                format!("🛟 Restored {messages} message(s) from checkpoint after a corrupt session log was detected."),
            ));
        }
        TurnEvent::PullProgress {
            status,
            completed,
            total,
        } => {
            state.provider.pull_progress = Some(crate::tui::app::PullProgress {
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
            // mirror it in `state.conversation.messages` so the user sees
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
            let mut rebuilt: VecDeque<ConversationEntry> =
                VecDeque::with_capacity(new_messages.len() + 1);
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
                rebuilt.push_back(ConversationEntry::new(role_str, content));
            }
            // Append a status message describing what happened
            rebuilt.push_back(ConversationEntry::new(
                "system",
                format!(
                    "🧹 Compacted: {original_count} → {compacted_count} messages ({tokens_before} → {tokens_after} tokens), dropped {dropped_tool_results} tool result(s), condensed {condensed_assistant_turns} assistant turn(s)."
                ),
            ));
            state.conversation.messages = rebuilt;
            state.conversation.expanded_tools.clear();
            // Card registrations pointed into the pre-compaction list
            // (WO 48.31) — drop them with the old indexes.
            state.conversation.streaming_tool_index.clear();
            // The render cache is keyed on (idx, entry.version). The
            // rebuild above replaced every entry with a fresh
            // `ConversationEntry` (version=0); cache slots still hold
            // the OLD (version, lines) for the slot's position. Without
            // invalidation, the chat panel serves pre-compaction lines
            // at the new indices (WO 38.11).
            state.conversation.chat_render_cache.clear_entries();
            // Search match indices are also tied to the old message
            // list; clear them so a committed search doesn't jump to
            // a stale or non-existent index after compaction.
            state.search.matches.clear();
            state.search.match_idx = 0;
            // Scroll back to the bottom so the user sees the
            // status message and the last few kept turns.
            state.conversation.auto_scroll = true;
            state.conversation.scroll_offset = 0;
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
            state.budget.last_turn_prompt_tokens = estimate_messages_tokens(&new_messages);
        }
        TurnEvent::DoomLoopDetected {
            count,
            tool,
            last_error,
        } => {
            // Surface the doom-loop condition in TUI state so the
            // banner widget can render. The banner is shown when
            // `count >= 3 && !acknowledged`; acknowledgement is a
            // user action (break/plan/continue) handled by the
            // banner's key handler.
            state.doom.doom_loop = Some(crate::tui::app::DoomLoopState {
                count,
                tool: tool.clone(),
                last_error: last_error.clone(),
                acknowledged: false,
            });
            state.conversation.messages.push_back(ConversationEntry::new(
                "system",
                format!(
                    "🔁 Doom loop detected: {tool} has failed {count} times in a row with: {last_error}"
                ),
            ));
            state.mark_dirty();
        }
        TurnEvent::DoomLoopRemediation { action, hits } => {
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new(
                    "system",
                    format!("⚠️ Doom-loop circuit breaker: {action} after {hits} hits"),
                ));
            state.mark_dirty();
        }
        TurnEvent::ContinuationRound { round, max } => {
            state.generation.continuation = Some((round, max));
            state.mark_dirty();
        }
        TurnEvent::BashPartialOutput { call_id, text } => {
            apply_bash_partial_output(state, &call_id, &text);
        }
        TurnEvent::MemoryExtracted { count, turn } => {
            // Mirror into AppState so the status bar can render
            // "🧠count@turn". This resolves the deferred note in the
            // Verification arm above (WO 26.7-R3).
            state.session.memory_status = Some((count, turn));
            state.mark_dirty();
        }
        TurnEvent::TurnComplete => {
            // Terminal event: the turn is fully done (model finished,
            // tools ran, continuation exhausted, or cancelled). Emitted
            // exactly once by the executor at the end of `run_turn`.
            // This is the ONLY place that clears `is_generating` and
            // `streaming` unconditionally — decoupled from CostStats,
            // which only fires when the provider supplies usage data.
            state.generation.is_generating = false;
            state.generation.turn_tool_calls = 0;
            state.generation.continuation = None;
            for msg in &mut state.conversation.messages {
                if msg.role == "assistant" || msg.role == "tool" {
                    msg.streaming = false;
                    msg.bump_version();
                }
            }
            // Cancelled calls never sent a ToolResult — drop their
            // card registrations so stale ids can't redirect later
            // chunks (their cards stopped streaming above; a fresh
            // ToolStart re-registers under a new model-assigned id).
            state.conversation.streaming_tool_index.clear();
            // WO 48.43: mark the turn boundary so a LATE call_id'd
            // chunk with no live registration drops instead of
            // resurrecting a ghost streaming card.
            state.generation.turn_finished = true;
        }
    }
}

/// ToolResult's placeholder-pairing block, extracted from
/// `dispatch_turn_event` (WO 48.43). Exact call_id pairing first; the
/// legacy name-based path (see `remove_tool_placeholder`) remains for
/// events with an empty call_id (old replay traces). Every removal
/// re-bases the surviving map entries — skipping the rebase on the
/// legacy path is what stranded orphaned placeholders (WO 48.43).
fn finalize_streaming_placeholder(state: &mut AppState, name: &str, call_id: &str) {
    if call_id.is_empty() {
        if let Some(idx) = remove_tool_placeholder(
            &mut state.conversation.messages,
            name,
            &state.conversation.streaming_tool_index,
        ) {
            rebase_streaming_indexes(&mut state.conversation.streaming_tool_index, idx);
        }
    } else if let Some(idx) = state.conversation.streaming_tool_index.remove(call_id) {
        // Remove the placeholder at its registered index —
        // only if it is still a streaming tool card (a prior
        // removal may have shifted or replaced it).
        let is_streaming_card = state
            .conversation
            .messages
            .get(idx)
            .is_some_and(|m| m.role == "tool" && m.streaming);
        if is_streaming_card {
            state.conversation.messages.remove(idx);
            rebase_streaming_indexes(&mut state.conversation.streaming_tool_index, idx);
        } else if let Some(fallback_idx) = remove_tool_placeholder(
            &mut state.conversation.messages,
            name,
            &state.conversation.streaming_tool_index,
        ) {
            // Stale index (defensive): fall back to name matching.
            rebase_streaming_indexes(&mut state.conversation.streaming_tool_index, fallback_idx);
        }
    }
}

/// Stream a PTY chunk into a streaming tool card, extracted from
/// `dispatch_turn_event` (WO 48.43). With a call_id (WO 48.31) the
/// chunk routes to the exact card registered by that call's ToolStart
/// — two parallel bash streams never interleave into one card. Without
/// one (old traces / defense in depth), fall back to the last
/// streaming tool card, pushing a fresh one if the back entry is not a
/// streaming tool card, so chunks always land somewhere sane instead
/// of corrupting a completed card or being dropped — EXCEPT after
/// TurnComplete: a call_id'd chunk with no live registration then is
/// late turn residue and is dropped (WO 48.43), because resurrecting a
/// streaming card nothing clears leaves a permanent ghost.
fn apply_bash_partial_output(state: &mut AppState, call_id: &str, text: &str) {
    let target_idx: Option<usize> = if !call_id.is_empty() {
        match state.conversation.streaming_tool_index.get(call_id) {
            Some(&idx)
                if state
                    .conversation
                    .messages
                    .get(idx)
                    .is_some_and(|m| m.role == "tool") =>
            {
                Some(idx)
            }
            // Unknown call_id while a turn is running (chunks raced
            // ahead of ToolStart, or an old client): create a card and
            // register it so subsequent chunks for this call_id
            // coalesce.
            //
            // ponytail: turn_finished is the only late guard — a
            // straggler arriving after the NEXT turn's first event
            // re-creates a card; per-id tombstones of finished
            // call_ids are the upgrade path if that ever shows.
            _ if !state.generation.turn_finished => {
                let mut entry = ConversationEntry::new("tool", "🔧 bash …");
                entry.streaming = true;
                state.conversation.messages.push_back(entry);
                let idx = state.conversation.messages.len() - 1;
                state
                    .conversation
                    .streaming_tool_index
                    .insert(call_id.to_string(), idx);
                Some(idx)
            }
            // Late chunk after TurnComplete: no live map entry, no
            // streaming card — noise, drop it.
            _ => None,
        }
    } else {
        let needs_fresh = match state.conversation.messages.back() {
            Some(last) => last.role != "tool" || !last.streaming,
            None => true,
        };
        if needs_fresh {
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new("tool", "🔧 bash …"));
        }
        Some(state.conversation.messages.len() - 1)
    };
    if let Some(last) = target_idx.and_then(|idx| state.conversation.messages.get_mut(idx)) {
        last.streaming = true;
        // Bound the streaming card: a `watch`/`top`/long
        // ping balloons `content` and the render path
        // re-wraps the whole string every frame. Keep the
        // tail under 64 KiB and surface a byte-count
        // marker so the user sees `… [N bytes total,
        // showing last 64K]` instead of silent loss. The
        // full output still lands in `tool_output` when
        // `ToolResult` finalizes the entry (WO 38.11).
        const PTY_TAIL_BYTES: usize = 64 * 1024;
        last.content.push_str(text);
        if last.content.len() > PTY_TAIL_BYTES {
            let total = last.content.len();
            let mut start = total - PTY_TAIL_BYTES;
            // Walk back to a char boundary BEFORE slicing so a
            // multibyte char straddling the offset doesn't
            // panic (WO 43.25). The prior char_indices fixup
            // here was dead code — the slice panicked first.
            while !last.content.is_char_boundary(start) {
                start -= 1;
            }
            let tail = last.content[start..].to_string();
            last.content =
                format!("… [{total} bytes total, showing last {PTY_TAIL_BYTES}]\n{tail}");
        }
        last.bump_version();
        state.mark_dirty();
    }
}

/// Decrement every streaming-tool index above `removed_idx` after a
/// mid-deque removal shifted later entries down one slot (WO 48.31).
/// Entries pointing at `removed_idx` itself are dropped — their card
/// is gone (their call finalized).
fn rebase_streaming_indexes(
    map: &mut std::collections::HashMap<String, usize>,
    removed_idx: usize,
) {
    map.retain(|_, idx| *idx != removed_idx);
    for idx in map.values_mut() {
        if *idx > removed_idx {
            *idx -= 1;
        }
    }
}

/// Remove this tool's streaming placeholder so `ToolResult` replaces
/// it in place instead of stacking two entries per call. Returns the
/// removed index so the caller can re-base `streaming_tool_index`.
///
/// Parallel tool batches push placeholders back-to-back (WO 46.35):
/// matching only `back()` pops the WRONG card and strands the sibling
/// as a ghost "streaming" row. Search backwards for the newest
/// streaming card whose content starts with `🔧 {name} ` — the
/// trailing space keeps `bash` from matching `bash_verbose`, and the
/// prefix survives PTY chunks appending to the card (including the
/// defense-in-depth fresh-card form "🔧 bash …").
///
/// Cards registered in `streaming_tool_index` are NEVER taken
/// (WO 48.43): an id-registered card belongs to a specific in-flight
/// call, so an empty-call_id (legacy) result must only pair with
/// legacy placeholders — otherwise it steals a sibling's card and
/// strands the id'd call's placeholder as an orphan.
///
/// Fallback: the PTY tail-budget rewrite (>64 KiB) strips the 🔧
/// header entirely, so if no named card exists, take the back card
/// when it is still streaming (the pre-46.35 behavior) — unless it is
/// id-registered (same WO 48.43 guard).
///
/// ponytail: legacy path for events with no call id (WO 48.31 added
/// exact call_id pairing above) — same-name parallel calls pair by
/// position (newest first), visually equivalent but subject to the
/// mixing the call_id index fixes.
fn remove_tool_placeholder(
    messages: &mut VecDeque<ConversationEntry>,
    name: &str,
    registered: &std::collections::HashMap<String, usize>,
) -> Option<usize> {
    let is_registered = |i: usize| registered.values().any(|&r| r == i);
    let prefix = format!("🔧 {name} ");
    let idx = messages
        .iter()
        .enumerate()
        .rev()
        .find(|(i, m)| {
            m.role == "tool" && m.streaming && m.content.starts_with(&prefix) && !is_registered(*i)
        })
        .map(|(i, _)| i);
    if let Some(idx) = idx {
        messages.remove(idx);
        Some(idx)
    } else {
        let back = messages.len().saturating_sub(1);
        if messages
            .back()
            .is_some_and(|m| m.role == "tool" && m.streaming)
            && !is_registered(back)
        {
            messages.pop_back();
            Some(back)
        } else {
            None
        }
    }
}

/// BPE-based token estimate for a message list. Routes through
/// `session::prompt::count_tokens` so the `budget` feature gate lives
/// in one place (minimal builds get the bytes/4 fallback there).
fn estimate_messages_tokens(messages: &[crate::shared::Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_tokens = crate::session::prompt::count_tokens(&m.content);
            let tool_call_tokens = m
                .tool_calls
                .as_ref()
                .map(|calls| {
                    let json = serde_json::to_string(calls).unwrap_or_default();
                    let json_tokens = crate::session::prompt::count_tokens(&json);
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
/// Tail byte budget for the thinking buffer. The render path joins +
/// re-wraps the whole buffer every frame; an unbounded buffer on a
/// long reasoning-model session is both a memory leak and an O(total)
/// per-frame cost. 32 KiB keeps the latest reasoning visible without
/// blowing up. Ponytail: ceiling — if users want full reasoning
/// history, surface it via a sidecar file instead of holding it in
/// the live TUI state.
const THINKING_TAIL_BYTES: usize = 32 * 1024;

/// Trim the thinking buffer from the front until the joined byte
/// length fits under `THINKING_TAIL_BYTES`. Keeps the most recent
/// chunks (the reasoning the user is currently watching) and drops
/// the oldest. Cheap: only runs when the budget is exceeded, and the
/// joined length is recomputed via a single pass.
fn trim_thinking_buffer_tail(buf: &mut Vec<String>) {
    if buf.is_empty() {
        return;
    }
    let total: usize = buf.iter().map(|s| s.len()).sum();
    if total <= THINKING_TAIL_BYTES {
        return;
    }
    // Drop from the front until we're under budget. We keep at least
    // one chunk so the panel never shows an empty buffer mid-stream.
    let mut kept_bytes = total;
    let mut drop_to = 0;
    for (i, s) in buf.iter().enumerate() {
        if kept_bytes <= THINKING_TAIL_BYTES || i + 1 == buf.len() {
            break;
        }
        kept_bytes -= s.len();
        drop_to = i + 1;
    }
    if drop_to > 0 {
        buf.drain(0..drop_to);
    }
}

pub fn drain_turn_events(
    state: &mut AppState,
    first: Option<TurnEvent>,
    event_rx: &mut mpsc::Receiver<TurnEvent>,
) {
    let mut any = false;
    if let Some(ev) = first {
        dispatch_turn_event(state, ev);
        any = true;
    }
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
    if state.conversation.messages.len() <= MAX_DISPLAY_MESSAGES {
        return;
    }
    let n_drop = state.conversation.messages.len() - KEEP_DISPLAY_MESSAGES;
    state.conversation.messages.drain(0..n_drop);
    // Insert a sentinel so the user knows old entries were trimmed.
    state.conversation.messages.push_front(
        ConversationEntry::new(
            "system",
            format!("[{n_drop} older messages pruned from display — use /save to preserve the full session]"),
        ),
    );
    // The sentinel is now at [0]; kept messages shifted by (1 - n_drop).
    // Re-map: old_idx → new_idx = old_idx - n_drop + 1  (only for old_idx >= n_drop)
    let remap = |i: usize| -> Option<usize> { i.checked_sub(n_drop).map(|x| x + 1) };
    state.conversation.collapsed_messages = state
        .conversation
        .collapsed_messages
        .iter()
        .filter_map(|&i| remap(i))
        .collect();
    // Streaming tool cards follow the same remap; a card pruned off the
    // front drops its entry so the call_id can never point at a
    // stranger's card (WO 48.31).
    state.conversation.streaming_tool_index = state
        .conversation
        .streaming_tool_index
        .iter()
        .filter_map(|(k, &i)| remap(i).map(|j| (k.clone(), j)))
        .collect();
    state.conversation.expanded_tools = state
        .conversation
        .expanded_tools
        .iter()
        .filter_map(|&i| remap(i))
        .collect();
    // The drain above re-indexed every kept entry to a lower slot.
    // Cache slots still hold the OLD (version, lines) for the
    // pre-prune positions; the (idx, version) guard can match by
    // coincidence when a shifted-down entry happens to share a
    // version with the slot's prior occupant. Drop the cache so the
    // next render repopulates from the new indices (WO 38.11).
    state.conversation.chat_render_cache.clear_entries();
    // Search indices reference old message positions — clear and let next search recompute.
    state.search.matches.clear();
    state.search.match_idx = 0;
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
    first: Option<ApprovalRequest>,
    approval_rx: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
) {
    let mut any = false;
    if let Some(req) = first {
        install_approval(state, req);
        any = true;
    }
    while let Ok(req) = approval_rx.try_recv() {
        install_approval(state, req);
        any = true;
    }
    if any {
        // A new approval (or a new approval replacing an old one)
        // appeared. The dialog overlay must be drawn on the next
        // frame, so mark the state dirty.
        state.mark_dirty();
    }
}

/// Install a single approval request into state, denying any pending
/// approval first (its oneshot sender must be answered or the executor
/// hangs forever on `response_rx.await`). Also clears any pending bang
/// gate so a model approval and a bang approval cannot be open at once.
fn install_approval(state: &mut AppState, req: ApprovalRequest) {
    // Deny any existing pending approval first. With the
    // `ApprovalResponder` drop-guard, simply dropping the old
    // responder would also send `Denied`; we send explicitly here so
    // the log records why.
    if let Some(old) = state.approval.pending_approval.take() {
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
    if state.approval.pending_bang.is_some() {
        state.approval.pending_bang = None;
    }
    state.approval.pending_approval = Some(PendingApproval {
        tool_name: req.tool_name.clone(),
        args: req.args.clone(),
        responder: Some(req.response),
    });
    // Reset approval scroll for each new request — a fresh dialog
    // starts at the top, regardless of where the previous one was.
    state.approval.approval_scroll = 0;
    state.approval.approval_max_scroll = 0;
    // Reset the side-by-side diff toggle too — otherwise a Tab toggle
    // on approval #1 persists into approval #2, showing a stale view
    // mode the user didn't ask for on the new approval.
    state.approval.approval_diff_side_by_side = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::executor::ApprovalResponder;
    use crate::shared::test_util::app_state;
    use crate::shared::{Message, Role};
    use crate::tui::app::ActiveTab;
    use tokio::sync::mpsc;

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
        let mut s = app_state();
        assert!(!s.generation.is_generating);
        dispatch_turn_event(&mut s, TurnEvent::Token("hi".into()));
        assert_eq!(s.conversation.messages.len(), 1);
        assert_eq!(s.conversation.messages[0].role, "assistant");
        assert_eq!(s.conversation.messages[0].content, "hi");
        assert!(s.generation.is_generating);
    }

    /// Subsequent `Token` events append to the *last* assistant
    /// entry — that's how streaming chat looks (one growing entry,
    /// not a new entry per delta).
    #[test]
    fn token_appends_to_last_assistant_entry() {
        let mut s = app_state();
        dispatch_turn_event(&mut s, TurnEvent::Token("foo".into()));
        dispatch_turn_event(&mut s, TurnEvent::Token("bar".into()));
        assert_eq!(s.conversation.messages.len(), 1);
        assert_eq!(s.conversation.messages[0].content, "foobar");
    }

    /// Within-turn streaming: "text → tool → more text" keeps appending
    /// to the SAME assistant entry. The `streaming` flag (set by the
    /// first Token, NOT cleared by ToolStart/ToolResult) is the
    /// within-turn marker. This is the case the `is_current_turn` check
    /// must preserve — the model emits text, calls a tool, then emits
    /// more text, and all of it belongs to one assistant message.
    #[test]
    fn token_appends_across_tool_calls_within_turn() {
        let mut s = app_state();
        // Turn 1: text, then a tool call, then more text.
        dispatch_turn_event(&mut s, TurnEvent::Token("Let me check ".into()));
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({"command": "ls"}),
                call_id: String::new(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "file.txt".into(),
                success: true,
                call_id: String::new(),
            },
        );
        // The model emits more text after the tool result — this must
        // append to the SAME assistant entry, not open a new one.
        dispatch_turn_event(&mut s, TurnEvent::Token("found it".into()));
        assert_eq!(
            s.conversation.messages.len(),
            2,
            "should be assistant + tool, not a second assistant entry"
        );
        assert_eq!(s.conversation.messages[0].role, "assistant");
        assert_eq!(
            s.conversation.messages[0].content, "Let me check found it",
            "post-tool text should append to the pre-tool assistant entry"
        );
    }

    /// Cross-turn isolation: after `TurnComplete` clears `streaming`, a
    /// new turn's first Token opens a NEW assistant entry instead of
    /// appending to the prior turn's assistant. This is the regression
    /// guard for the "slaps all text into the same initial text response"
    /// bug — the old heuristic (all entries after the assistant are
    /// tool/system) caused turn 2's text to bleed into turn 1's
    /// assistant entry because the tool entries from turn 1 were still
    /// the last entries.
    #[test]
    fn token_opens_new_entry_after_turn_complete() {
        let mut s = app_state();
        // Turn 1: text + tool call.
        dispatch_turn_event(&mut s, TurnEvent::Token("turn one".into()));
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({"command": "ls"}),
                call_id: String::new(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "ok".into(),
                success: true,
                call_id: String::new(),
            },
        );
        // Turn 1 ends — TurnComplete clears streaming on all assistant entries.
        dispatch_turn_event(&mut s, TurnEvent::TurnComplete);
        assert!(
            !s.conversation.messages[0].streaming,
            "TurnComplete must clear streaming on the assistant entry"
        );
        // Turn 2: first token. Must NOT append to turn 1's assistant.
        dispatch_turn_event(&mut s, TurnEvent::Token("turn two".into()));
        assert_eq!(
            s.conversation.messages.len(),
            3,
            "turn 2 should open a new assistant entry (assistant + tool + assistant)"
        );
        assert_eq!(s.conversation.messages[0].content, "turn one");
        assert_eq!(s.conversation.messages[2].role, "assistant");
        assert_eq!(
            s.conversation.messages[2].content, "turn two",
            "turn 2 text must NOT bleed into turn 1's assistant entry"
        );
    }

    /// `Thinking` accumulates into the thinking buffer. The TUI
    /// renders it on demand when the user toggles the thinking
    /// panel (Esc). One push per delta.
    #[test]
    fn thinking_appends_to_buffer() {
        let mut s = app_state();
        dispatch_turn_event(&mut s, TurnEvent::Thinking("a".into()));
        dispatch_turn_event(&mut s, TurnEvent::Thinking("b".into()));
        assert_eq!(
            s.generation.thinking_buffer,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    /// `ToolStart` creates a "🔧 name ..." entry and flips
    /// `is_generating` to false (the model has paused to call a tool).
    #[test]
    fn toolstart_creates_running_entry() {
        let mut s = app_state();
        dispatch_turn_event(&mut s, TurnEvent::Token("hmm".into()));
        assert!(s.generation.is_generating);
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({"cmd": "ls"}),
                call_id: String::new(),
            },
        );
        assert!(!s.generation.is_generating);
        assert_eq!(s.conversation.messages.len(), 2);
        assert_eq!(s.conversation.messages[1].role, "tool");
        assert!(s.conversation.messages[1].content.contains("bash"));
        assert!(s.conversation.messages[1].content.contains("..."));
    }

    /// `BashPartialOutput` streams PTY output into the running tool card:
    /// it marks the last tool entry as streaming and appends the chunk.
    /// The entry stays streaming until `ToolResult` finalizes it.
    ///
    /// WO 44.38: with ToolStart now emitted at dispatch time, this is the
    /// real event order (start → chunks → result). The test also covers
    /// the defense-in-depth path: chunks arriving with no streaming card
    /// get a fresh one instead of being dropped.
    #[test]
    fn bash_partial_output_streams_into_running_tool_card() {
        // ── Order 1: ToolStart first (the real order post-44.38) ──
        let mut s = app_state();
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({"cmd": "top"}),
                call_id: String::new(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: String::new(),
                text: "PID  USER".into(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: String::new(),
                text: "\n  1 root".into(),
            },
        );

        assert_eq!(s.conversation.messages.len(), 1);
        let entry = &s.conversation.messages[0];
        assert_eq!(entry.role, "tool");
        assert!(entry.streaming, "entry must be marked streaming");
        assert!(entry.content.contains("PID  USER"));
        assert!(entry.content.contains("1 root"));

        // `ToolResult` finalizes the entry: streaming is cleared and the
        // full output replaces the incremental text.
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "final".into(),
                success: true,
                call_id: String::new(),
            },
        );
        assert_eq!(s.conversation.messages.len(), 1);
        assert!(!s.conversation.messages[0].streaming);
        assert_eq!(
            s.conversation.messages[0].tool_output.as_deref(),
            Some("final")
        );

        // ── Order 2: chunks before ToolStart (defense in depth) ──
        // If chunks arrive with no streaming tool card (e.g. a race or
        // pre-44.38 executor), the TUI must push a fresh streaming card
        // instead of dropping the chunks.
        let mut s2 = app_state();
        dispatch_turn_event(
            &mut s2,
            TurnEvent::BashPartialOutput {
                call_id: String::new(),
                text: "early chunk".into(),
            },
        );
        assert_eq!(s2.conversation.messages.len(), 1);
        assert_eq!(s2.conversation.messages[0].role, "tool");
        assert!(s2.conversation.messages[0].streaming);
        assert!(s2.conversation.messages[0].content.contains("early chunk"));
    }

    /// WO 44.38 regression: chunks arriving after a completed tool card
    /// must NOT corrupt it. The completed card's content and streaming
    /// flag must be untouched; a fresh streaming card is pushed instead.
    #[test]
    fn bash_partial_output_after_completed_card_does_not_corrupt_it() {
        let mut s = app_state();
        // First tool: start → result (completed card).
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "grep".into(),
                args: serde_json::json!({"pattern": "foo"}),
                call_id: String::new(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "grep".into(),
                output: "1 hit".into(),
                success: true,
                call_id: String::new(),
            },
        );
        assert_eq!(s.conversation.messages.len(), 1);
        assert!(!s.conversation.messages[0].streaming);
        let completed_content = s.conversation.messages[0].content.clone();

        // Second tool's PTY chunks arrive (before its ToolStart, or after
        // a completed card from a prior batch). Must not touch the grep card.
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: String::new(),
                text: "chunk for bash".into(),
            },
        );

        // The completed card is untouched.
        assert_eq!(s.conversation.messages[0].content, completed_content);
        assert!(!s.conversation.messages[0].streaming);
        // A fresh streaming card was pushed for the chunks.
        assert_eq!(s.conversation.messages.len(), 2);
        assert_eq!(s.conversation.messages[1].role, "tool");
        assert!(s.conversation.messages[1].streaming);
        assert!(s.conversation.messages[1]
            .content
            .contains("chunk for bash"));
    }

    /// WO 46.35: parallel tool batches push placeholders back-to-back.
    /// `ToolResult` must replace ITS OWN tool's placeholder — not just
    /// "the last streaming card" — or the sibling card is stranded as a
    /// ghost spinner and the completed result lands out of order.
    #[test]
    fn parallel_tool_batch_results_replace_their_own_placeholders() {
        let mut s = app_state();
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "edit_file".into(),
                args: serde_json::json!({"path": "a.rs"}),
                call_id: String::new(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({"cmd": "ls"}),
                call_id: String::new(),
            },
        );

        // First result is for edit_file — NOT the most recent card.
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "edit_file".into(),
                output: "ok".into(),
                success: true,
                call_id: String::new(),
            },
        );
        assert_eq!(
            s.conversation.messages.len(),
            2,
            "edit_file's result replaced its own placeholder; bash's remains"
        );
        // messages[0] is bash's still-open placeholder.
        assert!(
            s.conversation.messages[0].streaming,
            "bash placeholder still open"
        );
        assert!(s.conversation.messages[0].content.contains("bash"));
        // messages[1] is edit_file's finalized card.
        assert!(!s.conversation.messages[1].streaming);
        assert!(s.conversation.messages[1]
            .content
            .contains("edit_file (done)"));

        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "ran".into(),
                success: true,
                call_id: String::new(),
            },
        );
        assert_eq!(
            s.conversation.messages.len(),
            2,
            "both results replaced their placeholders — no ghost cards"
        );
        assert!(!s.conversation.messages[0].streaming);
        assert!(!s.conversation.messages[1].streaming);
        assert!(s.conversation.messages[1].content.contains("bash (done)"));
        assert_eq!(
            s.conversation.messages[1].tool_output.as_deref(),
            Some("ran")
        );
    }

    /// WO 46.35: same-name parallel calls — each result consumes one
    /// placeholder; neither is left streaming.
    #[test]
    fn same_name_parallel_tool_results_leave_no_ghost() {
        let mut s = app_state();
        for path in ["x.rs", "y.rs"] {
            dispatch_turn_event(
                &mut s,
                TurnEvent::ToolStart {
                    name: "read_file".into(),
                    args: serde_json::json!({ "path": path }),
                    call_id: String::new(),
                },
            );
        }
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "read_file".into(),
                output: "one".into(),
                success: true,
                call_id: String::new(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "read_file".into(),
                output: "two".into(),
                success: true,
                call_id: String::new(),
            },
        );
        assert_eq!(
            s.conversation.messages.len(),
            2,
            "two calls → exactly two finalized cards"
        );
        assert!(
            s.conversation.messages.iter().all(|m| !m.streaming),
            "no ghost streaming cards"
        );
        let outs: Vec<_> = s
            .conversation
            .messages
            .iter()
            .map(|m| m.tool_output.as_deref())
            .collect();
        assert!(outs.contains(&Some("one")) && outs.contains(&Some("two")));
    }

    /// WO 48.31: the exact WO-10-era failure scenario — two parallel
    /// bash calls streaming PTY chunks concurrently. Chunks carry a
    /// call_id; each call's chunks land ONLY in its own card, never
    /// in the sibling's, no matter how they interleave.
    #[test]
    fn concurrent_same_name_pty_streams_route_to_separate_cards() {
        let mut s = app_state();
        for (id, cmd) in [("call-a", "watch ps"), ("call-b", "tail -f log")] {
            dispatch_turn_event(
                &mut s,
                TurnEvent::ToolStart {
                    name: "bash".into(),
                    args: serde_json::json!({ "command": cmd }),
                    call_id: id.into(),
                },
            );
        }
        assert_eq!(s.conversation.messages.len(), 2);
        assert_eq!(s.conversation.streaming_tool_index.len(), 2);

        // Interleaved chunks — a1, b1, b2, a2 — the exact mixing order
        // that corrupted the single last-streaming-card path.
        for (id, text) in [
            ("call-a", "A-line-1\n"),
            ("call-b", "B-line-1\n"),
            ("call-b", "B-line-2\n"),
            ("call-a", "A-line-2\n"),
        ] {
            dispatch_turn_event(
                &mut s,
                TurnEvent::BashPartialOutput {
                    call_id: id.into(),
                    text: text.into(),
                },
            );
        }

        let a = &s.conversation.messages[0];
        let b = &s.conversation.messages[1];
        assert!(a.content.contains("A-line-1") && a.content.contains("A-line-2"));
        assert!(b.content.contains("B-line-1") && b.content.contains("B-line-2"));
        assert!(
            !a.content.contains("B-line"),
            "call-a's card must not contain call-b's chunks: {}",
            a.content
        );
        assert!(
            !b.content.contains("A-line"),
            "call-b's card must not contain call-a's chunks: {}",
            b.content
        );

        // Out-of-order finalization: call-b finishes first. Its own
        // placeholder (index 1) is removed — call-a's registration must
        // re-base to the shifted index and still finalize correctly.
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "b done".into(),
                success: true,
                call_id: "call-b".into(),
            },
        );
        assert_eq!(s.conversation.messages.len(), 2);
        assert!(s.conversation.messages[0].streaming, "call-a still open");
        assert_eq!(
            s.conversation.messages[1].tool_output.as_deref(),
            Some("b done")
        );
        assert_eq!(s.conversation.streaming_tool_index.len(), 1);

        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: "call-a".into(),
                text: "A-line-3\n".into(),
            },
        );
        assert!(
            s.conversation.messages[0].content.contains("A-line-3"),
            "post-rebase chunk must still reach call-a's card"
        );

        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "a done".into(),
                success: true,
                call_id: "call-a".into(),
            },
        );
        assert_eq!(s.conversation.messages.len(), 2);
        assert!(
            s.conversation.streaming_tool_index.is_empty(),
            "all registrations removed after finalize"
        );
        let outs: Vec<_> = s
            .conversation
            .messages
            .iter()
            .map(|m| m.tool_output.as_deref())
            .collect();
        assert!(outs.contains(&Some("a done")) && outs.contains(&Some("b done")));
    }

    /// WO 48.31: parallel same-name batch pairs each ToolResult with
    /// its OWN placeholder by call_id — results arriving in the
    /// opposite order to the starts still land on the right card.
    #[test]
    fn call_id_tool_results_pair_with_own_placeholder() {
        let mut s = app_state();
        for (id, path) in [("id-1", "x.rs"), ("id-2", "y.rs")] {
            dispatch_turn_event(
                &mut s,
                TurnEvent::ToolStart {
                    name: "read_file".into(),
                    args: serde_json::json!({ "path": path }),
                    call_id: id.into(),
                },
            );
        }
        // Reverse-order results: id-2 finishes first.
        for (id, out) in [("id-2", "contents-of-y"), ("id-1", "contents-of-x")] {
            dispatch_turn_event(
                &mut s,
                TurnEvent::ToolResult {
                    name: "read_file".into(),
                    output: out.into(),
                    success: true,
                    call_id: id.into(),
                },
            );
        }
        assert_eq!(s.conversation.messages.len(), 2);
        assert!(s.conversation.messages.iter().all(|m| !m.streaming));
        // Cards finalize in completion order (y finished first — its
        // placeholder left the deque first); each output pairs with its
        // own call, no ghost placeholders, no crossed outputs.
        assert_eq!(
            s.conversation.messages[0].tool_output.as_deref(),
            Some("contents-of-y")
        );
        assert_eq!(
            s.conversation.messages[1].tool_output.as_deref(),
            Some("contents-of-x")
        );
    }

    /// WO 48.31 back-compat: a chunk with an UNKNOWN call_id (raced
    /// ahead of ToolStart) creates and registers a card — later chunks
    /// for the same id coalesce into it instead of stacking cards.
    #[test]
    fn unknown_call_id_chunk_creates_and_registers_card() {
        let mut s = app_state();
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: "call-x".into(),
                text: "first ".into(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: "call-x".into(),
                text: "second".into(),
            },
        );
        assert_eq!(s.conversation.messages.len(), 1, "both chunks coalesce");
        assert!(s.conversation.messages[0].content.contains("first second"));
        assert_eq!(s.conversation.streaming_tool_index.len(), 1);

        // The late ToolStart for the same id keeps the existing card
        // (re-registers the same index) — no duplicate.
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({ "command": "x" }),
                call_id: "call-x".into(),
            },
        );
        assert_eq!(s.conversation.messages.len(), 1);

        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "done".into(),
                success: true,
                call_id: "call-x".into(),
            },
        );
        assert_eq!(s.conversation.messages.len(), 1);
        assert_eq!(
            s.conversation.messages[0].tool_output.as_deref(),
            Some("done")
        );
    }

    /// WO 48.43 probe 1: an empty-call_id ToolResult (old-trace path)
    /// must pair ONLY with legacy (unregistered) placeholders — it
    /// must not remove a streaming card registered under a real
    /// call_id by a different in-flight call, and the survivor's map
    /// entry must be re-based to the shifted index.
    #[test]
    fn legacy_tool_result_never_steals_call_id_card() {
        let mut s = app_state();
        // Legacy placeholder first, id'd second — the name-matcher's
        // backwards scan would find the id'd card (the newest "bash"
        // match) and steal it pre-fix.
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({ "command": "legacy" }),
                call_id: String::new(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({ "command": "modern" }),
                call_id: "call-a".into(),
            },
        );
        // Mark call-a's card with its own chunk so the two same-name
        // cards are distinguishable.
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: "call-a".into(),
                text: "A-stream".into(),
            },
        );
        assert_eq!(s.conversation.messages.len(), 2);
        assert_eq!(s.conversation.streaming_tool_index.len(), 1);

        // The legacy result finalizes the LEGACY card...
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "legacy done".into(),
                success: true,
                call_id: String::new(),
            },
        );
        // ...not call-a's: exactly one streaming card remains, it is
        // call-a's, and the map re-based to the shifted index (was 1,
        // the legacy removal at 0 shifts it to 0).
        assert_eq!(s.conversation.streaming_tool_index.get("call-a"), Some(&0));
        let streaming: Vec<_> = s
            .conversation
            .messages
            .iter()
            .filter(|m| m.streaming)
            .collect();
        assert_eq!(streaming.len(), 1, "call-a's card must survive");
        assert!(
            streaming[0].content.contains("A-stream"),
            "the survivor is call-a's card, not the legacy one"
        );
        assert!(
            !streaming[0].content.contains("(done)"),
            "the legacy summary must not have landed on call-a's card"
        );

        // Chunks and the final result still route to call-a's card;
        // no orphaned placeholder survives the full lifecycle.
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: "call-a".into(),
                text: "A-tail".into(),
            },
        );
        assert!(s.conversation.messages.iter().any(|m| {
            m.streaming && m.content.contains("A-stream") && m.content.contains("A-tail")
        }));
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "modern done".into(),
                success: true,
                call_id: "call-a".into(),
            },
        );
        assert!(s.conversation.streaming_tool_index.is_empty());
        assert!(
            !s.conversation.messages.iter().any(|m| m.streaming),
            "no orphaned placeholder"
        );
        let outs: Vec<_> = s
            .conversation
            .messages
            .iter()
            .map(|m| m.tool_output.as_deref())
            .collect();
        assert!(
            outs.contains(&Some("legacy done")) && outs.contains(&Some("modern done")),
            "each result pairs with its own call: {outs:?}"
        );
    }

    /// WO 48.43 probe 2: a late BashPartialOutput arriving AFTER
    /// TurnComplete (map cleared, nothing streaming) must be dropped —
    /// pre-fix it resurrected a streaming card nothing ever clears.
    #[test]
    fn late_bash_partial_output_after_turn_complete_is_dropped() {
        let mut s = app_state();
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({ "command": "x" }),
                call_id: "call-a".into(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: "call-a".into(),
                text: "live chunk".into(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: "done".into(),
                success: true,
                call_id: "call-a".into(),
            },
        );
        dispatch_turn_event(&mut s, TurnEvent::TurnComplete);
        let len_before = s.conversation.messages.len();
        assert!(s.conversation.messages.iter().all(|m| !m.streaming));
        assert!(s.conversation.streaming_tool_index.is_empty());
        assert!(s.generation.turn_finished);

        // The late chunk: no live map entry, no streaming card —
        // noise after turn end. Dropped, not resurrected.
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: "call-a".into(),
                text: "late tail".into(),
            },
        );
        assert_eq!(s.conversation.messages.len(), len_before, "no ghost card");
        assert!(s.conversation.messages.iter().all(|m| !m.streaming));
        assert!(s.conversation.streaming_tool_index.is_empty());
        assert!(
            !s.conversation
                .messages
                .iter()
                .any(|m| m.content.contains("late tail")),
            "the late chunk must not land anywhere"
        );

        // A NEW turn's unknown-id chunk still creates its card — the
        // WO 48.31 race-ahead contract survives the turn boundary.
        dispatch_turn_event(&mut s, TurnEvent::Token("next turn".into()));
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: "call-z".into(),
                text: "fresh".into(),
            },
        );
        assert!(
            s.conversation
                .messages
                .iter()
                .any(|m| m.content.contains("fresh")),
            "mid-turn unknown-id chunk still gets a card"
        );
        assert!(!s.generation.turn_finished);
    }

    /// `ToolResult` is the v1.1 contract: full output goes into
    /// the sidecar, the visible `content` is a one-line summary
    /// with the byte/line count. This is what makes Ctrl+T flood
    /// control possible.
    #[test]
    fn toolresult_stores_full_output_in_sidecar() {
        let mut s = app_state();
        let full = "line 1\nline 2\nline 3\n".to_string();
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolResult {
                name: "bash".into(),
                output: full.clone(),
                success: true,
                call_id: String::new(),
            },
        );
        assert_eq!(s.conversation.messages.len(), 1);
        let entry = &s.conversation.messages[0];
        assert_eq!(entry.role, "tool");
        // Visible summary contains the byte count and expand hint
        assert!(entry.content.contains("bash"));
        assert!(entry.content.contains("bytes"));
        assert!(entry.content.contains("Enter or Tab to expand"));
        // Sidecar holds the full output verbatim
        assert_eq!(entry.tool_output.as_deref(), Some(full.as_str()));
    }

    /// `Verification` prefixes with 🔍 on a non-failure outcome and ⚠️ on
    /// `Failed`. The same code path handles both — only the prefix and
    /// the `outcome` discriminant differ. WO 45.36.
    #[test]
    fn verification_prefixes_reflect_success() {
        use crate::session::executor::types::VerificationOutcome;
        let mut s = app_state();
        dispatch_turn_event(
            &mut s,
            TurnEvent::Verification {
                message: "lint clean".into(),
                outcome: VerificationOutcome::Clean,
                file: None,
                line: None,
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::Verification {
                message: "found 2 warnings".into(),
                outcome: VerificationOutcome::Failed,
                file: None,
                line: None,
            },
        );
        assert!(s.conversation.messages[0].content.starts_with("🔍"));
        assert!(s.conversation.messages[0].content.contains("lint clean"));
        assert!(s.conversation.messages[1].content.starts_with("⚠️"));
        assert!(s.conversation.messages[1]
            .content
            .contains("found 2 warnings"));
    }

    /// `Error` is a plain "Error: ..." system message. The model
    /// saw a transport or parse failure and the turn ended.
    #[test]
    fn error_pushes_system_message_and_stops_generation() {
        let mut s = app_state();
        dispatch_turn_event(&mut s, TurnEvent::Token("partial".into()));
        assert!(s.generation.is_generating);
        dispatch_turn_event(&mut s, TurnEvent::Error("timeout".into()));
        assert!(!s.generation.is_generating);
        assert_eq!(s.conversation.messages.back().unwrap().role, "system");
        assert!(s
            .conversation
            .messages
            .back()
            .unwrap()
            .content
            .contains("timeout"));
    }

    /// WO 38.11: `TurnEvent::Error` must clear stale `continuation`
    /// and `streaming` flags, mirroring `TurnComplete`. Otherwise a
    /// continuation round that errored leaves "⟳ 3/5" stuck in the
    /// status bar and the partial assistant entry keeps rendering a
    /// spinner forever.
    #[test]
    fn error_clears_continuation_and_streaming_flags() {
        let mut s = app_state();
        // Simulate a streaming turn mid-continuation.
        dispatch_turn_event(&mut s, TurnEvent::Token("partial".into()));
        s.generation.continuation = Some((3, 5));
        s.generation.turn_tool_calls = 4;
        assert!(s.conversation.messages[0].streaming);
        assert!(s.generation.is_generating);
        // The error ends the turn.
        dispatch_turn_event(&mut s, TurnEvent::Error("transport".into()));
        assert!(!s.generation.is_generating);
        assert!(
            s.generation.continuation.is_none(),
            "continuation must clear"
        );
        assert_eq!(
            s.generation.turn_tool_calls, 0,
            "tool-call counter must reset"
        );
        assert!(
            !s.conversation.messages[0].streaming,
            "streaming flag on the partial assistant must clear"
        );
    }

    /// WO 38.11: the thinking buffer is bounded to a tail byte
    /// budget. Long reasoning-model sessions emit thousands of
    /// chunks; without trimming the buffer grows unbounded and the
    /// render path re-wraps the whole thing every frame. After the
    /// budget is exceeded, the oldest chunks are dropped and the
    /// most recent (what the user is watching) are kept.
    #[test]
    fn thinking_buffer_is_bounded_to_tail_budget() {
        let mut s = app_state();
        // Push 100 chunks of 1 KiB each = 100 KiB total, well over
        // the 32 KiB budget.
        let chunk = "x".repeat(1024);
        for _ in 0..100 {
            dispatch_turn_event(&mut s, TurnEvent::Thinking(chunk.clone()));
        }
        let total: usize = s.generation.thinking_buffer.iter().map(|s| s.len()).sum();
        assert!(
            total <= super::THINKING_TAIL_BYTES + 1024,
            "buffer should be bounded to ~{} bytes, got {total}",
            super::THINKING_TAIL_BYTES
        );
        // The buffer must still hold the most recent chunks (the
        // tail), not be empty.
        assert!(
            !s.generation.thinking_buffer.is_empty(),
            "buffer must retain the tail, not be emptied"
        );
        // The last chunk should be the most recent one we pushed.
        let last = s.generation.thinking_buffer.last().unwrap();
        assert_eq!(last.len(), 1024);
    }

    /// WO 38.11: a streaming PTY tool card is capped to a tail byte
    /// budget with a byte-count marker. A `watch`/`top`-style command
    /// balloons `entry.content` without limit otherwise; the render
    /// path re-wraps the full string every frame. After the cap, the
    /// content shows a truncation marker + the tail.
    #[test]
    fn bash_partial_output_caps_streaming_card_to_tail_budget() {
        let mut s = app_state();
        dispatch_turn_event(
            &mut s,
            TurnEvent::ToolStart {
                name: "bash".into(),
                args: serde_json::json!({"cmd": "top"}),
                call_id: String::new(),
            },
        );
        // Push 100 KiB of output — well over the 64 KiB cap.
        let chunk = "y".repeat(1024);
        for _ in 0..100 {
            dispatch_turn_event(
                &mut s,
                TurnEvent::BashPartialOutput {
                    call_id: String::new(),
                    text: chunk.clone(),
                },
            );
        }
        let entry = &s.conversation.messages[0];
        assert!(entry.streaming, "entry must stay streaming");
        assert!(
            entry.content.contains("bytes total, showing last"),
            "capped content should show the byte-count marker, got: {}...",
            &entry.content[..200.min(entry.content.len())]
        );
        // The content should be roughly the cap + marker, not 100 KiB.
        assert!(
            entry.content.len() < 70_000,
            "capped content should be under ~70 KiB, got {}",
            entry.content.len()
        );
    }

    /// WO 38.11: `CompactionReport` clears the chat render cache.
    /// Without invalidation, the cache slots hold the pre-compaction
    /// renders and the chat panel serves stale lines at the new
    /// indices.
    #[test]
    fn compaction_clears_chat_render_cache() {
        let mut s = app_state();
        // Populate the cache with a dummy entry so we can detect
        // the clear.
        s.conversation
            .chat_render_cache
            .entries
            .push(Some((42, Vec::new())));
        assert_eq!(s.conversation.chat_render_cache.entries.len(), 1);
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
        assert!(
            s.conversation.chat_render_cache.entries.is_empty(),
            "CompactionReport must clear the render cache"
        );
    }

    /// WO 38.11: `prune_display_messages` clears the chat render
    /// cache. The drain re-indexes kept entries to lower slots; cache
    /// slots still hold the pre-prune renders and can match by
    /// coincidence.
    #[test]
    fn prune_clears_chat_render_cache() {
        let mut s = app_state();
        // Overfill past MAX_DISPLAY_MESSAGES so prune runs.
        for i in 0..(super::MAX_DISPLAY_MESSAGES + 50) {
            s.conversation
                .messages
                .push_back(ConversationEntry::new("user", format!("msg {i}")));
        }
        // Populate the cache so we can detect the clear.
        s.conversation.chat_render_cache.entries =
            vec![Some((1, Vec::new())); s.conversation.messages.len()];
        assert!(!s.conversation.chat_render_cache.entries.is_empty());
        super::prune_display_messages(&mut s);
        assert!(
            s.conversation.chat_render_cache.entries.is_empty(),
            "prune_display_messages must clear the render cache"
        );
    }

    /// `CostStats` accumulates the **cumulative** token counters
    /// (sent/received) and overwrites the per-turn cost fields.
    /// Also mirrors the per-turn `prompt_tokens` into
    /// `last_turn_prompt_tokens` so the status bar can show
    /// context pressure.
    ///
    /// Note: `CostStats` no longer clears `is_generating` /
    /// `streaming` / `continuation` — that's `TurnComplete`'s job
    /// now (decoupled so providers with `usage: None` still
    /// finalize the UI).
    #[test]
    fn coststats_accumulates_and_mirrors_last_turn() {
        let mut s = app_state();
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
        assert_eq!(s.budget.tokens_sent, 100);
        assert_eq!(s.budget.tokens_received, 50);
        assert_eq!(s.budget.turn_cost, 0.001);
        assert_eq!(s.budget.cumulative_cost, 0.001);
        assert_eq!(s.budget.last_turn_prompt_tokens, 100);
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
        assert_eq!(s.budget.tokens_sent, 300);
        assert_eq!(s.budget.tokens_received, 130);
        assert_eq!(s.budget.last_turn_prompt_tokens, 200);
    }

    /// Regression (Bug #2): a provider that emits `Done { usage: None }`
    /// never sends `CostStats`. The TUI must still finalize the turn.
    /// `TurnComplete` is the terminal event that clears `is_generating`
    /// and `streaming` unconditionally.
    #[test]
    fn turn_complete_finalizes_without_cost_stats() {
        let mut s = app_state();
        // Simulate a streaming turn: tokens arrive, is_generating flips.
        // Two tokens so the second appends to the first entry and sets
        // the streaming flag (the first token creates the entry; the
        // second sets streaming=true on append).
        dispatch_turn_event(&mut s, TurnEvent::Token("hello".into()));
        dispatch_turn_event(&mut s, TurnEvent::Token(" world".into()));
        assert!(s.generation.is_generating);
        assert!(s.conversation.messages[0].streaming);
        // No CostStats arrives (usage: None provider). TurnComplete must
        // still clear the flags.
        dispatch_turn_event(&mut s, TurnEvent::TurnComplete);
        assert!(
            !s.generation.is_generating,
            "TurnComplete must clear is_generating without CostStats"
        );
        assert!(
            !s.conversation.messages[0].streaming,
            "TurnComplete must clear streaming flag without CostStats"
        );
    }

    /// `TurnComplete` also clears the continuation indicator and
    /// resets the per-turn tool-call counter — the role CostStats
    /// previously played (incorrectly, since not every turn emits
    /// CostStats).
    #[test]
    fn turn_complete_clears_continuation_and_tool_calls() {
        let mut s = app_state();
        s.generation.continuation = Some((3, 5));
        s.generation.turn_tool_calls = 7;
        dispatch_turn_event(&mut s, TurnEvent::TurnComplete);
        assert!(s.generation.continuation.is_none());
        assert_eq!(s.generation.turn_tool_calls, 0);
    }

    /// `CompactionReport` rebuilds `messages` from `new_messages`,
    /// appends a status line, clears `expanded_tools` (indices
    /// are now meaningless), and resets scroll to the bottom.
    #[test]
    fn compaction_rebuilds_messages_and_resets_scroll() {
        let mut s = app_state();
        // Pre-existing tool expansion that references index 0 —
        // must be cleared, not silently re-applied to the wrong
        // entry after the rebuild.
        s.conversation.expanded_tools.insert(0);
        s.conversation.scroll_offset = 42;
        s.conversation.auto_scroll = false;

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
        assert_eq!(s.conversation.messages.len(), 3);
        assert_eq!(s.conversation.messages[0].role, "user");
        assert_eq!(s.conversation.messages[0].content, "hi");
        assert_eq!(s.conversation.messages[1].role, "assistant");
        assert_eq!(s.conversation.messages[1].content, "hello");
        assert_eq!(s.conversation.messages[2].role, "system");
        assert!(s.conversation.messages[2].content.contains("10 → 4"));
        assert!(s.conversation.messages[2].content.contains("dropped 3"));
        assert!(s.conversation.messages[2].content.contains("condensed 2"));
        // Per-index expansion cleared (stale indices)
        assert!(s.conversation.expanded_tools.is_empty());
        // Scroll reset to bottom so the user sees the status line
        assert!(s.conversation.auto_scroll);
        assert_eq!(s.conversation.scroll_offset, 0);
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
        let mut s = app_state();
        // Pre-compact: a 30K token context (the kind of pressure
        // /compact exists to relieve).
        s.budget.last_turn_prompt_tokens = 30_000;
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
            s.budget.last_turn_prompt_tokens < 1_000,
            "post-compact estimate should be near-zero, got {}",
            s.budget.last_turn_prompt_tokens
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
        let mut s = app_state();
        s.budget.last_turn_prompt_tokens = 0;
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
            s.budget.last_turn_prompt_tokens > 1_000,
            "post-compact estimate must count tool_calls, got {}",
            s.budget.last_turn_prompt_tokens
        );
    }

    /// The post-compact estimate is strictly the *post*-compact
    /// value, never the pre. This is the regression guard for the
    /// exact bug: a user at 110K/128K (red) issues `/compact` and
    /// expects to see the green/lower number, not the red one.
    #[test]
    fn compaction_estimate_uses_post_compact_size_not_pre() {
        let mut s = app_state();
        // Pretend we were at 110K (deep red).
        s.budget.last_turn_prompt_tokens = 110_000;
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
            s.budget.last_turn_prompt_tokens < 1_000,
            "post-compact estimate leaked the pre-compact value: {}",
            s.budget.last_turn_prompt_tokens
        );
    }

    /// `drain_turn_events` pulls every event in queue order and
    /// applies each one. After the call the channel is empty.
    #[test]
    fn drain_turn_events_pulls_all() {
        let mut s = app_state();
        let (tx, mut rx) = mpsc::channel::<TurnEvent>(10_000);
        tx.try_send(TurnEvent::Token("a".into())).unwrap();
        tx.try_send(TurnEvent::Token("b".into())).unwrap();
        tx.try_send(TurnEvent::Token("c".into())).unwrap();
        drain_turn_events(&mut s, None, &mut rx);
        assert_eq!(s.conversation.messages.len(), 1);
        assert_eq!(s.conversation.messages[0].content, "abc");
        // Channel is drained — next call is a no-op
        drain_turn_events(&mut s, None, &mut rx);
        assert_eq!(s.conversation.messages.len(), 1);
    }

    /// Regression: the `select!` arm consumes one event via `recv()`,
    /// then `drain_turn_events` must dispatch THAT event plus everything
    /// still queued. The prior code dropped the `recv()`'d event and only
    /// drained what arrived after — losing the first chunk of every
    /// burst and (in slow streams) every token.
    #[test]
    fn drain_turn_events_dispatches_first_event() {
        let mut s = app_state();
        let (_tx, mut rx) = mpsc::channel::<TurnEvent>(10_000);
        // Simulate the select! arm: it received "hello" and passed it as
        // `first`. Nothing else is queued (slow stream). The drain must
        // still dispatch "hello".
        let first = Some(TurnEvent::Token("hello".into()));
        drain_turn_events(&mut s, first, &mut rx);
        assert_eq!(s.conversation.messages.len(), 1);
        assert_eq!(
            s.conversation.messages[0].content, "hello",
            "first event from select! must be dispatched, not dropped"
        );
    }

    /// Regression: a burst of events all arrive before the loop wakes.
    /// `select!` consumes the first ("a"); the rest sit in the channel.
    /// All four must be dispatched in order.
    #[test]
    fn drain_turn_events_dispatches_first_plus_drained_burst() {
        let mut s = app_state();
        let (tx, mut rx) = mpsc::channel::<TurnEvent>(10_000);
        tx.try_send(TurnEvent::Token("b".into())).unwrap();
        tx.try_send(TurnEvent::Token("c".into())).unwrap();
        tx.try_send(TurnEvent::Token("d".into())).unwrap();
        // select! consumed "a"; b/c/d remain queued.
        let first = Some(TurnEvent::Token("a".into()));
        drain_turn_events(&mut s, first, &mut rx);
        assert_eq!(s.conversation.messages.len(), 1);
        assert_eq!(
            s.conversation.messages[0].content, "abcd",
            "first event + drained burst must all be dispatched in order"
        );
    }

    /// `drain_approval_requests` replaces the pending approval
    /// when a new one arrives, but **denies the old one first** —
    /// the previous audit found that dropping the old oneshot
    /// sender hangs the executor forever.
    #[tokio::test]
    async fn drain_replaces_pending_and_denies_old() {
        let mut s = app_state();
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

        drain_approval_requests(&mut s, None, &mut approval_rx);

        // Old responder received Denied before being dropped.
        let old_answer: Option<ApprovalResponse> = old_rx.try_recv().ok();
        assert_eq!(old_answer, Some(ApprovalResponse::Denied));
        // The pending approval is now the new one
        assert!(s.approval.pending_approval.is_some());
        assert_eq!(
            s.approval.pending_approval.as_ref().unwrap().tool_name,
            "edit_file"
        );
    }

    /// `PullProgress` updates `state.provider.pull_progress` and marks state dirty
    /// so the TUI re-renders the progress bar.
    #[test]
    fn pull_progress_updates_state_and_marks_dirty() {
        let mut s = app_state();
        s.dirty = false;
        dispatch_turn_event(
            &mut s,
            TurnEvent::PullProgress {
                status: "pulling manifest".into(),
                completed: None,
                total: None,
            },
        );
        let p = s
            .provider
            .pull_progress
            .as_ref()
            .expect("pull_progress set");
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
        let p = s
            .provider
            .pull_progress
            .as_ref()
            .expect("pull_progress still set");
        assert_eq!(p.status, "downloading");
        assert_eq!(p.completed, Some(128 * 1024 * 1024));
        assert_eq!(p.total, Some(512 * 1024 * 1024));
    }

    /// `DoomLoopDetected` sets `state.doom.doom_loop` so the banner
    /// widget can render, marks the state dirty, and pushes a
    /// human-readable system message into the conversation.
    /// The banner itself is a render-time decision keyed on
    /// `count >= THRESHOLD && !acknowledged`, so we just verify
    /// the state here.
    #[test]
    fn doom_loop_detected_sets_state_and_marks_dirty() {
        let mut s = app_state();
        s.dirty = false;
        assert!(s.doom.doom_loop.is_none());
        dispatch_turn_event(
            &mut s,
            TurnEvent::DoomLoopDetected {
                count: 3,
                tool: "bash".into(),
                last_error: "command not found".into(),
            },
        );
        let dl = s.doom.doom_loop.as_ref().expect("doom_loop set");
        assert_eq!(dl.count, 3);
        assert_eq!(dl.tool, "bash");
        assert_eq!(dl.last_error, "command not found");
        assert!(!dl.acknowledged, "freshly-set doom loop is unacknowledged");
        assert!(s.dirty, "doom loop event should mark state dirty");

        // The system message describes the loop for the user.
        let last = s
            .conversation
            .messages
            .back()
            .expect("system message pushed");
        assert_eq!(last.role, "system");
        assert!(last.content.contains("bash"));
        assert!(last.content.contains('3'));
    }

    /// A second `DoomLoopDetected` event overwrites the previous
    /// state (the count may have grown). The banner is keyed on
    /// the latest snapshot, not on the first one we ever saw.
    #[test]
    fn doom_loop_detected_overwrites_previous_state() {
        let mut s = app_state();
        dispatch_turn_event(
            &mut s,
            TurnEvent::DoomLoopDetected {
                count: 3,
                tool: "bash".into(),
                last_error: "boom".into(),
            },
        );
        dispatch_turn_event(
            &mut s,
            TurnEvent::DoomLoopDetected {
                count: 5,
                tool: "grep".into(),
                last_error: "still broken".into(),
            },
        );
        let dl = s.doom.doom_loop.as_ref().expect("doom_loop still set");
        assert_eq!(dl.count, 5);
        assert_eq!(dl.tool, "grep");
        assert_eq!(dl.last_error, "still broken");
    }

    /// `ContinuationRound` sets `state.generation.continuation` and marks dirty.
    #[test]
    fn continuation_round_sets_state_and_marks_dirty() {
        let mut s = app_state();
        s.dirty = false;
        assert!(s.generation.continuation.is_none());
        dispatch_turn_event(&mut s, TurnEvent::ContinuationRound { round: 3, max: 5 });
        assert_eq!(s.generation.continuation, Some((3, 5)));
        assert!(s.dirty);
    }

    /// `MemoryExtracted` mirrors the store size + turn into
    /// `state.session.memory_status` and marks dirty so the status bar
    /// updates in real-time as memory grows.
    #[test]
    fn memory_extracted_updates_status_and_marks_dirty() {
        let mut s = app_state();
        s.dirty = false;
        assert!(s.session.memory_status.is_none());
        dispatch_turn_event(&mut s, TurnEvent::MemoryExtracted { count: 3, turn: 5 });
        assert_eq!(s.session.memory_status, Some((3, 5)));
        assert!(s.dirty);
        // A later extraction overwrites (does not accumulate).
        dispatch_turn_event(&mut s, TurnEvent::MemoryExtracted { count: 7, turn: 8 });
        assert_eq!(s.session.memory_status, Some((7, 8)));
    }

    // ── Mouse handler (WO 27.7) ──────────────────────────────────

    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    fn mouse(kind: MouseEventKind, row: u16, column: u16) -> MouseEvent {
        MouseEvent {
            kind,
            row,
            column,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }

    /// ScrollUp scrolls the chat up, turns auto-follow off, and marks dirty.
    #[test]
    fn mouse_scroll_up_offsets_and_sticks() {
        let mut s = app_state();
        s.conversation.scroll_offset = 10;
        s.conversation.max_scroll = 100;
        s.conversation.auto_scroll = true;
        s.dirty = false;
        handle_mouse_event(&mut s, mouse(MouseEventKind::ScrollUp, 5, 5));
        assert_eq!(s.conversation.scroll_offset, 7);
        assert!(!s.conversation.auto_scroll);
        assert!(s.dirty);
    }

    /// ScrollDown advances the offset but never past `max_scroll`.
    #[test]
    fn mouse_scroll_down_clamps_to_max() {
        let mut s = app_state();
        s.conversation.scroll_offset = 98;
        s.conversation.max_scroll = 100;
        handle_mouse_event(&mut s, mouse(MouseEventKind::ScrollDown, 5, 5));
        assert_eq!(s.conversation.scroll_offset, 100);
        // Already at the bottom — stays clamped.
        handle_mouse_event(&mut s, mouse(MouseEventKind::ScrollDown, 5, 5));
        assert_eq!(s.conversation.scroll_offset, 100);
    }

    /// WO 34.1: the top tab bar is gone; row 0 is the header. A click
    /// on row 0 no longer switches tabs — it grabs the chat for
    /// drag-scroll like any other non-input row. This pins the removal
    /// so a future regression that re-introduces a row-0 tab-bar click
    /// handler is caught.
    #[test]
    fn mouse_click_header_grabs_chat_not_tab() {
        let mut s = app_state();
        assert_eq!(s.ui.active_tab, ActiveTab::None);
        s.conversation.auto_scroll = true;
        // Click row 0 (the header) at an arbitrary column.
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Down(MouseButton::Left), 0, 45),
        );
        // No tab switch happened — still chat-only.
        assert_eq!(s.ui.active_tab, ActiveTab::None);
        // The header click grabbed the chat for drag-scroll.
        assert!(!s.conversation.auto_scroll);
        assert_eq!(s.ui.mouse_drag_row, Some(0));
    }

    /// A click on row 0 in what used to be a separator gutter is also a
    /// drag-grab now (no tabs to switch between). Kept as the successor
    /// to the old `mouse_click_tab_gutter_is_noop` test.
    #[test]
    fn mouse_click_header_gutter_grabs_chat() {
        let mut s = app_state();
        handle_mouse_event(&mut s, mouse(MouseEventKind::Down(MouseButton::Left), 0, 9));
        assert_eq!(s.ui.active_tab, ActiveTab::None);
        assert_eq!(s.ui.mouse_drag_row, Some(0));
    }

    /// Clicking the chat body (row > 0) grabs it for drag-scroll:
    /// auto-follow turns off and the drag baseline is recorded.
    #[test]
    fn mouse_click_body_grabs_chat_for_drag() {
        let mut s = app_state();
        s.conversation.auto_scroll = true;
        s.ui.mouse_drag_row = None;
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Down(MouseButton::Left), 10, 5),
        );
        assert!(!s.conversation.auto_scroll);
        assert_eq!(s.ui.mouse_drag_row, Some(10));
    }

    /// Drag scrolls the chat by the row delta (natural scrolling:
    /// drag down → content moves down → offset shrinks).
    #[test]
    fn mouse_drag_scrolls_by_row_delta() {
        let mut s = app_state();
        s.conversation.scroll_offset = 50;
        s.conversation.max_scroll = 100;
        s.conversation.auto_scroll = true;
        // Start drag at row 20.
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Down(MouseButton::Left), 20, 5),
        );
        assert_eq!(s.conversation.scroll_offset, 50);
        // Drag down 3 rows → offset drops by 3.
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Drag(MouseButton::Left), 23, 5),
        );
        assert_eq!(s.conversation.scroll_offset, 47);
        // Drag up 1 row from there → offset grows by 1.
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Drag(MouseButton::Left), 22, 5),
        );
        assert_eq!(s.conversation.scroll_offset, 48);
        assert!(!s.conversation.auto_scroll);
    }

    /// Drag never scrolls below 0 or above `max_scroll`.
    #[test]
    fn mouse_drag_clamps_to_scroll_bounds() {
        let mut s = app_state();
        s.conversation.max_scroll = 100;

        // Lower clamp: drag down past the top → stops at 0 (no underflow).
        s.conversation.scroll_offset = 1;
        handle_mouse_event(&mut s, mouse(MouseEventKind::Down(MouseButton::Left), 5, 5));
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Drag(MouseButton::Left), 55, 5),
        );
        assert_eq!(s.conversation.scroll_offset, 0);

        // Upper clamp: drag up past the bottom → stops at max_scroll.
        s.conversation.scroll_offset = 95;
        s.ui.mouse_drag_row = Some(100);
        handle_mouse_event(&mut s, mouse(MouseEventKind::Drag(MouseButton::Left), 0, 5));
        assert_eq!(s.conversation.scroll_offset, 100);
    }

    /// MouseUp ends the drag: the baseline is cleared so a later drag
    /// without a fresh Down does nothing.
    #[test]
    fn mouse_up_ends_drag() {
        let mut s = app_state();
        s.conversation.scroll_offset = 10;
        s.conversation.max_scroll = 100;
        handle_mouse_event(&mut s, mouse(MouseEventKind::Down(MouseButton::Left), 5, 5));
        handle_mouse_event(&mut s, mouse(MouseEventKind::Up(MouseButton::Left), 5, 5));
        assert_eq!(s.ui.mouse_drag_row, None);
        let before = s.conversation.scroll_offset;
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Drag(MouseButton::Left), 50, 5),
        );
        assert_eq!(s.conversation.scroll_offset, before);
    }

    // ── Click-in-prompt cursor positioning (WO 32.12) ──────────────

    #[test]
    fn mouse_click_in_input_moves_cursor() {
        use ratatui::layout::Rect;
        let mut s = app_state();
        s.conversation.input = "hello world".to_string();
        s.conversation.cursor_position = 0;
        s.ui.last_input_rect = Some(Rect::new(0, 20, 40, 3));
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Down(MouseButton::Left), 21, 6),
        );
        assert_eq!(s.conversation.cursor_position, 5);
    }

    #[test]
    fn mouse_click_past_end_clamps() {
        use ratatui::layout::Rect;
        let mut s = app_state();
        s.conversation.input = "hi".to_string();
        s.conversation.cursor_position = 0;
        s.ui.last_input_rect = Some(Rect::new(0, 20, 40, 3));
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Down(MouseButton::Left), 21, 30),
        );
        assert_eq!(s.conversation.cursor_position, 2);
    }

    #[test]
    fn mouse_click_outside_input_still_drags_chat() {
        use ratatui::layout::Rect;
        let mut s = app_state();
        s.conversation.input = "hello".to_string();
        s.conversation.cursor_position = 0;
        s.conversation.auto_scroll = true;
        s.ui.last_input_rect = Some(Rect::new(0, 20, 40, 3));
        handle_mouse_event(
            &mut s,
            mouse(MouseEventKind::Down(MouseButton::Left), 10, 5),
        );
        assert_eq!(s.conversation.cursor_position, 0);
        assert!(!s.conversation.auto_scroll);
        assert_eq!(s.ui.mouse_drag_row, Some(10));
    }

    #[test]
    fn set_cursor_line_col_handles_multiline() {
        let mut s = app_state();
        s.conversation.input = "abc\ndefgh\nij".to_string();
        s.set_cursor_line_col(0, 1);
        assert_eq!(s.conversation.cursor_position, 1);
        s.set_cursor_line_col(1, 2);
        assert_eq!(s.conversation.cursor_position, 6);
        s.set_cursor_line_col(1, 100);
        assert_eq!(s.conversation.cursor_position, 9);
        s.set_cursor_line_col(5, 0);
        assert_eq!(s.conversation.cursor_position, 12);
    }

    /// Multibyte regression (WO 43.19): `cursor_position` is a CHAR
    /// index everywhere (cursor_byte:808, keys:1299, apply_paste:839).
    /// The previous `set_cursor_line_col` used `char_indices()` (BYTE
    /// offsets), so a multibyte char on an earlier line made the
    /// cursor land wrong. On ASCII the byte and char offsets are
    /// identical, which is why the test above passed but this one fails
    /// on the pre-fix code.
    #[test]
    fn set_cursor_line_col_handles_multibyte_on_earlier_line() {
        let mut s = app_state();
        // "🎉" is 1 char, 4 bytes. Line 0 has 3 chars (a,🎉,b); line 1
        // starts at CHAR index 4 (after the \n).
        s.conversation.input = "a🎉b\nc".to_string();
        // Click line 1, col 0 → char index 4 (the 'c'). Pre-fix returned
        // 7 (byte offset of 'c'), which is past the 5-char input → the
        // `input.chars().count()` fall-through? No: 7 > 5 so it clamped
        // to end (5). Either way, wrong.
        s.set_cursor_line_col(1, 0);
        assert_eq!(
            s.conversation.cursor_position, 4,
            "cursor must be char index 4 ('c'), not byte offset 7"
        );
        // col 1 on line 1 is past 'c' → clamp to end of line 1 = 5.
        s.set_cursor_line_col(1, 1);
        assert_eq!(s.conversation.cursor_position, 5);
        // Click line 0, col 1 → between 'a' and '🎉' = char index 1.
        s.set_cursor_line_col(0, 1);
        assert_eq!(s.conversation.cursor_position, 1);
        // Click line 0, col 2 → after '🎉' = char index 2.
        s.set_cursor_line_col(0, 2);
        assert_eq!(s.conversation.cursor_position, 2);
    }

    /// CJK multibyte (3 bytes/char) on an earlier line. Same bug class:
    /// byte offset > char index. Two CJK chars on line 0 = 6 bytes but
    /// 2 chars; line 1 starts at char index 3 (after \n at char 2).
    #[test]
    fn set_cursor_line_col_handles_cjk_multibyte() {
        let mut s = app_state();
        s.conversation.input = "你好\nx".to_string();
        // line 1 starts at char index 3 (你,好,\n). col 0 → 3.
        s.set_cursor_line_col(1, 0);
        assert_eq!(s.conversation.cursor_position, 3);
        // line 0, col 1 → after '你' = char index 1.
        s.set_cursor_line_col(0, 1);
        assert_eq!(s.conversation.cursor_position, 1);
        // line 0, col 2 → after '好' = char index 2 (the \n).
        s.set_cursor_line_col(0, 2);
        assert_eq!(s.conversation.cursor_position, 2);
    }

    /// Combining character (é as 'e' + combining acute U+0301) on an
    /// earlier line. Rust `chars()` yields both code points, so
    /// `cursor_position` counts them as 2 chars. The bug treated the
    /// 2-byte 'e' + 2-byte combiner as 4 bytes, landing the cursor off
    /// by 2 on the next line.
    #[test]
    fn set_cursor_line_col_handles_combining_char_on_earlier_line() {
        let mut s = app_state();
        // "e\u{0301}" = é (2 code points, 4 bytes). Line 0 = "e\u{0301}z"
        // (3 chars, 6 bytes); line 1 starts at char index 4.
        s.conversation.input = "e\u{0301}z\nw".to_string();
        s.set_cursor_line_col(1, 0);
        assert_eq!(s.conversation.cursor_position, 4);
        // col 1 on line 0 = after 'e' = char index 1.
        s.set_cursor_line_col(0, 1);
        assert_eq!(s.conversation.cursor_position, 1);
    }

    /// PTY tail cut at 64 KiB with a multibyte char straddling the cut
    /// point must not panic and must produce char-aligned output
    /// (WO 43.25). Before the fix, `&last.content[start..]` panicked
    /// because `start` landed mid-character; the `char_indices` fixup
    /// that followed was dead code (sliced first).
    #[test]
    fn bash_partial_output_tail_cut_handles_multibyte_without_panic() {
        let mut s = app_state();
        // Seed a tool message so the BashPartialOutput arm appends to it.
        s.conversation
            .messages
            .push_back(ConversationEntry::new("tool", ""));

        // Build content whose 64 KiB tail boundary lands inside `🎉`
        // (4 bytes). We want the char that straddles `start` to be
        // multibyte, so the tail starts mid-character.
        const PTY_TAIL_BYTES: usize = 64 * 1024;
        // content = prefix + "🎉" + suffix, total > PTY_TAIL_BYTES, and
        // start = total - PTY_TAIL_BYTES lands inside the emoji.
        // Want start in [prefix_len+1, prefix_len+3] (inside the emoji).
        //   start = prefix_len + 4 + suffix_len - PTY_TAIL_BYTES
        //   pick suffix_len = PTY_TAIL_BYTES - 2 => start = prefix_len + 2.
        let emoji = "🎉";
        let suffix_len = PTY_TAIL_BYTES - 2;
        let prefix_len = PTY_TAIL_BYTES + 10; // ensures total > PTY_TAIL_BYTES
        let prefix = "a".repeat(prefix_len);
        let suffix = "b".repeat(suffix_len);
        let content = format!("{prefix}{emoji}{suffix}");
        let total = content.len();
        assert!(total > PTY_TAIL_BYTES);
        let start = total - PTY_TAIL_BYTES;
        // Confirm the cut lands inside the emoji (mid-char).
        assert!(!content.is_char_boundary(start));
        assert!(start > prefix_len && start < prefix_len + emoji.len());

        // Drive the handler: append a chunk that takes content over the
        // cap. We push the whole content as one chunk for the test.
        dispatch_turn_event(
            &mut s,
            TurnEvent::BashPartialOutput {
                call_id: String::new(),
                text: content,
            },
        );

        // Did not panic. The tool message is now the bounded tail form.
        let last = s.conversation.messages.back().expect("tool msg present");
        assert_eq!(last.role, "tool");
        // Output must be valid UTF-8 (no sliced char) — `to_string`
        // would have panicked on invalid utf8 during the format!.
        // Verify the boundary walk: the tail after the marker line is
        // char-aligned.
        assert!(last.content.starts_with("… ["));
        let header_end = last.content.find('\n').expect("header line");
        let tail = &last.content[header_end + 1..];
        assert!(tail.is_char_boundary(0));
        // The prefix `a`s were trimmed (the tail starts inside the
        // emoji region, not back in the prefix).
        assert!(!tail.starts_with('a'));
    }
}
