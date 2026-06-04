pub mod app;
pub mod components;
pub mod rendering;
pub mod widgets;

use crate::session::carryover::CarryoverProfile;
use crate::session::conversation::ConversationLog;
use crate::session::executor::{self, ApprovalRequest, ApprovalResponse};
use crate::shared::Config;
use crate::tools::Tool;
use app::{AppState, ConnectionState, ConversationEntry, PendingApproval};
use components::approval::render_approval_dialog;
use widgets::chat::render_chat;
use widgets::input::render_input;
use widgets::status::render_status;

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;

/// Panic-safe guard that restores terminal state on drop.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

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

    // Panic guard: restores terminal even if we unwind
    let _guard = TerminalGuard;

    // Application state
    let model_info = adapter.model_info();
    let mut state = AppState::new(config.clone());
    state.model_info = Some(model_info.clone());
    state.connection = ConnectionState::Connected {
        model: model_info.name.clone(),
        since: Instant::now(),
    };

    // Set up session forking metadata
    let log_path = conversation_log.path().clone();
    let session_id = crate::session::new_session_id().to_string();
    state.log_path = Some(log_path.clone());
    state.session_id = session_id.clone();
    state.fork_manager = Some(crate::session::session_fork::ForkManager::new(
        &session_id,
        &log_path,
    ));

    // Initialize skill registry: scan for SKILL.md files
    state
        .skill_registry
        .add_scan_path(std::path::PathBuf::from(".claude/skills"));
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

    // ── Carryover profile (shared between executor and save) ──
    let carryover_target: Option<Arc<Mutex<CarryoverProfile>>> = if config.carryover_enabled {
        Some(Arc::new(Mutex::new(CarryoverProfile::default())))
    } else {
        None
    };
    let saved_profile = carryover_target.clone();

    // ── Channels ──
    // User input: TUI → Executor
    let (input_tx, input_rx) = mpsc::unbounded_channel::<String>();
    // Stream events: Executor → TUI
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<executor::TurnEvent>();
    // Approval requests: Executor → TUI
    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
    // Cancellation: TUI → Executor (sends () to cancel current turn)
    let (cancel_tx, cancel_rx) = mpsc::unbounded_channel::<()>();
    // Resume: TUI → Executor (sends a ConversationLog to swap in for fork resumption)
    let (resume_tx, resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
    // Keyboard events: background reader thread → TUI event loop
    let (kb_tx, mut kb_rx) = mpsc::unbounded_channel::<Event>();

    // Spawn a dedicated thread to read crossterm events without blocking
    // the async event loop. This eliminates the 50ms poll latency floor.
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if kb_tx.send(ev).is_err() {
                break; // receiver dropped (TUI exited)
            }
        }
    });

    // Spawn the executor on a background task
    let mut exe =
        executor::Executor::with_log(adapter, tools, config, conversation_log, carryover_target);
    let handle = tokio::spawn(async move {
        let _ = exe.run(input_rx, event_tx, approval_tx, cancel_rx, resume_rx).await;
    });

    // Event loop
    let res = run_event_loop(
        &mut terminal,
        &mut state,
        &mut event_rx,
        &mut approval_rx,
        &mut kb_rx,
        &input_tx,
        &cancel_tx,
        &resume_tx,
    )
    .await;

    // Drop input sender to close the executor's recv loop, then wait for flush
    drop(input_tx);
    let _ = handle.await;

    // Save carryover profile
    if let Some(ref target) = saved_profile {
        if let Ok(guard) = target.lock() {
            crate::session::carryover::save_carryover(&guard);
        }
    }

    // Cleanup
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);

    res
}

#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    event_rx: &mut mpsc::UnboundedReceiver<executor::TurnEvent>,
    approval_rx: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
    kb_rx: &mut mpsc::UnboundedReceiver<Event>,
    input_tx: &mpsc::UnboundedSender<String>,
    cancel_tx: &mpsc::UnboundedSender<()>,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
) -> anyhow::Result<()> {
    loop {
        // Check for exit signal
        if state.should_exit {
            break Ok(());
        }

        // ── Drain pending stream events ──
        while let Ok(ev) = event_rx.try_recv() {
            match ev {
                executor::TurnEvent::Token(t) => {
                    state.is_generating = true; // got first token — turn off spinner
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
                    state.is_generating = false; // turn ended (tool call)
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
                    state.is_generating = false;
                    state.messages.push(ConversationEntry {
                        role: "system".into(),
                        content: format!("Error: {}", e),
                        timestamp: chrono::Local::now(),
                    });
                }
                executor::TurnEvent::CostStats {
                    prompt_tokens,
                    completion_tokens,
                    turn_cost,
                    cumulative_cost,
                } => {
                    state.is_generating = false;
                    state.tokens_sent = state.tokens_sent.wrapping_add(prompt_tokens);
                    state.tokens_received = state.tokens_received.wrapping_add(completion_tokens);
                    state.turn_cost = turn_cost;
                    state.cumulative_cost = cumulative_cost;
                }
            }
        }

        // ── Check for completed background jobs ──
        notify_completed_jobs(state).await;

        // ── Drain pending approval requests ──
        // If a new approval arrives while one is pending, deny the old one
        // before replacing it — otherwise the old oneshot sender is dropped
        // without sending, causing the executor to hang forever.
        while let Ok(req) = approval_rx.try_recv() {
            // Deny any existing pending approval first
            if let Some(old) = state.pending_approval.take() {
                if let Some(tx) = old.responder {
                    let _ = tx.send(ApprovalResponse::Denied);
                }
            }
            state.pending_approval = Some(PendingApproval {
                tool_name: req.tool_name.clone(),
                args: req.args.clone(),
                responder: Some(req.response),
            });
        }

        // ── Render ──
        // Tick spinner for loading animation
        state.spinner_tick = state.spinner_tick.wrapping_add(1);
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

        // ── Handle keyboard events (non-blocking, from background thread) ──
        while let Ok(ev) = kb_rx.try_recv() {
            match ev {
                Event::Key(key) => {
                    if state.pending_approval.is_some() {
                        handle_approval_key(key, state);
                    } else {
                        handle_input_key(key, state, input_tx, cancel_tx, resume_tx).await?;
                    }
                }
                Event::Resize(_w, _h) => {}
                _ => {}
            }
        }
    }
}

async fn handle_input_key(
    key: KeyEvent,
    state: &mut AppState,
    input_tx: &mpsc::UnboundedSender<String>,
    cancel_tx: &mpsc::UnboundedSender<()>,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match c {
                    'c' => {
                        // Ctrl+C: cancel in-flight generation (if any),
                        // then clear the input buffer.
                        if state.is_generating {
                            let _ = cancel_tx.send(());
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
                    _ => {}
                }
            } else {
                let byte_pos = state.cursor_byte();
                state.input.insert(byte_pos, c);
                state.cursor_position += 1;
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
                                "Built-in commands:\n  /clear   Clear conversation\n  /exit    Quit\n  /fork    Fork session: /fork list | <label> [count]\n  /resume  Resume a fork: /resume <fork-id>\n  /jobs    List background bash jobs\n".to_string();
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
                            state.messages.push(ConversationEntry {
                                role: "system".into(),
                                content: help_text,
                                timestamp: chrono::Local::now(),
                            });
                            return Ok(());
                        }
                        "/fork" => {
                            let msg = handle_fork_command(args, state).await;
                            state.messages.push(ConversationEntry {
                                role: "system".into(),
                                content: msg,
                                timestamp: chrono::Local::now(),
                            });
                            return Ok(());
                        }
                        "/resume" => {
                            let msg = handle_resume_command(args, state, resume_tx).await;
                            state.messages.push(ConversationEntry {
                                role: "system".into(),
                                content: msg,
                                timestamp: chrono::Local::now(),
                            });
                            return Ok(());
                        }
                        "/jobs" => {
                            let msg = handle_jobs_command().await;
                            state.messages.push(ConversationEntry {
                                role: "system".into(),
                                content: msg,
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
                            content: format!(
                                "🔧 Running skill: {} — {}",
                                skill.meta.name, skill.meta.description
                            ),
                            timestamp: chrono::Local::now(),
                        });
                        // Send the skill prompt to the model via executor
                        state.is_generating = true;
                        let _ = input_tx.send(rendered);
                    } else {
                        state.messages.push(ConversationEntry {
                            role: "system".into(),
                            content: format!(
                                "Unknown command: {}\nType /help for available commands.",
                                cmd
                            ),
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
                    state.is_generating = true;
                    let _ = input_tx.send(input);
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
            state.scroll_offset = state.scroll_offset.saturating_add(1);
        }
        KeyCode::PageUp => {
            state.auto_scroll = false;
            state.scroll_offset = state.scroll_offset.saturating_sub(10);
        }
        KeyCode::PageDown => {
            state.scroll_offset = state.scroll_offset.saturating_add(10);
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

/// Handle /fork command: list forks or create a new one.
async fn handle_fork_command(args: &str, state: &mut AppState) -> String {
    let fm = match state.fork_manager.as_mut() {
        Some(fm) => fm,
        None => return "No fork manager available (session not initialized).".into(),
    };

    let trimmed = args.trim();
    if trimmed.eq_ignore_ascii_case("list") || trimmed.is_empty() {
        let forks = fm.list();
        if forks.is_empty() {
            return "No forks created yet. Use `/fork <label> [count]` to create one.".into();
        }
        let mut out = "Session forks:\n".to_string();
        for f in forks {
            out.push_str(&format!(
                "  {} — {} (fork point: {}, created: {})\n",
                f.id, f.label, f.fork_point, f.created_at
            ));
        }
        return out;
    }

    // Parse: label [count]
    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    if parts.is_empty() || parts[0].is_empty() {
        return "Usage: /fork list | /fork <label> [count]".into();
    }

    let label = parts[0];

    // Build a fake ConversationLog from our messages for fork creation
    // We use the conversation log path stored in state
    let log_path = match &state.log_path {
        Some(p) => p.clone(),
        None => return "No log path available. Cannot create fork.".into(),
    };

    // Open the conversation log to read the latest state
    match crate::session::conversation::ConversationLog::open(log_path) {
        Ok(conv_log) => {
            // Fork point: -1 (end) by default, or parse an optional count
            let fork_point: i64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);

            match fm.create_fork(label, &conv_log, fork_point) {
                Ok(fork) => format!(
                    "✅ Created fork {} — \"{}\" at message #{} (path: {})",
                    fork.id,
                    fork.label,
                    fork.fork_point,
                    fork.path.display()
                ),
                Err(e) => format!("Error creating fork: {}", e),
            }
        }
        Err(e) => format!("Error opening conversation log: {}", e),
    }
}

/// Handle /resume command: switch the active session to a fork.
///
/// Usage: /resume <fork-id>
async fn handle_resume_command(
    args: &str,
    state: &mut AppState,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
) -> String {
    let fork_id = args.trim();
    if fork_id.is_empty() {
        return "Usage: /resume <fork-id>\nUse `/fork list` to see available forks.".into();
    }

    let fm = match state.fork_manager.as_mut() {
        Some(fm) => fm,
        None => return "No fork manager available.".into(),
    };

    // Verify the fork exists
    let fork = match fm.get(fork_id) {
        Some(f) => f.clone(),
        None => {
            let available: Vec<&str> = fm.list().iter().map(|f| f.id.as_str()).collect();
            return format!(
                "Fork '{}' not found. Available forks: [{}]",
                fork_id,
                available.join(", ")
            );
        }
    };

    // Open the fork's conversation log
    match ConversationLog::open(fork.path.clone()) {
        Ok(fork_log) => {
            // Send the fork log to the executor (swaps in-place)
            if resume_tx.send(fork_log).is_err() {
                return "Error: executor is not running.".into();
            }

            // Reload TUI state from the fork's conversation
            state.messages.clear();
            state.thinking_buffer.clear();

            // Update session identity
            state.session_id = format!("{} (fork: {})", state.session_id, fork.id);
            state.log_path = Some(fork.path);

            format!(
                "✅ Resumed fork '{}' — \"{}\" ({} messages). Type a message to continue.",
                fork.id,
                fork.label,
                fork.fork_point,
            )
        }
        Err(e) => format!("Error opening fork log: {}", e),
    }
}

/// Handle /jobs command: list background bash jobs.
async fn handle_jobs_command() -> String {
    let registry = crate::session::bash_jobs::global_registry();
    let jobs = registry.list().await;
    if jobs.is_empty() {
        return "No background jobs.".into();
    }
    let mut out = "Background jobs:\n".to_string();
    for job in &jobs {
        let status = match &job.status {
            crate::session::bash_jobs::JobStatus::Running => format!("⏳ running (id={})", job.id),
            crate::session::bash_jobs::JobStatus::Completed(code) => {
                format!("✅ completed #{} (exit {})", job.id, code)
            }
            crate::session::bash_jobs::JobStatus::Failed(e) => {
                format!("❌ failed #{}: {}", job.id, e)
            }
            crate::session::bash_jobs::JobStatus::Cancelled => format!("🚫 cancelled #{}", job.id),
        };
        out.push_str(&format!("  {} — {}\n", status, job.command));
    }
    out.pop();
    out
}

/// Check for recently-completed background jobs and push a notification.
async fn notify_completed_jobs(state: &mut AppState) {
    let registry = crate::session::bash_jobs::global_registry();
    let jobs = registry.list().await;
    for job in &jobs {
        let finished = match job.status {
            crate::session::bash_jobs::JobStatus::Completed(_)
            | crate::session::bash_jobs::JobStatus::Failed(_)
            | crate::session::bash_jobs::JobStatus::Cancelled => true,
            crate::session::bash_jobs::JobStatus::Running => false,
        };
        if finished && state.notified_jobs.insert(job.id) {
            // First time seeing this job as finished — push a notification
            let status_icon = match &job.status {
                crate::session::bash_jobs::JobStatus::Completed(code) => {
                    format!("✅ Job #{} completed (exit {})", job.id, code)
                }
                crate::session::bash_jobs::JobStatus::Failed(e) => {
                    format!("❌ Job #{} failed: {}", job.id, e)
                }
                crate::session::bash_jobs::JobStatus::Cancelled => {
                    format!("🚫 Job #{} cancelled", job.id)
                }
                _ => continue,
            };
            state.messages.push(ConversationEntry {
                role: "system".into(),
                content: format!("{} — `{}`", status_icon, job.command),
                timestamp: chrono::Local::now(),
            });
        }
    }
}
