//! Input-mode keyboard handler.
//!
//! This is the regular (non-approval) key handling path. It lives in its own
//! module so `tui/mod.rs` can stay focused on the event-loop orchestration.
//!
//! The handler takes a single `HandleInputContext` instead of a long parameter
//! list so the orchestrator can pass all channels in one struct.  The
//! signature is `async fn handle_input_key(key, state, ctx) -> anyhow::Result<()>`.
//! The orchestrator calls us only when `state.pending_approval.is_none()`.

use crate::session::conversation::ConversationLog;
use crate::session::executor::TurnEvent;
use crate::session::prompt::CompactRequest;
use crate::shared::Config;
use crate::tui::app::{AppState, ConversationEntry};
use crate::tui::commands::PersonaResult;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirkforge_plugin_host::PluginRegistry;
use tokio::sync::mpsc;

mod slash_commands;
mod text;

use slash_commands::{dispatch_slash_command, SlashContext};
use text::{
    char_index_for_line_col, current_line_bounds, delete_word_backward, search_nav_direction,
    SearchDirection,
};

/// All channel endpoints the input-mode key handler needs.
///
/// Bundling the senders removes the 13-argument signature flagged in
/// review.md and makes it impossible to swap two similar-looking channels at
/// the call site.
pub(crate) struct HandleInputContext<'a> {
    pub input_tx: &'a mpsc::UnboundedSender<String>,
    pub cancel_tx: &'a mpsc::UnboundedSender<()>,
    pub resume_tx: &'a mpsc::UnboundedSender<ConversationLog>,
    pub compact_tx: &'a mpsc::UnboundedSender<CompactRequest>,
    pub model_tx: &'a mpsc::UnboundedSender<String>,
    pub undo_tx: &'a mpsc::UnboundedSender<()>,
    pub config_tx: &'a mpsc::UnboundedSender<Config>,
    pub plan_tx: &'a mpsc::UnboundedSender<bool>,
    pub persona_tx: &'a mpsc::UnboundedSender<PersonaResult>,
    pub event_tx: &'a mpsc::Sender<TurnEvent>,
    pub plugin_reload_tx: &'a mpsc::UnboundedSender<PluginRegistry>,
}

/// Split a `!` command's formatted output into a two-line summary and the
/// full output, for use with `ConversationEntry::tool(summary, full)`.
///
/// The summary is always the first two lines of the formatted output (the
/// `$ <cmd>` header and the `✅/❌/⏰` banner). If the command produced no
/// output, the second line is empty and the summary is just the header.
/// The full output is the entire formatted string. This mirrors the
/// `tool_should_collapse` / `expanded_tools` pattern: the chat panel
/// shows only the summary by default; Enter or Tab on empty input
/// expands it.
pub(crate) fn split_bang_summary(formatted: &str) -> (String, String) {
    let mut lines = formatted.splitn(3, '\n');
    let first = lines.next().unwrap_or("").to_string();
    let second = lines.next().unwrap_or("").to_string();
    let summary = format!("{first}\n{second}");
    (summary, formatted.to_string())
}

/// Handle a single key event in the regular input mode.
///
/// Returns `Ok(())` after a single event. Only errors on I/O failure
/// (e.g. terminal draw failure bubbling up from the event loop — in
/// practice this function itself does no I/O, so `Ok(())` is the only
/// realistic outcome, but we keep `Result` for symmetry with the caller).
pub(crate) async fn handle_input_key(
    key: KeyEvent,
    state: &mut AppState,
    ctx: &HandleInputContext<'_>,
) -> anyhow::Result<()> {
    // ── Session picker interceptor ─────────────────────────
    // When the recent-session picker overlay is active, all keys route
    // to it. Enter confirms the selection and resumes the session;
    // Esc/q cancels. The overlay is cleared once a choice is made.
    //
    // We `take()` the picker out of AppState while handling it so the
    // mutable borrow of `state.session_picker` does not conflict with
    // the mutable borrow of `state` passed to `resume_conversation_log`.
    if let Some(mut picker) = state.session_picker.take() {
        let consumed = picker.handle_key(key);
        if consumed && picker.is_confirmed() {
            if let Some(path) = picker.selected_path() {
                match crate::session::conversation::ConversationLog::open_async(path).await {
                    Ok((log, _outcome)) => {
                        let msg = crate::tui::commands::resume_conversation_log(
                            log,
                            state,
                            ctx.resume_tx,
                        )
                        .await;
                        state
                            .messages
                            .push_back(ConversationEntry::new("system", msg));
                    }
                    Err(e) => {
                        state.messages.push_back(ConversationEntry::new(
                            "system",
                            format!("Error resuming session: {e}"),
                        ));
                    }
                }
            }
            // Picker is consumed: don't restore it.
            return Ok(());
        }
        if consumed && picker.is_cancelled() {
            // Picker is consumed: don't restore it.
            return Ok(());
        }
        // Ctrl+C always exits, even from the picker.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            state.should_exit = true;
            return Ok(());
        }
        // Key did not finalize the picker and was not a global shortcut:
        // cancel the overlay and fall through so the key is handled as
        // normal input. This lets slash commands such as `/exit` work even
        // when the startup picker is showing — the first character dismisses
        // the picker and is typed into the input box, and the rest of the
        // command follows. Navigation keys (arrows, j/k, Enter, Esc, q) are
        // consumed above.
    }

    // ── Search mode interceptor ─────────────────────────────
    // When search_mode is on, the input box is acting as a search
    // bar. We intercept Enter, Esc, Backspace, and any printable
    // char here so the regular input handling doesn't fire. `n`
    // / `N` (navigate next/prev match) are handled below the
    // search-mode branch — they're only meaningful AFTER a search
    // has been committed, not while typing a new query.
    if state.search_mode {
        match key.code {
            KeyCode::Esc => {
                state.search_mode = false;
                state.search_query.clear();
                state.search_matches.clear();
                state.search_match_idx = 0;
                return Ok(());
            }
            KeyCode::Enter => {
                // Commit the search. The matches are computed
                // from the current query; the renderer can now
                // highlight them. If there are matches we leave
                // search mode (so `n` / `N` can cycle) and jump to
                // the first one, expanding any collapsed tool card
                // that contains the match.
                let matches = crate::tui::search::compute_matches(
                    state.messages.make_contiguous(),
                    &state.search_query,
                );
                state.search_matches = matches;
                state.search_match_idx = 0;
                if !state.search_matches.is_empty() {
                    state.search_mode = false;
                    if let Some(offset) = crate::tui::widgets::chat::scroll_offset_for_search_match(
                        state,
                        state.last_content_width,
                    ) {
                        state.auto_scroll = false;
                        state.scroll_offset = offset;
                    }
                }
                return Ok(());
            }
            KeyCode::Backspace => {
                state.search_query.pop();
                return Ok(());
            }
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    state.search_query.push(c);
                    return Ok(());
                }
                // Ctrl+C in search mode = cancel and exit.
                if c == 'c' {
                    state.search_mode = false;
                    state.search_query.clear();
                    state.search_matches.clear();
                    state.search_match_idx = 0;
                    state.input.clear();
                    state.cursor_position = 0;
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    // ── Post-search navigation (n / Shift+N) ─────────────
    // Only active when a search is committed (matches is
    // non-empty). Falls through to regular handling otherwise.
    if !state.search_matches.is_empty() && !state.search_mode {
        match search_nav_direction(&key) {
            Some(SearchDirection::Next) => {
                if let Some(idx) = crate::tui::search::navigate_next(
                    state.search_match_idx,
                    state.search_matches.len(),
                ) {
                    state.search_match_idx = idx;
                    if let Some(offset) = crate::tui::widgets::chat::scroll_offset_for_search_match(
                        state,
                        state.last_content_width,
                    ) {
                        state.auto_scroll = false;
                        state.scroll_offset = offset;
                    }
                }
                return Ok(());
            }
            Some(SearchDirection::Prev) => {
                if let Some(idx) = crate::tui::search::navigate_prev(
                    state.search_match_idx,
                    state.search_matches.len(),
                ) {
                    state.search_match_idx = idx;
                    if let Some(offset) = crate::tui::widgets::chat::scroll_offset_for_search_match(
                        state,
                        state.last_content_width,
                    ) {
                        state.auto_scroll = false;
                        state.scroll_offset = offset;
                    }
                }
                return Ok(());
            }
            None => {}
        }
    }
    match key.code {
        KeyCode::Char(c) => {
            // Ctrl+Shift+C: copy the last assistant message to the
            // system clipboard. The SHIFT-included modifier check
            // has to come BEFORE the plain Ctrl-only check below —
            // otherwise the SHIFT bit is ignored and we fall into
            // the cancel-current-generation path.
            if key
                .modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
                && (c == 'c' || c == 'C')
            {
                let last = state
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant")
                    .map(|m| m.content.clone());
                let line = match last {
                    Some(text) if !text.is_empty() => {
                        match crate::tui::clipboard::copy_to_clipboard(&text) {
                            Ok(n) => format!("📋 Copied {n} chars to clipboard"),
                            Err(e) => {
                                format!("📋 Clipboard error: {e}")
                            }
                        }
                    }
                    Some(_) | None => "📋 No assistant message to copy".to_string(),
                };
                state
                    .messages
                    .push_back(ConversationEntry::new("system", line));
                return Ok(());
            }
            // Ctrl+Shift+B: copy a code block from the most recent
            // assistant message. The first press copies the last block;
            // repeated presses cycle backward through earlier blocks in
            // that message, so the user can copy any block without
            // per-block mouse focus.
            if key
                .modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
                && (c == 'b' || c == 'B')
            {
                let blocks: Vec<String> = state
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant")
                    .map(|m| crate::tui::rendering::all_code_blocks(&m.content))
                    .unwrap_or_default();
                let line = if blocks.is_empty() {
                    "📋 No code block to copy".to_string()
                } else {
                    // Start at the most recent (last) block and cycle backward on
                    // repeated presses. `blocks` is in document order, so the last
                    // block is at blocks.len() - 1.
                    let offset = state.code_block_copy_index % blocks.len();
                    let idx = (blocks.len() - 1).wrapping_sub(offset);
                    state.code_block_copy_index = (state.code_block_copy_index + 1) % blocks.len();
                    let text = &blocks[idx];
                    match crate::tui::clipboard::copy_to_clipboard(text) {
                        Ok(n) => format!(
                            "📋 Copied code block {}/{} ({} chars) to clipboard",
                            idx + 1,
                            blocks.len(),
                            n
                        ),
                        Err(e) => format!("📋 Clipboard error: {e}"),
                    }
                };
                state
                    .messages
                    .push_back(ConversationEntry::new("system", line));
                return Ok(());
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                // Ctrl+F is a no-op while in search mode (the
                // input box is the search box; we don't want to
                // toggle out of it).
                if c == 'f' && !state.search_mode {
                    state.search_mode = true;
                    state.search_query.clear();
                    state.search_matches.clear();
                    state.search_match_idx = 0;
                    return Ok(());
                }
                match c {
                    'c' => {
                        // Ctrl+C: cancel a running persona first, then
                        // cancel in-flight generation. If nothing is running,
                        // treat it as a quit signal so the user can escape the
                        // app the same way every other terminal app works.
                        if let Some(cancel) = state.persona_cancel.take() {
                            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                            state.persona_in_progress = None;
                            state.is_generating = false;
                            state.messages.push_back(ConversationEntry::new(
                                "system",
                                "⛔ Persona cancelled.".to_string(),
                            ));
                            state.input.clear();
                            state.cursor_position = 0;
                            return Ok(());
                        }
                        if state.is_generating {
                            if ctx.cancel_tx.send(()).is_err() {
                                // The executor driver is gone — the
                                // session is ending or the TUI is
                                // shutting down. Treat the key as a
                                // quit signal instead of leaving the
                                // user stuck in a dead loop.
                                tracing::debug!(
                                    "cancel_tx receiver dropped on Ctrl+C; executor already gone"
                                );
                                state.should_exit = true;
                                return Ok(());
                            }
                            state.is_generating = false;
                            state.input.clear();
                            state.cursor_position = 0;
                            return Ok(());
                        }
                        state.should_exit = true;
                        return Ok(());
                    }
                    'w' => {
                        // Ctrl+W: delete word backward within the current line.
                        let byte_pos = state.cursor_byte();
                        let (line_start, line_end) = current_line_bounds(&state.input, byte_pos);
                        let line = &state.input[line_start..line_end];
                        let rel_cursor = byte_pos - line_start;
                        let (new_line, new_rel_cursor) = delete_word_backward(line, rel_cursor);
                        state.input = format!(
                            "{}{}{}",
                            &state.input[..line_start],
                            new_line,
                            &state.input[line_end..]
                        );
                        state.cursor_position =
                            state.input[..line_start].chars().count() + new_rel_cursor;
                    }
                    'u' => {
                        // Ctrl+U: clear from the start of the current line to
                        // the cursor. In a single-line input this clears the
                        // whole line; in a multi-line input it clears only the
                        // current line's prefix.
                        let byte_pos = state.cursor_byte();
                        let (line_start, _) = current_line_bounds(&state.input, byte_pos);
                        state.input =
                            format!("{}{}", &state.input[..line_start], &state.input[byte_pos..]);
                        state.cursor_position = state.input[..line_start].chars().count();
                    }
                    'l' => {
                        // Ctrl+L: clear screen (terminal handles this)
                    }
                    't' => {
                        // Ctrl+T: toggle tool output collapse. When ON, tool
                        // entries show only a one-line summary; when OFF,
                        // they render the full output (the legacy flooding
                        // behavior). Per-entry expansion in `expanded_tools`
                        // overrides this global flag.
                        state.tool_collapsed = !state.tool_collapsed;
                        if state.tool_collapsed {
                            // Re-collapse: forget any per-entry expansions so
                            // the user gets a clean collapsed view.
                            state.expanded_tools.clear();
                        }
                    }
                    _ => {}
                }
            } else {
                let byte_pos = state.cursor_byte();
                state.input.insert(byte_pos, c);
                state.cursor_position += 1;
            }
        }
        KeyCode::Tab => {
            // Tab on an empty input toggles expand/collapse on the most
            // recent message. Tool entries use `expanded_tools`; all other
            // messages use `collapsed_messages`.
            if state.input.is_empty() {
                if let Some(last_idx) = state.messages.len().checked_sub(1) {
                    if state.messages[last_idx].role == "tool"
                        && state.messages[last_idx].tool_output.is_some()
                    {
                        if state.expanded_tools.contains(&last_idx) {
                            state.expanded_tools.remove(&last_idx);
                        } else {
                            state.expanded_tools.insert(last_idx);
                        }
                    } else if state.collapsed_messages.contains(&last_idx) {
                        state.collapsed_messages.remove(&last_idx);
                    } else {
                        state.collapsed_messages.insert(last_idx);
                    }
                    return Ok(());
                }
            }
        }
        KeyCode::Backspace => {
            if state.cursor_position > 0 {
                // Move back one char in char-index terms, then find the byte
                // offset of the char we want to remove.
                state.cursor_position -= 1;
                let remove_byte = state.cursor_byte();
                state.input.remove(remove_byte);
            }
        }
        KeyCode::Delete => {
            let char_count = state.input.chars().count();
            if state.cursor_position < char_count {
                let byte_pos = state.cursor_byte();
                state.input.remove(byte_pos);
            }
        }
        KeyCode::Left => {
            let (line, col) = state.cursor_line_col();
            if col > 0 {
                state.cursor_position -= 1;
            } else if line > 0 {
                let lines: Vec<&str> = state.input.split('\n').collect();
                let prev_len = lines[line - 1].chars().count();
                state.cursor_position = char_index_for_line_col(&state.input, line - 1, prev_len);
            }
        }
        KeyCode::Right => {
            let (line, col) = state.cursor_line_col();
            let lines: Vec<&str> = state.input.split('\n').collect();
            let line_len = lines[line].chars().count();
            if col < line_len {
                state.cursor_position += 1;
            } else if line + 1 < lines.len() {
                state.cursor_position = char_index_for_line_col(&state.input, line + 1, 0);
            }
        }
        KeyCode::Home => {
            let (line, _) = state.cursor_line_col();
            state.cursor_position = char_index_for_line_col(&state.input, line, 0);
        }
        KeyCode::End => {
            let (line, _) = state.cursor_line_col();
            let line_len = state
                .input
                .split('\n')
                .nth(line)
                .map(|l| l.chars().count())
                .unwrap_or(0);
            state.cursor_position = char_index_for_line_col(&state.input, line, line_len);
        }
        KeyCode::Enter => {
            // Shift+Enter / Alt+Enter insert a literal newline instead of
            // submitting the input. This is the only way to type multi-line
            // prompts in the TUI input box.
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
            {
                let byte_pos = state.cursor_byte();
                state.input.insert(byte_pos, '\n');
                state.cursor_position += 1;
                return Ok(());
            }
            // v1.2-p14 — `!` bash passthrough. A line beginning with `!`
            // (and at least one non-`!` char after it) runs directly via
            // /bin/sh with no model round trip and (when
            // `bang_requires_approval` is set in config) through the
            // approval gate. The returned string is rendered as a tool
            // entry so the existing collapse/expand UX in `chat.rs`
            // applies — a 500-line `!find` doesn't flood the chat.
            //
            // Review.md arch concern #1: the `bang_requires_approval`
            // config flag was previously defined but not wired into this
            // branch — a security hole. We now route through the gate
            // when the flag is on, and only run directly when it's off.
            if let Some(rest) = state.input.strip_prefix('!') {
                let rest = rest.to_string();
                state.input.clear();
                state.cursor_position = 0;

                let config = crate::shared::read_shared_config(&state.config).clone();
                match crate::tui::commands::bang_permission_action(&rest, &config) {
                    crate::shared::permission::PermissionAction::Deny => {
                        state.messages.push_back(crate::tui::app::ConversationEntry::new(
                            "system",
                            format!("🚫 Permission rule denied `!{rest}` — the command matches a deny rule."),
                        ));
                        return Ok(());
                    }
                    crate::shared::permission::PermissionAction::Ask => {
                        // Park the command on AppState and let the next
                        // event-loop iteration render the approval dialog.
                        // The user hits Y to run, N/Esc to discard. We
                        // intentionally do NOT run the command here — that
                        // would defeat the gate.
                        state.pending_bang =
                            Some(crate::tui::app::PendingBangCommand { cmd: rest });
                        return Ok(());
                    }
                    crate::shared::permission::PermissionAction::Allow => {
                        let out = crate::tui::commands::handle_bang_command(&rest, &config).await;
                        // Split into summary (first line) and full output so the
                        // collapse UX has something to show by default. The
                        // summary is "$ <cmd>\n<icon> exit <code>" — two lines.
                        // Full output is everything.
                        let (summary, full) = split_bang_summary(&out);
                        state
                            .messages
                            .push_back(crate::tui::app::ConversationEntry::tool(summary, full));
                        return Ok(());
                    }
                }
            }

            // If the most recent message is a collapsed tool entry and
            // we're not actively typing a real input, Enter expands it
            // in-place. This is the discoverable "peek under the hood"
            // gesture — a long tool output stays one line until the user
            // asks for it. We only intercept Enter when the input buffer
            // is empty so users can still send messages.
            if state.input.is_empty() {
                if let Some(last_idx) = state.messages.len().checked_sub(1) {
                    if state.messages[last_idx].role == "tool"
                        && state.messages[last_idx].tool_output.is_some()
                    {
                        if state.expanded_tools.contains(&last_idx) {
                            state.expanded_tools.remove(&last_idx);
                        } else {
                            state.expanded_tools.insert(last_idx);
                        }
                    } else if state.collapsed_messages.contains(&last_idx) {
                        state.collapsed_messages.remove(&last_idx);
                    } else {
                        state.collapsed_messages.insert(last_idx);
                    }
                    return Ok(());
                }
            }

            let input = state.input.clone();
            state.input.clear();
            state.cursor_position = 0;

            if !input.is_empty() {
                if input.starts_with('/') {
                    let parts: Vec<&str> = input.splitn(2, ' ').collect();
                    let cmd = parts[0];
                    let args = parts.get(1).copied().unwrap_or("");

                    let slash_ctx = SlashContext {
                        cancel_tx: ctx.cancel_tx,
                        resume_tx: ctx.resume_tx,
                        compact_tx: ctx.compact_tx,
                        model_tx: ctx.model_tx,
                        undo_tx: ctx.undo_tx,
                        config_tx: ctx.config_tx,
                        plan_tx: ctx.plan_tx,
                        persona_tx: ctx.persona_tx,
                        event_tx: ctx.event_tx,
                        plugin_reload_tx: ctx.plugin_reload_tx,
                    };
                    let handled = dispatch_slash_command(cmd, args, state, &slash_ctx).await?;
                    if !handled {
                        if let Some(skill) = state.skill_registry.get_by_trigger(cmd) {
                            if let Err(e) = crate::session::skills::Skill::tokenize_args(args) {
                                state.messages.push_back(ConversationEntry::new(
                                    "system",
                                    format!("❌ Invalid arguments for {cmd}: {e}"),
                                ));
                                return Ok(());
                            }
                            let rendered = skill.render_prompt(args);
                            state.messages.push_back(ConversationEntry::new(
                                "system",
                                format!(
                                    "🔧 Running skill: {} — {}",
                                    skill.meta.name, skill.meta.description
                                ),
                            ));
                            state.is_generating = true;
                            if ctx.input_tx.send(rendered).is_err() {
                                tracing::warn!(skill = %skill.meta.name, "input_tx receiver dropped while dispatching skill prompt");
                                state.is_generating = false;
                                return Ok(());
                            }
                        } else {
                            state.messages.push_back(ConversationEntry::new(
                                "system",
                                format!(
                                    "Unknown command: {cmd}\nType /help for available commands."
                                ),
                            ));
                        }
                    }
                } else {
                    // Regular message — push to display and send to executor.
                    // v1.2-p15: expand `@<path>` mentions inline before sending.
                    let mentions = crate::tui::commands::parse_mentions(&input);
                    let path_guard = crate::session::access::PathGuard::default();
                    let expansions = crate::tui::commands::expand_mentions(&mentions, &path_guard);
                    let cleaned = if mentions.is_empty() {
                        input.clone()
                    } else {
                        crate::tui::commands::strip_mentions(&input, &mentions)
                    };
                    let rendered_block = crate::tui::commands::render_mentions_block(&expansions);
                    let status_msg = crate::tui::commands::format_mention_status(&expansions);

                    state
                        .messages
                        .push_back(ConversationEntry::new("user", cleaned.clone()));
                    if !status_msg.is_empty() {
                        state
                            .messages
                            .push_back(ConversationEntry::new("system", status_msg));
                    }
                    state.is_generating = true;
                    let prompt = if rendered_block.is_empty() {
                        cleaned
                    } else {
                        format!("{cleaned}{rendered_block}")
                    };
                    if ctx.input_tx.send(prompt).is_err() {
                        // Same pattern as the skill branch — the
                        // executor is gone, so the spinner we'd
                        // otherwise be stuck on would never get
                        // cleared. Bail to the main loop and let it
                        // see the empty TUI/executor state.
                        tracing::warn!(
                            "input_tx receiver dropped while dispatching slash-command prompt"
                        );
                        state.is_generating = false;
                        return Ok(());
                    }
                }
            }
        }
        KeyCode::Esc => {
            // Toggle thinking panel
            state.thinking_panel_visible = !state.thinking_panel_visible;
        }
        KeyCode::Up => {
            if state.input.contains('\n') {
                let (line, col) = state.cursor_line_col();
                if line > 0 {
                    let lines: Vec<&str> = state.input.split('\n').collect();
                    let new_col = col.min(lines[line - 1].chars().count());
                    state.cursor_position =
                        char_index_for_line_col(&state.input, line - 1, new_col);
                }
            } else {
                // Scroll up (see older content)
                state.auto_scroll = false;
                state.scroll_offset = state.scroll_offset.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if state.input.contains('\n') {
                let (line, col) = state.cursor_line_col();
                let lines: Vec<&str> = state.input.split('\n').collect();
                if line + 1 < lines.len() {
                    let new_col = col.min(lines[line + 1].chars().count());
                    state.cursor_position =
                        char_index_for_line_col(&state.input, line + 1, new_col);
                }
            } else {
                // Scroll down (see newer content)
                // Clamp to max_scroll so the view doesn't run off the bottom
                // waiting for the next render to correct it.
                state.scroll_offset = (state.scroll_offset + 1).min(state.max_scroll);
            }
        }
        KeyCode::PageUp => {
            state.auto_scroll = false;
            state.scroll_offset = state.scroll_offset.saturating_sub(10);
        }
        KeyCode::PageDown => {
            state.scroll_offset = (state.scroll_offset + 10).min(state.max_scroll);
        }
        _ => {}
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{delete_word_backward, search_nav_direction, SearchDirection};
    use crate::session::prompt::CompactRequest;

    fn check(input: &str, cursor_byte: usize, expected_input: &str, expected_cursor: usize) {
        let (got_input, got_cursor) = delete_word_backward(input, cursor_byte);
        assert_eq!(got_input, expected_input, "input mismatch for {input:?}");
        assert_eq!(got_cursor, expected_cursor, "cursor mismatch for {input:?}");
    }

    #[test]
    fn delete_word_backward_preserves_leading_whitespace() {
        // "   hello|" should become "   |" — leading spaces stay.
        check("   hello", 8, "   ", 3);
    }

    #[test]
    fn delete_word_backward_removes_word_and_separating_spaces() {
        // "one   two|" should become "one|".
        check("one   two", 9, "one", 3);
    }

    #[test]
    fn delete_word_backward_removes_trailing_whitespace_run() {
        // "hello   |" should become "hello|".
        check("hello   ", 8, "hello", 5);
    }

    #[test]
    fn delete_word_backward_removes_single_word_from_start() {
        // "hello|" should become "|".
        check("hello", 5, "", 0);
    }

    #[test]
    fn delete_word_backward_removes_leading_whitespace_when_no_word_before() {
        // "   |" should become "|".
        check("   ", 3, "", 0);
    }

    #[test]
    fn delete_word_backward_removes_leading_whitespace_before_word_ahead() {
        // "   |hello" should become "|hello".
        check("   hello", 3, "hello", 0);
    }

    #[test]
    fn delete_word_backward_cursor_at_start_is_noop() {
        check("hello", 0, "hello", 0);
    }

    #[test]
    fn delete_word_backward_handles_multibyte_characters() {
        // "héllo world|" should become "héllo|" (cursor_byte is byte offset).
        let input = "héllo world";
        let cursor_byte = input.len(); // 12 bytes
        check(input, cursor_byte, "héllo", 5);
    }

    use super::{char_index_for_line_col, handle_input_key, HandleInputContext};
    use crate::session::conversation::ConversationLog;
    use crate::session::executor::TurnEvent;
    use crate::shared::Config;
    use crate::tui::app::AppState;
    use crate::tui::commands::PersonaResult;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc;

    fn key(c: char, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), mods)
    }

    #[test]
    fn search_nav_direction_plain_n_is_next() {
        assert_eq!(
            search_nav_direction(&key('n', KeyModifiers::NONE)),
            Some(SearchDirection::Next)
        );
    }

    #[test]
    fn search_nav_direction_shift_n_is_prev() {
        assert_eq!(
            search_nav_direction(&key('N', KeyModifiers::SHIFT)),
            Some(SearchDirection::Prev)
        );
    }

    #[test]
    fn search_nav_direction_ignores_other_keys() {
        assert_eq!(search_nav_direction(&key('x', KeyModifiers::NONE)), None);
    }

    #[test]
    fn search_nav_direction_ignores_modified_n() {
        assert_eq!(search_nav_direction(&key('n', KeyModifiers::CONTROL)), None);
        assert_eq!(
            search_nav_direction(&key('N', KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            None
        );
    }

    #[test]
    fn char_index_for_line_col_maps_back_to_position() {
        // line 0: "ab", line 1: "c"
        let input = "ab\nc";
        assert_eq!(char_index_for_line_col(input, 0, 0), 0);
        assert_eq!(char_index_for_line_col(input, 0, 1), 1);
        assert_eq!(char_index_for_line_col(input, 0, 2), 2); // before newline
        assert_eq!(char_index_for_line_col(input, 1, 0), 3);
        assert_eq!(char_index_for_line_col(input, 1, 1), 4);
        // Clamp past end.
        assert_eq!(char_index_for_line_col(input, 1, 10), 4);
    }

    #[tokio::test]
    async fn shift_enter_inserts_newline_without_sending() {
        let mut state = AppState::new(Arc::new(RwLock::new(Config::default())));
        state.input = "hello".into();
        state.cursor_position = 5;

        let (input_tx, _input_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = mpsc::unbounded_channel();
        let (resume_tx, _resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
        let (compact_tx, _compact_rx) = mpsc::unbounded_channel();
        let (model_tx, _model_rx) = mpsc::unbounded_channel();
        let (undo_tx, _undo_rx) = mpsc::unbounded_channel();
        let (config_tx, _config_rx) = mpsc::unbounded_channel::<Config>();
        let (plan_tx, _plan_rx) = mpsc::unbounded_channel::<bool>();
        let (persona_tx, _persona_rx) = mpsc::unbounded_channel::<PersonaResult>();
        let (event_tx, _event_rx) = mpsc::channel::<TurnEvent>(10_000);
        let (plugin_reload_tx, _plugin_reload_rx) =
            mpsc::unbounded_channel::<kirkforge_plugin_host::PluginRegistry>();

        let ctx = HandleInputContext {
            input_tx: &input_tx,
            cancel_tx: &cancel_tx,
            resume_tx: &resume_tx,
            compact_tx: &compact_tx,
            model_tx: &model_tx,
            undo_tx: &undo_tx,
            config_tx: &config_tx,
            plan_tx: &plan_tx,
            persona_tx: &persona_tx,
            event_tx: &event_tx,
            plugin_reload_tx: &plugin_reload_tx,
        };
        let result = handle_input_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            &mut state,
            &ctx,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(state.input, "hello\n");
        assert_eq!(state.cursor_position, 6);
        // No message sent.
        assert!(state.messages.is_empty());
    }

    #[tokio::test]
    async fn arrow_keys_move_across_input_lines() {
        let mut state = AppState::new(Arc::new(RwLock::new(Config::default())));
        state.input = "ab\ncd".into();
        // Start at end: line 1, col 2 (char index 4).
        state.cursor_position = 4;

        let (input_tx, _input_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = mpsc::unbounded_channel();
        let (resume_tx, _resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
        let (compact_tx, _compact_rx) = mpsc::unbounded_channel();
        let (model_tx, _model_rx) = mpsc::unbounded_channel();
        let (undo_tx, _undo_rx) = mpsc::unbounded_channel();
        let (config_tx, _config_rx) = mpsc::unbounded_channel::<Config>();
        let (plan_tx, _plan_rx) = mpsc::unbounded_channel::<bool>();
        let (persona_tx, _persona_rx) = mpsc::unbounded_channel::<PersonaResult>();
        let (event_tx, _event_rx) = mpsc::channel::<TurnEvent>(10_000);
        let (plugin_reload_tx, _plugin_reload_rx) =
            mpsc::unbounded_channel::<kirkforge_plugin_host::PluginRegistry>();

        let ctx = HandleInputContext {
            input_tx: &input_tx,
            cancel_tx: &cancel_tx,
            resume_tx: &resume_tx,
            compact_tx: &compact_tx,
            model_tx: &model_tx,
            undo_tx: &undo_tx,
            config_tx: &config_tx,
            plan_tx: &plan_tx,
            persona_tx: &persona_tx,
            event_tx: &event_tx,
            plugin_reload_tx: &plugin_reload_tx,
        };

        async fn send(state: &mut AppState, key: KeyEvent, ctx: &HandleInputContext<'_>) {
            handle_input_key(key, state, ctx).await.unwrap();
        }

        send(
            &mut state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &ctx,
        )
        .await;
        assert_eq!(state.cursor_position, 1); // col 1 on line 0 (clamped from 2)

        send(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &ctx,
        )
        .await;
        assert_eq!(state.cursor_position, 4); // back to end of line 1

        send(
            &mut state,
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            &ctx,
        )
        .await;
        assert_eq!(state.cursor_position, 3); // start of line 1

        send(
            &mut state,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
            &ctx,
        )
        .await;
        assert_eq!(state.cursor_position, 2); // end of line 0
    }

    #[tokio::test]
    async fn enter_runs_plugins_command_and_pushes_system_message() {
        let mut state = AppState::new(Arc::new(RwLock::new(Config::default())));
        state.input = "/plugins list".into();

        let (input_tx, _input_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = mpsc::unbounded_channel();
        let (resume_tx, _resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
        let (compact_tx, _compact_rx) = mpsc::unbounded_channel::<CompactRequest>();
        let (model_tx, _model_rx) = mpsc::unbounded_channel();
        let (undo_tx, _undo_rx) = mpsc::unbounded_channel();
        let (config_tx, _config_rx) = mpsc::unbounded_channel::<Config>();
        let (plan_tx, _plan_rx) = mpsc::unbounded_channel::<bool>();
        let (persona_tx, _persona_rx) = mpsc::unbounded_channel::<PersonaResult>();
        let (event_tx, _event_rx) = mpsc::channel::<TurnEvent>(10_000);
        let (plugin_reload_tx, _plugin_reload_rx) =
            mpsc::unbounded_channel::<kirkforge_plugin_host::PluginRegistry>();

        let ctx = HandleInputContext {
            input_tx: &input_tx,
            cancel_tx: &cancel_tx,
            resume_tx: &resume_tx,
            compact_tx: &compact_tx,
            model_tx: &model_tx,
            undo_tx: &undo_tx,
            config_tx: &config_tx,
            plan_tx: &plan_tx,
            persona_tx: &persona_tx,
            event_tx: &event_tx,
            plugin_reload_tx: &plugin_reload_tx,
        };
        let result = handle_input_key(KeyEvent::from(KeyCode::Enter), &mut state, &ctx).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].role, "system");
        assert!(
            state.messages[0].content.contains("Active plugins"),
            "unexpected message: {}",
            state.messages[0].content
        );
    }
}
