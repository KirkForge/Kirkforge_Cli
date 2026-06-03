pub mod app;
pub mod components;
pub mod rendering;
pub mod widgets;

use app::{AppState, ConnectionState, ConversationEntry, PendingApproval};
use components::approval::render_approval_dialog;
use crate::session::executor::{self, ApprovalRequest, ApprovalResponse};
use crate::session::conversation::ConversationLog;
use crate::shared::Config;
use crate::tools::Tool;
use widgets::chat::render_chat;
use widgets::input::render_input;
use widgets::status::render_status;

use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Run the TUI event loop.
pub async fn run_tui(
    config: Config,
    adapter: Box<dyn crate::adapters::ModelAdapter>,
    tools: Vec<Arc<dyn Tool>>,
    conversation_log: ConversationLog,
) -> anyhow::Result<()> {
    // Initialize terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Application state
    let model_info = adapter.model_info();
    let mut state = AppState::new(config.clone());
    state.model_info = Some(model_info.clone());
    state.connection = ConnectionState::Connected {
        model: model_info.name.clone(),
        since: Instant::now(),
    };

    // Initialize skill registry: scan for SKILL.md files
    state.skill_registry.add_scan_path(std::path::PathBuf::from(".claude/skills"));
    match state.skill_registry.scan_and_load() {
        Ok(count) => {
            if count > 0 {
                tracing::info!("Loaded {} skills from .claude/skills", count);
            }
        }
        Err(e) => {
            tracing::warn!("Skill scan error: {}", e);
        }
    }
    // Always register built-in skills
    for skill in crate::session::skills::builtin_skills() {
        state.skill_registry.register(skill);
    }

    // ── Channels ──
    // User input: TUI → Executor
    let (input_tx, input_rx) = mpsc::unbounded_channel::<String>();
    // Stream events: Executor → TUI
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<executor::TurnEvent>();
    // Approval requests: Executor → TUI
    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

    // Spawn the executor on a background task
    let mut exe = executor::Executor::with_log(adapter, tools, config, conversation_log);
    tokio::spawn(async move {
        let _ = exe.run(input_rx, event_tx, approval_tx).await;
    });

    // Event loop
    let res = run_event_loop(
        &mut terminal,
        &mut state,
        &mut event_rx,
        &mut approval_rx,
        &input_tx,
    ).await;

    // Cleanup
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);

    res
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    event_rx: &mut mpsc::UnboundedReceiver<executor::TurnEvent>,
    approval_rx: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
    input_tx: &mpsc::UnboundedSender<String>,
) -> anyhow::Result<()> {
    loop {
        // ── Drain pending stream events ──
        while let Ok(ev) = event_rx.try_recv() {
            match ev {
                executor::TurnEvent::Token(t) => {
                    // Accumulate into the last assistant entry, or create one
                    let role_str = "assistant".to_string();
                    if let Some(last) = state.messages.last_mut() {
                        if last.role == role_str {
                            last.content.push_str(&t);
                        } else {
                            state.messages.push(ConversationEntry {
                                role: role_str,
                                content: t,
                                timestamp: chrono::Local::now(),
                            });
                        }
                    } else {
                        state.messages.push(ConversationEntry {
                            role: role_str,
                            content: t,
                            timestamp: chrono::Local::now(),
                        });
                    }
                }
                executor::TurnEvent::Thinking(t) => {
                    state.thinking_buffer.push(t);
                }
                executor::TurnEvent::ToolStart { name, args: _ } => {
                    state.messages.push(ConversationEntry {
                        role: "tool".into(),
                        content: format!("🔧 {} ...", name),
                        timestamp: chrono::Local::now(),
                    });
                }
                executor::TurnEvent::ToolResult { name, output } => {
                    // Update the last tool message with its output
                    let label = format!("🔧 {} (done)", name);
                    if let Some(last) = state.messages.last_mut() {
                        if last.role == "tool" && last.content.starts_with("🔧") {
                            last.content = format!("{}\n{}", label, output);
                        } else {
                            state.messages.push(ConversationEntry {
                                role: "tool".into(),
                                content: format!("{}\n{}", label, output),
                                timestamp: chrono::Local::now(),
                            });
                        }
                    } else {
                        state.messages.push(ConversationEntry {
                            role: "tool".into(),
                            content: format!("{}\n{}", label, output),
                            timestamp: chrono::Local::now(),
                        });
                    }
                }
                executor::TurnEvent::Verification { message, success } => {
                    let prefix = if success { "🔍" } else { "⚠️" };
                    state.messages.push(ConversationEntry {
                        role: "system".into(),
                        content: format!("{} {}", prefix, message),
                        timestamp: chrono::Local::now(),
                    });
                }
                executor::TurnEvent::Error(e) => {
                    state.messages.push(ConversationEntry {
                        role: "system".into(),
                        content: format!("Error: {}", e),
                        timestamp: chrono::Local::now(),
                    });
                }
                executor::TurnEvent::CostStats { prompt_tokens, completion_tokens, turn_cost, cumulative_cost } => {
                    state.tokens_sent = state.tokens_sent.wrapping_add(prompt_tokens);
                    state.tokens_received = state.tokens_received.wrapping_add(completion_tokens);
                    state.turn_cost = turn_cost;
                    state.cumulative_cost = cumulative_cost;
                }
            }
        }

        // ── Drain pending approval requests ──
        while let Ok(req) = approval_rx.try_recv() {
            state.pending_approval = Some(PendingApproval {
                tool_name: req.tool_name.clone(),
                args: req.args.clone(),
                responder: Some(req.response),
            });
        }

        // ── Render ──
        terminal.draw(|f| {
            let size = f.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(size);

            render_chat(f, chunks[0], state);
            render_input(f, chunks[1], state);
            render_status(f, chunks[2], state);

            // Approval dialog overlay
            if let Some(ref approval) = state.pending_approval {
                render_approval_dialog(f, size, approval);
            }
        })?;

        // ── Handle events ──
        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if state.pending_approval.is_some() {
                        handle_approval_key(key, state);
                    } else {
                        handle_input_key(key, state, input_tx)?;
                    }
                }
                Event::Resize(_w, _h) => {}
                _ => {}
            }
        }
    }
}

fn handle_input_key(
    key: KeyEvent,
    state: &mut AppState,
    input_tx: &mpsc::UnboundedSender<String>,
) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match c {
                    'c' => {
                        // Ctrl+C: send empty string to cancel
                        state.input.clear();
                        state.cursor_position = 0;
                    }
                    'w' => {
                        // Ctrl+W: delete word backward
                        let before = &state.input[..state.cursor_position];
                        if let Some(pos) = before.rfind(|c: char| c.is_whitespace()) {
                            let trimmed = before[..pos].trim_end_matches(' ');
                            let new_pos = trimmed.len();
                            state.input.drain(new_pos..state.cursor_position);
                            state.cursor_position = new_pos;
                        } else {
                            state.input.drain(..state.cursor_position);
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
                    _ => {}
                }
            } else {
                state.input.insert(state.cursor_position, c);
                state.cursor_position += 1;
            }
        }
        KeyCode::Backspace => {
            if state.cursor_position > 0 {
                let pos = state.cursor_position - 1;
                state.input.remove(pos);
                state.cursor_position = pos;
            }
        }
        KeyCode::Delete => {
            if state.cursor_position < state.input.len() {
                state.input.remove(state.cursor_position);
            }
        }
        KeyCode::Left => {
            if state.cursor_position > 0 {
                state.cursor_position -= 1;
            }
        }
        KeyCode::Right => {
            if state.cursor_position < state.input.len() {
                state.cursor_position += 1;
            }
        }
        KeyCode::Home => {
            state.cursor_position = 0;
        }
        KeyCode::End => {
            state.cursor_position = state.input.len();
        }
        KeyCode::Enter => {
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
                            std::process::exit(0);
                        }
                        "/help" | "/h" | "/?" => {
                            let mut help_text =
                                "Built-in commands:\n  /clear  Clear conversation\n  /exit   Quit\n".to_string();
                            let skills = state.skill_registry.all();
                            if !skills.is_empty() {
                                help_text.push_str("\nSkills:\n");
                                for skill in skills {
                                    help_text.push_str(&format!(
                                        "  {}  — {}{}\n",
                                        skill.meta.trigger,
                                        skill.meta.description,
                                        skill.meta.model.as_ref()
                                            .map(|m| format!(" [{}]", m))
                                            .unwrap_or_default(),
                                    ));
                                }
                            }
                            state.messages.push(ConversationEntry {
                                role: "system".into(),
                                content: help_text,
                                timestamp: chrono::Local::now(),
                            });
                            return Ok(());
                        }
                        _ => {}
                    }

                    if let Some(skill) = state.skill_registry.get_by_trigger(cmd) {
                        let rendered = skill.render_prompt(args);
                        state.messages.push(ConversationEntry {
                            role: "system".into(),
                            content: format!("🔧 Running skill: {} — {}", skill.meta.name, skill.meta.description),
                            timestamp: chrono::Local::now(),
                        });
                        // Send the skill prompt to the model via executor
                        let _ = input_tx.send(rendered);
                    } else {
                        state.messages.push(ConversationEntry {
                            role: "system".into(),
                            content: format!("Unknown command: {}\nType /help for available commands.", cmd),
                            timestamp: chrono::Local::now(),
                        });
                    }
                } else {
                    // Regular message — push to display and send to executor
                    state.messages.push(ConversationEntry {
                        role: "user".into(),
                        content: input.clone(),
                        timestamp: chrono::Local::now(),
                    });
                    let _ = input_tx.send(input);
                }
            }
        }
        KeyCode::Esc => {
            // Toggle thinking panel
            state.thinking_panel_visible = !state.thinking_panel_visible;
        }
        KeyCode::Up => {
            // Scroll up
            state.scroll_offset = state.scroll_offset.saturating_add(1);
        }
        KeyCode::Down => {
            state.scroll_offset = state.scroll_offset.saturating_sub(1);
        }
        KeyCode::PageUp => {
            state.scroll_offset = state.scroll_offset.saturating_add(10);
        }
        KeyCode::PageDown => {
            state.scroll_offset = state.scroll_offset.saturating_sub(10);
        }
        _ => {}
    }

    Ok(())
}

fn handle_approval_key(key: KeyEvent, state: &mut AppState) {
    let approval = match state.pending_approval.take() {
        Some(a) => a,
        None => return,
    };

    let response = match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(ApprovalResponse::Approved),
        KeyCode::Char('n') | KeyCode::Char('N') => Some(ApprovalResponse::Denied),
        KeyCode::Char('a') | KeyCode::Char('A') => Some(ApprovalResponse::AlwaysApprove),
        KeyCode::Esc => Some(ApprovalResponse::Denied),
        _ => {
            state.pending_approval = Some(approval);
            return;
        }
    };

    if let Some(resp) = response {
        if matches!(resp, ApprovalResponse::AlwaysApprove) {
            state.config.auto_approve = true;
            let _ = crate::session::config::save_config(&state.config);
        }
        if let Some(tx) = approval.responder {
            let _ = tx.send(resp);
        }
    }
}

