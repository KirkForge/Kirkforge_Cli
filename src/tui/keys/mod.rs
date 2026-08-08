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
use crate::tui::app::ActiveTab;
use crate::tui::app::{AppState, ConversationEntry};
use crate::tui::commands::PersonaResult;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kf_plugin_host::PluginRegistry;
use tokio::sync::mpsc;

pub(crate) mod slash_commands;
mod text;

use slash_commands::{complete_command, dispatch_slash_command, SlashContext};
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

/// Apply a doom-loop banner action. Always marks the banner
/// `acknowledged` so it hides; the chosen action also dispatches a
/// follow-up effect (cancel generation, switch to plan mode, or
/// just dismiss). Splitting the side effects from the key handler
/// keeps the key handler readable.
async fn handle_doom_action(
    action: crate::tui::widgets::doom_banner::DoomLoopAction,
    state: &mut AppState,
    ctx: &HandleInputContext<'_>,
) {
    use crate::tui::widgets::doom_banner::DoomLoopAction;
    if let Some(ref mut dl) = state.doom_loop {
        dl.acknowledged = true;
    }
    state.mark_dirty();
    match action {
        DoomLoopAction::Break => {
            // Cancel the in-flight generation. The cancel channel
            // is the same one the user hits Ctrl+C for; it's the
            // standard "stop what you're doing" signal.
            crate::send_or_warn!(
                ctx.cancel_tx.send(()),
                "doom-loop break: cancel channel receiver dropped"
            );
            state.messages.push_back(ConversationEntry::new(
                "system",
                "⏹ Break: cancelled in-flight generation to escape the doom loop.",
            ));
        }
        DoomLoopAction::Plan => {
            // Switch into plan mode. Plan mode disables all mutating
            // tools at the dispatch layer, so even if the model tries
            // the same broken approach again, it cannot repeat the
            // destructive side effect.
            crate::send_or_warn!(
                ctx.plan_tx.send(true),
                "doom-loop plan: plan channel receiver dropped"
            );
            state.messages.push_back(ConversationEntry::new(
                "system",
                "📐 Plan: switched to plan mode to break the doom loop. Type /implement when ready to exit plan mode.",
            ));
        }
        DoomLoopAction::Continue => {
            // Just dismiss. The executor-side tracker keeps its
            // window so the next identical error will re-fire the
            // banner if the model hasn't broken out of the loop
            // yet. That's the point — we let the user opt out of
            // the warning without losing it.
            state.messages.push_back(ConversationEntry::new(
                "system",
                "▶️ Continue: dismissed doom-loop warning. The model will keep trying; the banner will re-appear if the loop continues.",
            ));
        }
    }
}

async fn handle_doom_loop_keys(
    key: KeyEvent,
    state: &mut AppState,
    ctx: &HandleInputContext<'_>,
) -> Option<anyhow::Result<()>> {
    let dl = state.doom_loop.as_ref()?;
    if dl.count < crate::session::executor::DoomLoopTracker::THRESHOLD || dl.acknowledged {
        return None;
    }
    use crate::tui::widgets::doom_banner::DoomLoopAction;
    match key.code {
        KeyCode::Left => {
            let cur = state.doom_loop_selection.index;
            let len = DoomLoopAction::ALL.len();
            state.doom_loop_selection.index = (cur + len - 1) % len;
            state.mark_dirty();
        }
        KeyCode::Right => {
            let cur = state.doom_loop_selection.index;
            let len = DoomLoopAction::ALL.len();
            state.doom_loop_selection.index = (cur + 1) % len;
            state.mark_dirty();
        }
        KeyCode::Enter => {
            let action = state.doom_loop_selection.selected();
            handle_doom_action(action, state, ctx).await;
        }
        KeyCode::Esc => {
            handle_doom_action(DoomLoopAction::Continue, state, ctx).await;
        }
        _ => {}
    }
    Some(Ok(()))
}

fn handle_slash_menu_keys(key: KeyEvent, state: &mut AppState) -> Option<anyhow::Result<()>> {
    let menu = state.slash_menu.as_mut()?;
    match key.code {
        KeyCode::Up => {
            if menu.selected > 0 {
                menu.selected -= 1;
            }
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Down => {
            menu.selected += 1;
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Enter => {
            let commands = complete_command(&menu.query);
            if menu.selected < commands.len() {
                state.input = commands[menu.selected].to_string();
                state.cursor_position = state.input.chars().count();
            }
            state.slash_menu = None;
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Esc => {
            state.slash_menu = None;
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Backspace => {
            menu.query.pop();
            menu.selected = 0;
            if menu.query.is_empty() {
                state.slash_menu = None;
            }
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            menu.query.push(c);
            menu.selected = 0;
            state.mark_dirty();
            Some(Ok(()))
        }
        _ => None,
    }
}

fn handle_file_completer_keys(key: KeyEvent, state: &mut AppState) -> Option<anyhow::Result<()>> {
    let completer = state.file_completer.as_mut()?;
    match key.code {
        KeyCode::Up => {
            if completer.selected > 0 {
                completer.selected -= 1;
            }
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Down => {
            completer.selected += 1;
            if !completer.entries.is_empty() {
                completer.selected = completer.selected.min(completer.entries.len() - 1);
            }
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Enter => {
            if !completer.entries.is_empty() && completer.selected < completer.entries.len() {
                let entry = completer.entries[completer.selected].clone();
                let path = completer.dir.join(&entry);
                if path.is_dir() {
                    if completer.pick_directory {
                        if std::env::set_current_dir(&path).is_ok() {
                            state.cwd = path.clone();
                        }
                        state.file_completer = None;
                    } else {
                        let mut new_entries = Vec::new();
                        if let Ok(rd) = std::fs::read_dir(&path) {
                            for de in rd.flatten() {
                                if let Some(name) = de.file_name().to_str() {
                                    new_entries.push(name.to_string());
                                }
                            }
                        }
                        new_entries.sort();
                        completer.dir = path;
                        completer.entries = new_entries;
                        completer.selected = 0;
                        completer.query.clear();
                    }
                } else if !completer.pick_directory {
                    let rel = format!("@{}", completer.dir.join(&entry).display());
                    state.input = rel;
                    state.cursor_position = state.input.chars().count();
                    state.file_completer = None;
                }
            }
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Esc => {
            state.file_completer = None;
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Backspace => {
            if let Some(parent) = completer.dir.parent() {
                if parent != completer.dir {
                    let mut new_entries = Vec::new();
                    if let Ok(rd) = std::fs::read_dir(parent) {
                        for de in rd.flatten() {
                            if let Some(name) = de.file_name().to_str() {
                                new_entries.push(name.to_string());
                            }
                        }
                    }
                    new_entries.sort();
                    completer.dir = parent.to_path_buf();
                    completer.entries = new_entries;
                    completer.selected = 0;
                    completer.query.clear();
                }
            }
            state.mark_dirty();
            Some(Ok(()))
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            completer.query.push(c);
            let filtered: Vec<String> = completer
                .entries
                .iter()
                .filter(|e| {
                    e.to_lowercase()
                        .starts_with(&completer.query.to_lowercase())
                })
                .cloned()
                .collect();
            completer.entries = if filtered.is_empty() {
                completer.query.pop();
                let mut all = Vec::new();
                if let Ok(rd) = std::fs::read_dir(&completer.dir) {
                    for de in rd.flatten() {
                        if let Some(name) = de.file_name().to_str() {
                            all.push(name.to_string());
                        }
                    }
                }
                all.sort();
                all
            } else {
                filtered
            };
            completer.selected = 0;
            state.mark_dirty();
            Some(Ok(()))
        }
        _ => None,
    }
}

async fn handle_session_picker_keys(
    key: KeyEvent,
    state: &mut AppState,
    ctx: &HandleInputContext<'_>,
) -> Option<anyhow::Result<()>> {
    let mut picker = state.session_picker.take()?;
    let consumed = picker.handle_key(key);
    if consumed && picker.is_confirmed() {
        if let Some(path) = picker.selected_path() {
            match crate::session::conversation::ConversationLog::open_async(path).await {
                Ok((log, _outcome)) => {
                    let msg =
                        crate::tui::commands::resume_conversation_log(log, state, ctx.resume_tx)
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
        return Some(Ok(()));
    }
    if consumed && picker.is_cancelled() {
        return Some(Ok(()));
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        state.should_exit = true;
        return Some(Ok(()));
    }
    None
}

fn handle_search_mode_keys(key: KeyEvent, state: &mut AppState) -> Option<anyhow::Result<()>> {
    if !state.search_mode {
        return None;
    }
    match key.code {
        KeyCode::Esc => {
            state.search_mode = false;
            state.search_query.clear();
            state.search_matches.clear();
            state.search_match_idx = 0;
        }
        KeyCode::Enter => {
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
        }
        KeyCode::Backspace => {
            state.search_query.pop();
        }
        KeyCode::Char(c) => {
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                state.search_query.push(c);
            } else if c == 'c' {
                state.search_mode = false;
                state.search_query.clear();
                state.search_matches.clear();
                state.search_match_idx = 0;
                state.input.clear();
                state.cursor_position = 0;
            }
        }
        _ => return None,
    }
    Some(Ok(()))
}

fn handle_search_nav_keys(key: KeyEvent, state: &mut AppState) -> Option<anyhow::Result<()>> {
    if state.search_matches.is_empty() || state.search_mode {
        return None;
    }
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
        }
        None => return None,
    }
    Some(Ok(()))
}

/// Handle Enter on a non-Chat tab. Each tab gets a minimal action:
/// - Models (F2): show model details in a status message
/// - Plugins (F3): toggle the selected plugin on/off
/// - Jobs (F4): list all jobs
/// - Settings (F5): show the value of the selected config key
/// - Threads (F6): no-op (session picker handles its own Enter)
async fn handle_tab_enter(
    state: &mut AppState,
    ctx: &HandleInputContext<'_>,
) -> anyhow::Result<()> {
    let sel = match state.tab_list_state {
        Some(i) => i,
        None => {
            state
                .messages
                .push_back(ConversationEntry::new("system", "No row selected."));
            return Ok(());
        }
    };

    match state.active_tab {
        ActiveTab::Models => {
            if let Some(ref info) = state.model_info {
                let msg = format!(
                    "Model: {} (context: {} tokens)",
                    info.name,
                    crate::tui::rendering::format_token_count(info.max_context_tokens)
                );
                state
                    .messages
                    .push_back(ConversationEntry::new("system", msg));
            } else {
                state
                    .messages
                    .push_back(ConversationEntry::new("system", "No model connected."));
            }
            state.mark_dirty();
        }
        ActiveTab::Plugins => {
            let name = {
                let config = crate::shared::read_shared_config(&state.config);
                let names: Vec<String> = config.tools.plugin_sources.keys().cloned().collect();
                // render_plugins has 2 header lines before data rows.
                let idx = sel.saturating_sub(2);
                match names.get(idx) {
                    Some(n) => n.clone(),
                    None => {
                        state
                            .messages
                            .push_back(ConversationEntry::new("system", "No plugin at this row."));
                        return Ok(());
                    }
                }
            };
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
            dispatch_slash_command("/plugins", &format!("toggle {name}"), state, &slash_ctx)
                .await?;
            state.mark_dirty();
        }
        ActiveTab::Jobs => {
            let msg = crate::tui::commands::handle_jobs_command("", state).await;
            state
                .messages
                .push_back(ConversationEntry::new("system", msg));
            state.mark_dirty();
        }
        ActiveTab::Settings => {
            let line = {
                let config = crate::shared::read_shared_config(&state.config);
                let lines = settings_keys_and_values(&config);
                // render_settings has 2 header lines before data rows.
                let idx = sel.saturating_sub(2);
                match lines.get(idx) {
                    Some(l) => l.clone(),
                    None => "No setting at this row.".to_string(),
                }
            };
            state
                .messages
                .push_back(ConversationEntry::new("system", line));
            state.mark_dirty();
        }
        ActiveTab::Threads | ActiveTab::Chat => {}
    }
    Ok(())
}

/// Collect Settings tab key=value lines in the same order as
/// `render_settings`, so the Enter handler can look up the selected row.
fn settings_keys_and_values(config: &Config) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("default_model: {}", config.model.default_model));
    lines.push(format!("ollama_host: {}", config.model.ollama_host));
    lines.push(format!(
        "anthropic_provider: {}",
        config.model.anthropic_provider
    ));
    lines.push(format!("cache_enabled: {}", config.model.cache_enabled));
    lines.push(format!("auto_approve: {}", config.security.auto_approve));
    lines.push(format!(
        "sandbox_dir: {}",
        config.security.sandbox_dir.as_deref().unwrap_or("(none)")
    ));
    lines.push(format!(
        "block_dotfiles: {}",
        config.security.block_dotfiles
    ));
    lines.push(format!(
        "bang_requires_approval: {}",
        config.security.bang_requires_approval
    ));
    lines.push(format!("dry_run: {}", config.tools.dry_run));
    lines.push(format!("follow_symlinks: {}", config.tools.follow_symlinks));
    lines.push(format!(
        "max_tool_calls_per_turn: {}",
        config.tools.max_tool_calls_per_turn
    ));
    lines.push(format!(
        "carryover_enabled: {}",
        config.session.carryover_enabled
    ));
    lines.push(format!(
        "worktree_enabled: {}",
        config.session.worktree_enabled
    ));
    lines
}

pub(crate) async fn handle_input_key(
    key: KeyEvent,
    state: &mut AppState,
    ctx: &HandleInputContext<'_>,
) -> anyhow::Result<()> {
    if let Some(result) = handle_doom_loop_keys(key, state, ctx).await {
        return result;
    }
    if let Some(result) = handle_slash_menu_keys(key, state) {
        return result;
    }
    if let Some(result) = handle_file_completer_keys(key, state) {
        return result;
    }
    if let Some(result) = handle_session_picker_keys(key, state, ctx).await {
        return result;
    }
    if let Some(result) = handle_search_mode_keys(key, state) {
        return result;
    }
    if let Some(result) = handle_search_nav_keys(key, state) {
        return result;
    }
    if key.code != KeyCode::Tab {
        state.completion_suggestions.clear();
    }
    match key.code {
        // ── F-key tab switching ───────────────────────────────────
        // F1–F6 switch between Chat, Models, Plugins, Jobs, Settings, Threads.
        // Esc returns to Chat from any non-Chat tab.
        k if ActiveTab::from_key_code(k).is_some() => {
            let new_tab = ActiveTab::from_key_code(k).unwrap();
            // Reset list state when switching tabs so the highlight
            // doesn't carry over from a previous tab.
            if new_tab != state.active_tab {
                state.tab_list_state = if new_tab == ActiveTab::Chat {
                    None
                } else {
                    Some(0)
                };
            }
            state.active_tab = new_tab;
            if new_tab == ActiveTab::Jobs && state.cached_jobs_output.is_none() {
                state.jobs_dirty = true;
            }
            state.mark_dirty();
        }
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
                                tracing::trace!(
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
                    'o' => {
                        // Ctrl+O: open directory picker (file completer in
                        // directory-pick mode). Only directories confirm;
                        // Enter on a directory chdirs and closes the picker.
                        let cwd = state.cwd.clone();
                        let mut entries = Vec::new();
                        if let Ok(rd) = std::fs::read_dir(&cwd) {
                            for de in rd.flatten() {
                                if let Some(name) = de.file_name().to_str() {
                                    entries.push(name.to_string());
                                }
                            }
                        }
                        entries.sort();
                        state.file_completer = Some(crate::tui::app::FileCompleter {
                            dir: cwd,
                            entries,
                            selected: 0,
                            query: String::new(),
                            pick_directory: true,
                        });
                    }
                    _ => {}
                }
            } else {
                let byte_pos = state.cursor_byte();
                state.input.insert(byte_pos, c);
                state.cursor_position += 1;
                // ── Slash menu: open when `/` is the first character ──
                if c == '/' && state.input.starts_with('/') && state.input.chars().count() == 1 {
                    state.slash_menu = Some(crate::tui::app::SlashMenu {
                        query: String::new(),
                        selected: 0,
                    });
                } else if let Some(ref mut menu) = state.slash_menu {
                    // Append to the filter query while the popup is open.
                    menu.query.push(c);
                    menu.selected = 0;
                }
                // ── File completer: open when `@` is typed ──
                if c == '@' && state.input.starts_with('@') && state.input.chars().count() == 1 {
                    let cwd = std::env::current_dir().unwrap_or_default();
                    let mut entries = Vec::new();
                    if let Ok(rd) = std::fs::read_dir(&cwd) {
                        for de in rd.flatten() {
                            if let Some(name) = de.file_name().to_str() {
                                entries.push(name.to_string());
                            }
                        }
                    }
                    entries.sort();
                    state.file_completer = Some(crate::tui::app::FileCompleter {
                        dir: cwd,
                        entries,
                        selected: 0,
                        query: String::new(),
                        pick_directory: false,
                    });
                } else if let Some(ref mut completer) = state.file_completer {
                    if c != '/' {
                        completer.query.push(c);
                        completer.selected = 0;
                    }
                }
            }
        }
        KeyCode::Tab => {
            // Tab-completion (WO 14.6). When the buffer starts with
            // `/` or `@` and the cursor is right after the trigger
            // char, complete against `COMMANDS` (slash) or the
            // filesystem (@-mention paths). One match → replace the
            // buffer with it; many → show a one-line suggestion list
            // in `completion_suggestions`; zero → no-op. The existing
            // "Tab on empty input toggles expand/collapse" behavior is
            // preserved when the buffer is empty.
            if try_completion(state) {
                return Ok(());
            }
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
            // On non-Chat tabs, Enter invokes a tab-specific action.
            if state.active_tab != ActiveTab::Chat {
                handle_tab_enter(state, ctx).await?;
                return Ok(());
            }
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
            // On non-Chat tabs, Esc returns to Chat. Otherwise toggle
            // the thinking panel (the original behavior).
            if state.active_tab != ActiveTab::Chat {
                state.active_tab = ActiveTab::Chat;
                state.tab_list_state = None;
            } else {
                state.thinking_panel_visible = !state.thinking_panel_visible;
            }
            // Also dismiss any slash menu or file completer popup.
            if state.slash_menu.is_some() {
                state.slash_menu = None;
            }
            if state.file_completer.is_some() {
                state.file_completer = None;
            }
        }
        KeyCode::Up => {
            // On non-Chat tabs with list state, move selection up.
            if state.active_tab != ActiveTab::Chat {
                if let Some(idx) = state.tab_list_state {
                    state.tab_list_state = Some(idx.saturating_sub(1));
                    state.mark_dirty();
                }
            } else if state.input.contains('\n') {
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
            // On non-Chat tabs with list state, move selection down.
            if state.active_tab != ActiveTab::Chat {
                if let Some(idx) = state.tab_list_state {
                    state.tab_list_state = Some(idx + 1);
                    state.mark_dirty();
                }
            } else if state.input.contains('\n') {
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

// ── Tab completion helpers (WO 14.6) ─────────────────────────────
// The two completion sources are the `COMMANDS` table (slash) and the
// filesystem (@-mention paths). Both are prefix-match (readline
// contract — no fuzzy). A single match replaces the buffer; multiple
// matches fill `state.completion_suggestions` so the renderer shows a
// one-line hint. Returns true if the Tab key was consumed (a completion
// was attempted), false to let the caller fall through to the legacy
// empty-input expand/collapse behavior.

fn try_completion(state: &mut AppState) -> bool {
    // Completion only fires on the first line (no multi-line @-mentions
    // mid-buffer) and when the cursor is past the trigger char.
    let (line, col) = state.cursor_line_col();
    if line != 0 {
        return false;
    }
    let chars: Vec<char> = state.input.chars().collect();
    if chars.is_empty() || col <= 1 {
        return false;
    }
    let first = chars[0];
    if first != '/' && first != '@' {
        return false;
    }
    let prefix: String = chars[1..col].iter().collect();
    match first {
        '/' => complete_slash(state, &prefix),
        '@' => complete_mention(state, &prefix),
        _ => false,
    }
}

// Slash completion. `complete_command` returns triggers that already
// include the leading `/`, so we pass an empty trigger to
// `apply_completion` (it would otherwise double the slash).
fn complete_slash(state: &mut AppState, prefix: &str) -> bool {
    let matches = complete_command(prefix);
    apply_completion(state, "", matches.into_iter().map(String::from).collect())
}

// @-mention path completion. The `:A-B:raw` suffix is a modifier, not a
// path — only complete the path portion up to the first `:`. We read the
// parent directory of the typed prefix and list entries starting with
// the last path component. The directory portion of the typed prefix is
// preserved so `@src/ma` → `@src/main.rs` (not `@main.rs`).
fn complete_mention(state: &mut AppState, prefix: &str) -> bool {
    let (path_part, suffix) = match prefix.split_once(':') {
        Some((p, s)) => (p, format!(":{s}")),
        None => (prefix, String::new()),
    };
    let (dir, _last) = split_path_prefix(path_part);
    let entries = complete_path(prefix);
    let dir_prefix = if dir == "." {
        String::new()
    } else {
        format!("{dir}/")
    };
    let matches: Vec<String> = entries
        .into_iter()
        .map(|name| format!("{dir_prefix}{name}{suffix}"))
        .collect();
    apply_completion(state, "@", matches)
}

// Shared apply logic for both completion kinds. `trigger` is "/" or "@".
// On a single match, replace the whole buffer with `trigger + match`.
// On many, store them in `completion_suggestions`. Returns true (the
// Tab was consumed) in both cases; the caller never falls through.
fn apply_completion(state: &mut AppState, trigger: &str, matches: Vec<String>) -> bool {
    state.completion_suggestions.clear();
    match matches.len() {
        0 => {
            // No match — no-op (the WO allows a beep; we stay silent).
            true
        }
        1 => {
            let completed = format!("{trigger}{}", matches.into_iter().next().unwrap());
            state.input = completed;
            state.cursor_position = state.input.chars().count();
            state.mark_dirty();
            true
        }
        _ => {
            state.completion_suggestions = matches;
            state.mark_dirty();
            true
        }
    }
}

// Filesystem path completion for @-mentions. `prefix` is the text after
// `@` up to the cursor, e.g. `src/main` from `@src/main`. The `:A-B:raw`
// suffix is split off (only the path portion before the first `:` is
// completed). Returns matching entry names (without the `@`).
//
// Capped at a small constant so a giant directory never floods the
// suggestion line. Entries are sorted for a stable display.
fn complete_path(prefix: &str) -> Vec<String> {
    let path_part = prefix.split(':').next().unwrap_or(prefix);
    let (dir, last) = split_path_prefix(path_part);
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with(&last) {
                    out.push(name.to_string());
                    if out.len() >= 24 {
                        break;
                    }
                }
            }
        }
    }
    out.sort();
    out
}

// Split a path prefix into (parent_dir, last_component). An empty or
// bare `foo` prefix completes against `.`. A trailing separator means
// "list the whole dir" (last component empty).
fn split_path_prefix(prefix: &str) -> (String, String) {
    if prefix.is_empty() {
        return (".".to_string(), String::new());
    }
    // Split on the LAST separator (either `/` or `\`) so the
    // completion works on Windows where `Path::display()` emits
    // backslashes. `read_dir` takes OS-native paths, so the dir
    // portion keeps its native separators.
    let (dir, last) = match prefix.rfind(['/', '\\']) {
        Some(idx) => {
            let (d, l) = prefix.split_at(idx);
            (d.to_string(), l[1..].to_string())
        }
        None => (".".to_string(), prefix.to_string()),
    };
    let dir = if dir.is_empty() { ".".to_string() } else { dir };
    (dir, last)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::complete_path;
    use super::{delete_word_backward, search_nav_direction, split_path_prefix, SearchDirection};
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
    use crate::shared::test_util::app_state;
    use crate::shared::Config;
    use crate::tui::app::AppState;
    use crate::tui::commands::PersonaResult;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
        let mut state = app_state();
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
            mpsc::unbounded_channel::<kf_plugin_host::PluginRegistry>();

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
        let mut state = app_state();
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
            mpsc::unbounded_channel::<kf_plugin_host::PluginRegistry>();

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
        let mut state = app_state();
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
            mpsc::unbounded_channel::<kf_plugin_host::PluginRegistry>();

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

    // ── WO 14.6 Tab-completion tests ──────────────────────────────

    #[test]
    fn split_path_prefix_empty_completes_cwd() {
        assert_eq!(split_path_prefix(""), (".".to_string(), String::new()));
    }

    #[test]
    fn split_path_prefix_bare_name_completes_dot() {
        assert_eq!(
            split_path_prefix("foo"),
            (".".to_string(), "foo".to_string())
        );
    }

    #[test]
    fn split_path_prefix_with_separator_splits() {
        assert_eq!(
            split_path_prefix("src/main"),
            ("src".to_string(), "main".to_string())
        );
    }

    #[test]
    fn split_path_prefix_trailing_separator_lists_dir() {
        // "src/" → list the whole "src" dir (last component empty).
        assert_eq!(
            split_path_prefix("src/"),
            ("src".to_string(), String::new())
        );
    }

    #[cfg(unix)]
    #[test]
    fn complete_path_completes_against_temp_dir() {
        // Use an absolute path prefix so the test does not depend on
        // the process CWD (which is global and shared across parallel
        // test threads).
        let tmp = std::env::temp_dir().join("kf_code_complete_path_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("tmpfile.txt"), "x").unwrap();
        std::fs::write(tmp.join("tmpfile2.txt"), "y").unwrap();
        std::fs::write(tmp.join("other.txt"), "z").unwrap();

        // Pass the absolute dir with a trailing separator so the
        // parent dir is `tmp` and the last component is "" (match all).
        // Use the OS-native separator so the path parses correctly on
        // Windows (split_path_prefix splits on both `/` and `\`).
        let prefix = tmp.join("tmp");
        let matches = complete_path(&prefix.display().to_string());
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(matches.contains(&"tmpfile.txt".to_string()));
        assert!(matches.contains(&"tmpfile2.txt".to_string()));
        assert!(
            !matches.iter().any(|m| m == "other.txt"),
            "should not match non-prefix entry: {matches:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn complete_path_strips_range_suffix_before_completing() {
        // `@foo.rs:10-20:raw` — only the path portion before the first
        // `:` is completed. The suffix must not corrupt the lookup.
        let tmp = std::env::temp_dir().join("kf_code_complete_path_suffix_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("foo.rs"), "x").unwrap();

        // Use OS-native path joining; append the `:10-20:raw` suffix
        // as a string (the `:` is the @-mention range delimiter, not a
        // path separator).
        let prefix = format!("{}:10-20:raw", tmp.join("foo.rs").display());
        let matches = complete_path(&prefix);
        let _ = std::fs::remove_dir_all(&tmp);

        // The path portion is "<tmp>/foo.rs" which exists; the suffix
        // is ignored. We expect the matching entry back.
        assert_eq!(matches, vec!["foo.rs".to_string()]);
    }

    #[tokio::test]
    async fn tab_completes_slash_command_single_match() {
        // "/he" + Tab → "/help" (single match replaces the buffer).
        let mut state = app_state();
        state.input = "/he".into();
        state.cursor_position = 3; // end of "/he"

        let ctx_holder = make_ctx();
        let ctx = ctx_holder.ctx();
        handle_input_key(KeyEvent::from(KeyCode::Tab), &mut state, &ctx)
            .await
            .unwrap();
        assert_eq!(state.input, "/help");
        assert_eq!(state.cursor_position, 5);
        assert!(state.completion_suggestions.is_empty());
    }

    #[tokio::test]
    async fn tab_completes_slash_command_multiple_matches_shows_suggestions() {
        // "/p" + Tab → multiple matches (e.g. /plan, /plugins). The
        // buffer is unchanged; suggestions are populated.
        let mut state = app_state();
        state.input = "/p".into();
        state.cursor_position = 2;

        let ctx_holder = make_ctx();
        let ctx = ctx_holder.ctx();
        handle_input_key(KeyEvent::from(KeyCode::Tab), &mut state, &ctx)
            .await
            .unwrap();
        // Buffer unchanged (multiple matches → no auto-replace).
        assert_eq!(state.input, "/p");
        assert!(
            state.completion_suggestions.len() >= 2,
            "expected >=2 suggestions, got {:?}",
            state.completion_suggestions
        );
        assert!(state
            .completion_suggestions
            .iter()
            .all(|t| t.starts_with("/p")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tab_on_at_mention_completes_path() {
        // Use an absolute @-mention so the test doesn't depend on the
        // global process CWD (shared across parallel test threads).
        let tmp = std::env::temp_dir().join("kf_code_tab_at_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("tmpfile.txt"), "x").unwrap();

        let mut state = app_state();
        // Type "@<tmp>/tmpfile" — the absolute path prefix. Use the
        // OS-native path so Windows backslashes parse correctly.
        let typed = format!(
            "@{}tmpfile",
            tmp.display().to_string() + std::path::MAIN_SEPARATOR_STR
        );
        state.input = typed.clone();
        state.cursor_position = typed.chars().count();

        let ctx_holder = make_ctx();
        let ctx = ctx_holder.ctx();
        handle_input_key(KeyEvent::from(KeyCode::Tab), &mut state, &ctx)
            .await
            .unwrap();

        let _ = std::fs::remove_dir_all(&tmp);

        let expected = format!(
            "@{}tmpfile.txt",
            tmp.display().to_string() + std::path::MAIN_SEPARATOR_STR
        );
        assert_eq!(state.input, expected);
        assert_eq!(state.cursor_position, state.input.chars().count());
        assert!(state.completion_suggestions.is_empty());
    }

    #[tokio::test]
    async fn tab_preserves_expand_collapse_on_empty_input() {
        // Tab on empty input must still toggle expand/collapse on the
        // last message (the legacy behavior — don't break it).
        let mut state = app_state();
        state.input.clear();
        state.cursor_position = 0;
        state
            .messages
            .push_back(crate::tui::app::ConversationEntry::new("assistant", "hi"));

        let ctx_holder = make_ctx();
        let ctx = ctx_holder.ctx();
        let last_idx = state.messages.len() - 1;
        assert!(!state.collapsed_messages.contains(&last_idx));
        handle_input_key(KeyEvent::from(KeyCode::Tab), &mut state, &ctx)
            .await
            .unwrap();
        assert!(
            state.collapsed_messages.contains(&last_idx),
            "Tab on empty input should collapse the last message"
        );
        assert!(state.completion_suggestions.is_empty());
    }

    #[tokio::test]
    async fn tab_no_match_on_unknown_slash_is_noop() {
        // "/zzz" + Tab → no matches, buffer unchanged, no suggestions.
        let mut state = app_state();
        state.input = "/zzz".into();
        state.cursor_position = 4;

        let ctx_holder = make_ctx();
        let ctx = ctx_holder.ctx();
        handle_input_key(KeyEvent::from(KeyCode::Tab), &mut state, &ctx)
            .await
            .unwrap();
        assert_eq!(state.input, "/zzz");
        assert!(state.completion_suggestions.is_empty());
    }

    #[tokio::test]
    async fn typing_clears_completion_suggestions() {
        // After Tab shows suggestions, pressing any non-Tab key clears
        // them so the hint doesn't linger.
        let mut state = app_state();
        state.input = "/p".into();
        state.cursor_position = 2;
        state.completion_suggestions = vec!["/plan".into(), "/plugins".into()];

        let ctx_holder = make_ctx();
        let ctx = ctx_holder.ctx();
        // Press 'x' — should clear suggestions and insert the char.
        handle_input_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut state,
            &ctx,
        )
        .await
        .unwrap();
        assert!(state.completion_suggestions.is_empty());
        assert_eq!(state.input, "/px");
    }

    fn make_ctx() -> TestCtx {
        TestCtx::new()
    }

    // Owns the channels so the `HandleInputContext` borrows stay valid
    // for the test body. Inlining 11 channel creations per test was
    // too noisy; this keeps the WO 14.6 tests readable.
    struct TestCtx {
        _input_rx: mpsc::UnboundedReceiver<String>,
        input_tx: mpsc::UnboundedSender<String>,
        _cancel_rx: mpsc::UnboundedReceiver<()>,
        cancel_tx: mpsc::UnboundedSender<()>,
        _resume_rx: mpsc::UnboundedReceiver<ConversationLog>,
        resume_tx: mpsc::UnboundedSender<ConversationLog>,
        _compact_rx: mpsc::UnboundedReceiver<CompactRequest>,
        compact_tx: mpsc::UnboundedSender<CompactRequest>,
        _model_rx: mpsc::UnboundedReceiver<String>,
        model_tx: mpsc::UnboundedSender<String>,
        _undo_rx: mpsc::UnboundedReceiver<()>,
        undo_tx: mpsc::UnboundedSender<()>,
        _config_rx: mpsc::UnboundedReceiver<Config>,
        config_tx: mpsc::UnboundedSender<Config>,
        _plan_rx: mpsc::UnboundedReceiver<bool>,
        plan_tx: mpsc::UnboundedSender<bool>,
        _persona_rx: mpsc::UnboundedReceiver<PersonaResult>,
        persona_tx: mpsc::UnboundedSender<PersonaResult>,
        _event_rx: mpsc::Receiver<TurnEvent>,
        event_tx: mpsc::Sender<TurnEvent>,
        _plugin_reload_rx: mpsc::UnboundedReceiver<kf_plugin_host::PluginRegistry>,
        plugin_reload_tx: mpsc::UnboundedSender<kf_plugin_host::PluginRegistry>,
    }

    impl TestCtx {
        fn new() -> Self {
            let (input_tx, _input_rx) = mpsc::unbounded_channel();
            let (cancel_tx, _cancel_rx) = mpsc::unbounded_channel();
            let (resume_tx, _resume_rx) = mpsc::unbounded_channel();
            let (compact_tx, _compact_rx) = mpsc::unbounded_channel();
            let (model_tx, _model_rx) = mpsc::unbounded_channel();
            let (undo_tx, _undo_rx) = mpsc::unbounded_channel();
            let (config_tx, _config_rx) = mpsc::unbounded_channel();
            let (plan_tx, _plan_rx) = mpsc::unbounded_channel();
            let (persona_tx, _persona_rx) = mpsc::unbounded_channel();
            let (event_tx, _event_rx) = mpsc::channel(10_000);
            let (plugin_reload_tx, _plugin_reload_rx) = mpsc::unbounded_channel();
            Self {
                _input_rx,
                input_tx,
                _cancel_rx,
                cancel_tx,
                _resume_rx,
                resume_tx,
                _compact_rx,
                compact_tx,
                _model_rx,
                model_tx,
                _undo_rx,
                undo_tx,
                _config_rx,
                config_tx,
                _plan_rx,
                plan_tx,
                _persona_rx,
                persona_tx,
                _event_rx,
                event_tx,
                _plugin_reload_rx,
                plugin_reload_tx,
            }
        }

        fn ctx(&self) -> HandleInputContext<'_> {
            HandleInputContext {
                input_tx: &self.input_tx,
                cancel_tx: &self.cancel_tx,
                resume_tx: &self.resume_tx,
                compact_tx: &self.compact_tx,
                model_tx: &self.model_tx,
                undo_tx: &self.undo_tx,
                config_tx: &self.config_tx,
                plan_tx: &self.plan_tx,
                persona_tx: &self.persona_tx,
                event_tx: &self.event_tx,
                plugin_reload_tx: &self.plugin_reload_tx,
            }
        }
    }
}
