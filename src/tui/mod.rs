pub mod app;
pub mod components;
pub mod rendering;
pub mod widgets;

use app::{AppState, ConnectionState, PendingApproval};
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

    // Channels for approval flow
    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

    // Session — executor owns the conversation log
    let mut executor = executor::Executor::with_log(adapter, tools, config, conversation_log);

    // Event loop
    let res = _run_event_loop(
        &mut terminal,
        &mut state,
        &mut approval_rx,
        &mut executor,
    ).await;

    // Cleanup
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);

    res
}

async fn _run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    approval_rx: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
    _executor: &mut executor::Executor,
) -> anyhow::Result<()> {
    loop {
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
                        handle_input_key(key, state)?;
                    }
                }
                Event::Resize(_w, _h) => {}
                _ => {}
            }
        }

        // ── Check for approval requests ──
        if let Ok(_req) = approval_rx.try_recv() {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            state.pending_approval = Some(PendingApproval {
                tool_name: _req.tool_name.clone(),
                args: _req.args.clone(),
                responder: Some(tx),
            });
        }
    }
}

fn handle_input_key(key: KeyEvent, state: &mut AppState) -> anyhow::Result<()> {
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
                // Add to conversation display
                state.messages.push(crate::tui::app::ConversationEntry {
                    role: "user".into(),
                    content: input.clone(),
                    timestamp: chrono::Local::now(),
                });

                // Process the input as a command or message
                process_input(input, state);
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
        if let Some(tx) = approval.responder {
            let _ = tx.send(resp);
        }
        if matches!(resp, ApprovalResponse::AlwaysApprove) {
            // TODO: set auto_approve in config
        }
    }
}

fn process_input(input: String, state: &mut AppState) {
    if input.starts_with('/') {
        // Command handling
        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        match parts[0] {
            "/help" | "/h" => {
                state.messages.push(crate::tui::app::ConversationEntry {
                    role: "system".into(),
                    content: "Commands: /help, /connect <model>, /clear, /model, /exit".into(),
                    timestamp: chrono::Local::now(),
                });
            }
            "/clear" => {
                state.messages.clear();
                state.thinking_buffer.clear();
            }
            "/model" => {
                let info = state.model_info.as_ref().map(|m| m.name.as_str()).unwrap_or("unknown");
                state.messages.push(crate::tui::app::ConversationEntry {
                    role: "system".into(),
                    content: format!("Current model: {}", info),
                    timestamp: chrono::Local::now(),
                });
            }
            "/exit" | "/quit" => {
                // TODO: graceful shutdown
                std::process::exit(0);
            }
            other => {
                state.messages.push(crate::tui::app::ConversationEntry {
                    role: "system".into(),
                    content: format!("Unknown command: {}\nType /help for available commands.", other),
                    timestamp: chrono::Local::now(),
                });
            }
        }
    } else {
        // Regular message — add as user message
        state.messages.push(crate::tui::app::ConversationEntry {
            role: "user".into(),
            content: input,
            timestamp: chrono::Local::now(),
        });
    }
}