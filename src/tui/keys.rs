//! Input-mode keyboard handler.
//!
//! This is the regular (non-approval) key handling path. It lives in its own
//! module so `tui/mod.rs` can stay focused on the event-loop orchestration.
//!
//! Function signature is the same as the inline version it was extracted from:
//! `async fn handle_input_key(key, state, input_tx, cancel_tx, resume_tx, compact_tx) -> anyhow::Result<()>`.
//! The orchestrator calls us only when `state.pending_approval.is_none()`.

use crate::session::conversation::ConversationLog;
use crate::tui::app::{AppState, ConversationEntry};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

/// Split a `!` command's formatted output into a one-line summary and the
/// full output, for use with `ConversationEntry::tool(summary, full)`.
///
/// The summary is the first two lines of the formatted output (the
/// `$ <cmd>` header and the `✅/❌/⏰` banner) — enough for the user to
/// see what they ran and whether it worked without expanding. The full
/// output is the entire formatted string. This mirrors the
/// `tool_should_collapse` / `expanded_tools` pattern: the chat panel
/// shows only the summary by default; Enter or Tab on empty input
/// expands it.
///
/// Pin: the splitter has to be `pub(crate)` for unit tests if we add
/// any, but for now it's file-private and exercised end-to-end via
/// the `!` passthrough integration tests in `commands.rs`.
fn split_bang_summary(formatted: &str) -> (String, String) {
    let mut lines = formatted.splitn(3, '\n');
    let first = lines.next().unwrap_or("");
    let second = lines.next().unwrap_or("");
    let rest = lines.next().unwrap_or("");

    let summary = if rest.is_empty() {
        // Two-line output (no stdout/stderr) — just show both lines.
        format!("{}\n{}", first, second)
    } else {
        // Multi-line output — summary is the first two lines, full
        // output is everything.
        format!("{}\n{}", first, second)
    };

    (summary, formatted.to_string())
}

/// Handle a single key event in the regular input mode.
///
/// Returns `Ok(())` after a single event. Only errors on I/O failure
/// (e.g. terminal draw failure bubbling up from the event loop — in
/// practice this function itself does no I/O, so `Ok(())` is the only
/// realistic outcome, but we keep `Result` for symmetry with the caller).
#[allow(clippy::too_many_arguments)]
pub async fn handle_input_key(
    key: KeyEvent,
    state: &mut AppState,
    input_tx: &mpsc::UnboundedSender<String>,
    cancel_tx: &mpsc::UnboundedSender<()>,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
    compact_tx: &mpsc::UnboundedSender<()>,
) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match c {
                    'c' => {
                        // Ctrl+C: cancel in-flight generation (if any),
                        // then clear the input buffer.
                        if state.is_generating {
                            if cancel_tx.send(()).is_err() {
                                // The executor driver is gone — the
                                // session is ending or the TUI is
                                // shutting down. No need to keep
                                // pretending a cancel is in flight.
                                // Don't warn-and-continue; the user
                                // pressing Ctrl+C is itself the
                                // shutdown signal, so just suppress
                                // state mutation.
                                tracing::debug!(
                                    "cancel_tx receiver dropped on Ctrl+C; executor already gone"
                                );
                                return Ok(());
                            }
                            state.is_generating = false;
                        }
                        state.input.clear();
                        state.cursor_position = 0;
                    }
                    'w' => {
                        // Ctrl+W: delete word backward using char-index cursor
                        let cur_byte = state.cursor_byte();
                        let before = &state.input[..cur_byte];
                        if let Some(pos) = before.rfind(|c: char| c.is_whitespace()) {
                            // pos is a byte offset — count chars before it to get new cursor position
                            let trimmed = before[..pos].trim_end_matches(' ');
                            let new_byte = trimmed.len();
                            let new_cursor = trimmed.chars().count();
                            state.input.drain(new_byte..cur_byte);
                            state.cursor_position = new_cursor;
                        } else {
                            // Delete from start
                            state.input.drain(..cur_byte);
                            state.cursor_position = 0;
                        }
                    }
                    'u' => {
                        // Ctrl+U: clear line
                        state.input.clear();
                        state.cursor_position = 0;
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
            // Tab on an empty input toggles expand on the most recent
            // tool entry. Empty input means the user isn't typing
            // anything — Tab is otherwise useless in a single-line input,
            // so it's a free gesture.
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
                        return Ok(());
                    }
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
            if state.cursor_position > 0 {
                state.cursor_position -= 1;
            }
        }
        KeyCode::Right => {
            let char_count = state.input.chars().count();
            if state.cursor_position < char_count {
                state.cursor_position += 1;
            }
        }
        KeyCode::Home => {
            state.cursor_position = 0;
        }
        KeyCode::End => {
            state.cursor_position = state.input.chars().count();
        }
        KeyCode::Enter => {
            // v1.2-p14 — `!` bash passthrough. A line beginning with `!`
            // (and at least one non-`!` char after it) runs directly via
            // /bin/sh with no model round trip and no approval gate. The
            // returned string is rendered as a tool entry so the existing
            // collapse/expand UX in `chat.rs` applies — a 500-line `!find`
            // doesn't flood the chat.
            if let Some(rest) = state.input.strip_prefix('!') {
                let rest = rest.to_string();
                state.input.clear();
                state.cursor_position = 0;
                let out = crate::tui::commands::handle_bang_command(&rest).await;
                // Split into summary (first line) and full output so the
                // collapse UX has something to show by default. The
                // summary is "$ <cmd>\n<icon> exit <code>" — two lines.
                // Full output is everything.
                let (summary, full) = split_bang_summary(&out);
                state
                    .messages
                    .push(crate::tui::app::ConversationEntry::tool(summary, full));
                return Ok(());
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
                        return Ok(());
                    }
                }
            }

            let input = state.input.clone();
            state.input.clear();
            state.cursor_position = 0;

            if !input.is_empty() {
                if input.starts_with('/') {
                    // Command — dispatch via skill registry or built-in
                    let parts: Vec<&str> = input.splitn(2, ' ').collect();
                    let cmd = parts[0];
                    let args = parts.get(1).copied().unwrap_or("");

                    // Built-in commands that don't go through skills
                    match cmd {
                        "/clear" => {
                            state.messages.clear();
                            state.thinking_buffer.clear();
                            return Ok(());
                        }
                        "/exit" | "/quit" => {
                            state.should_exit = true;
                            return Ok(());
                        }
                        "/help" | "/h" | "/?" => {
                            let mut help_text =
                                "Built-in commands:\n  /clear    Clear conversation\n  /exit     Quit\n  /fork     Fork session: /fork list | <label> [count]\n  /resume   Resume a fork: /resume <fork-id>\n  /jobs     Background bash jobs: /jobs | <id> | clean\n  /status   Show model, cost, tokens, and context pressure (one-shot)\n  /compact  Compact conversation history: drop old tool results, condense old assistant turns. Destructive — see TUI for stats.\n\nBash passthrough:\n  !<command>  Run a shell command directly — no model round trip, no approval. Output is shown as a collapsible tool entry. 30-second timeout; for long jobs use `!<cmd> &` and check /jobs.\n\n@-mentions (inline file context):\n  @<path>          Inline the file's contents into the prompt (minified by default). The TUI shows a status row per mention.\n  @<path>:raw      Inline the file verbatim, no minification.\n  @<path>:A-B      Inline lines A–B (1-indexed, inclusive on both ends).\n  @<path>:A-B:raw  Range + verbatim, combined.\n  @~/...           Tilde expansion supported (e.g. @~/notes.md).\n  Multiple @<path> tokens in one input are all expanded. Each mention is capped at 50 KB (head + tail + marker) and respects the same path-safety rules as the model's read_file tool. Failures (missing, denied, I/O) are shown in the TUI as ✗ rows and as quoted placeholders in the prompt, so the model can react.\n\nKeybindings:\n  Ctrl+T   Toggle tool output collapse (default ON)\n  Enter    Expand/collapse the most recent tool output (when input is empty)\n  Tab      Same as Enter (alternative expand gesture)\n  Ctrl+C   Cancel generation + clear input\n  Ctrl+W   Delete word backward\n  Ctrl+U   Clear input line\n  Esc      Toggle thinking panel\n\nStatus bar:\n  The bottom bar shows session model, time, cumulative cost, and a colour-coded budget indicator. Green (< 50%) = comfortable, yellow (50–80%) = consider /compact, red (> 80%) = compact now. The same data is available on demand via /status.\n".to_string();
                            let skills = state.skill_registry.all();
                            if !skills.is_empty() {
                                help_text.push_str("\nSkills:\n");
                                for skill in skills {
                                    help_text.push_str(&format!(
                                        "  {}  — {}{}\n",
                                        skill.meta.trigger,
                                        skill.meta.description,
                                        skill
                                            .meta
                                            .model
                                            .as_ref()
                                            .map(|m| format!(" [{}]", m))
                                            .unwrap_or_default(),
                                    ));
                                }
                            }
                            state
                                .messages
                                .push(ConversationEntry::new("system", help_text));
                            return Ok(());
                        }
                        "/fork" => {
                            let msg = crate::tui::commands::handle_fork_command(args, state).await;
                            state.messages.push(ConversationEntry::new("system", msg));
                            return Ok(());
                        }
                        "/resume" => {
                            let msg =
                                crate::tui::commands::handle_resume_command(args, state, resume_tx)
                                    .await;
                            state.messages.push(ConversationEntry::new("system", msg));
                            return Ok(());
                        }
                        "/jobs" => {
                            let msg = crate::tui::commands::handle_jobs_command(args).await;
                            state.messages.push(ConversationEntry::new("system", msg));
                            return Ok(());
                        }
                        "/status" => {
                            let msg =
                                crate::tui::commands::handle_status_command(args, state).await;
                            state.messages.push(ConversationEntry::new("system", msg));
                            return Ok(());
                        }
                        "/compact" => {
                            let msg =
                                crate::tui::commands::handle_compact_command(args, compact_tx)
                                    .await;
                            state.messages.push(ConversationEntry::new("system", msg));
                            return Ok(());
                        }
                        "/memory" => {
                            let msg = crate::tui::commands::handle_memory_command(args);
                            state.messages.push(ConversationEntry::new("system", msg));
                            return Ok(());
                        }
                        "/plan" => {
                            let (display, plan_prompt) =
                                crate::tui::commands::handle_plan_command(args);
                            state
                                .messages
                                .push(ConversationEntry::new("system", display));
                            if !plan_prompt.is_empty() {
                                state.is_generating = true;
                                if input_tx.send(plan_prompt).is_err() {
                                    tracing::warn!(
                                        "input_tx receiver dropped while dispatching plan prompt"
                                    );
                                    state.is_generating = false;
                                    return Ok(());
                                }
                            }
                            return Ok(());
                        }
                        "/gh" => {
                            let msg = crate::tui::commands::handle_gh_command(args);
                            state.messages.push(ConversationEntry::new("system", msg));
                            return Ok(());
                        }
                        "/init" => {
                            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                            let msg = crate::tui::commands::handle_init_command(args, &cwd);
                            state.messages.push(ConversationEntry::new("system", msg));
                            return Ok(());
                        }
                        _ => {}
                    }

                    if let Some(skill) = state.skill_registry.get_by_trigger(cmd) {
                        let rendered = skill.render_prompt(args);
                        state.messages.push(ConversationEntry::new(
                            "system",
                            format!(
                                "🔧 Running skill: {} — {}",
                                skill.meta.name, skill.meta.description
                            ),
                        ));
                        // Send the skill prompt to the model via executor
                        state.is_generating = true;
                        if input_tx.send(rendered).is_err() {
                            // Executor driver gone — same situation
                            // as the Ctrl+C branch above. Don't
                            // leave the UI claiming it's
                            // "generating" when no one is listening.
                            tracing::warn!(skill = %skill.meta.name, "input_tx receiver dropped while dispatching skill prompt");
                            state.is_generating = false;
                            return Ok(());
                        }
                    } else {
                        state.messages.push(ConversationEntry::new(
                            "system",
                            format!(
                                "Unknown command: {}\nType /help for available commands.",
                                cmd
                            ),
                        ));
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
                        .push(ConversationEntry::new("user", cleaned.clone()));
                    if !status_msg.is_empty() {
                        state
                            .messages
                            .push(ConversationEntry::new("system", status_msg));
                    }
                    state.is_generating = true;
                    let prompt = if rendered_block.is_empty() {
                        cleaned
                    } else {
                        format!("{}{}", cleaned, rendered_block)
                    };
                    if input_tx.send(prompt).is_err() {
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
            // Scroll up (see older content)
            state.auto_scroll = false;
            state.scroll_offset = state.scroll_offset.saturating_sub(1);
        }
        KeyCode::Down => {
            // Scroll down (see newer content)
            // Clamp to max_scroll so the view doesn't run off the bottom
            // waiting for the next render to correct it.
            state.scroll_offset = (state.scroll_offset + 1).min(state.max_scroll);
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
