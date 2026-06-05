//! TUI entry point and event loop.
//!
//! The interactive terminal UI is a thin orchestrator that:
//!   1. Spins up the executor on a background task.
//!   2. Drains three event sources in a single loop: stream events from
//!      the executor, approval requests, and keyboard events from a
//!      dedicated thread.
//!   3. Renders the chat / input / status panels, optionally overlaid
//!      with an approval dialog.
//!   4. Routes keyboard input either to the input handler (regular mode)
//!      or the approval handler (when a pending approval is on screen).
//!
//! Key handling, slash-command logic, and event dispatch live in
//! sibling modules:
//!   - `keys`            — input-mode keyboard handler
//!   - `approval_keys`   — approval-mode keyboard handler
//!   - `commands`        — /fork, /resume, /jobs, and background-job notifier
//!   - `events`          — TurnEvent + ApprovalRequest dispatch
//!
//! Keeping these in their own files lets `mod.rs` stay focused on
//! orchestration and makes each piece unit-testable in isolation.

pub mod app;
pub mod approval_keys;
pub mod commands;
pub mod components;
pub mod events;
pub mod keys;
pub mod rendering;
pub mod widgets;

use crate::session::carryover::CarryoverProfile;
use crate::session::conversation::ConversationLog;
use crate::session::executor::{self, ApprovalRequest};
use crate::shared::Config;
use crate::tools::Tool;
use app::{AppState, ConnectionState};
use commands::notify_completed_jobs;
use components::approval::render_approval_dialog;
use events::{drain_approval_requests, drain_turn_events};
use widgets::chat::render_chat;
use widgets::input::render_input;
use widgets::status::render_status;
use crossterm::{
    event::{self, Event},
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
    tools: Vec<std::sync::Arc<dyn Tool>>,
    conversation_log: ConversationLog,
) -> anyhow::Result<()> {
    // ── Terminal setup ──
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let _guard = TerminalGuard;

    // ── AppState ──
    let mut state = AppState::new(config.clone());
    state.session_started = Instant::now();
    // Hook for sessions that need a connection indicator
    state.connection = ConnectionState::Disconnected;

    // Skills — load any project-local SKILL.md files from registered scan paths,
    // then layer the built-in skills on top. (Missing dirs are silently skipped,
    // so an empty project is fine.)
    if let Err(e) = state.skill_registry.scan_and_load() {
        tracing::warn!("Skill scan error: {}", e);
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
    // Compact: TUI → Executor (sends () to trigger a /compact pass)
    let (compact_tx, compact_rx) = mpsc::unbounded_channel::<()>();
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
        let _ = exe.run(input_rx, event_tx, approval_tx, cancel_rx, resume_rx, compact_rx).await;
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
        &compact_tx,
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
    compact_tx: &mpsc::UnboundedSender<()>,
) -> anyhow::Result<()> {
    loop {
        // Check for exit signal
        if state.should_exit {
            break Ok(());
        }

        // ── Drain pending stream events ──
        // Each event is a pure mutation of `state`. See
        // `tui::events` for the per-variant handlers and tests.
        drain_turn_events(state, event_rx);

        // ── Check for completed background jobs ──
        notify_completed_jobs(state).await;

        // ── Drain pending approval requests ──
        // If a new approval arrives while one is pending, deny the old one
        // before replacing it — otherwise the old oneshot sender is dropped
        // without sending, causing the executor to hang forever.
        drain_approval_requests(state, approval_rx);

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

            // Approval dialog overlay.
            //
            // `render_approval_dialog` needs both a `&PendingApproval` (to
            // display the args preview) and `&mut state` (to clamp
            // `state.approval_scroll` / `state.approval_max_scroll`). We
            // can't hold both borrows simultaneously because the immutable
            // borrow of `state.pending_approval` would extend through the
            // call site and conflict with the mutable borrow.
            //
            // The fix is `std::mem::take`: swap the `Option<PendingApproval>`
            // out for `None` (replacing the contained value with a sentinel
            // `None` via `mem::replace`), pass the owned approval by ref to
            // the renderer, then put it back. The closure is the cleanest
            // way to scope the `&mut state` borrow tightly.
            //
            // `std::mem::take` is sound here because:
            //   1. `pending_approval` is `Option<PendingApproval>`, and
            //      `None` is a valid value for it.
            //   2. We immediately restore the original value after the call.
            //   3. The dialog is the only consumer of `pending_approval`,
            //      and we're already inside the render path so no other
            //      code can observe the temporary `None`.
            let pending_taken = state.pending_approval.take();
            if let Some(ref approval) = pending_taken {
                render_approval_dialog(f, size, approval, state);
            }
            state.pending_approval = pending_taken;
        })?;

        // ── Handle keyboard events (non-blocking, from background thread) ──
        while let Ok(ev) = kb_rx.try_recv() {
            match ev {
                Event::Key(key) => {
                    if state.pending_approval.is_some() {
                        approval_keys::handle_approval_key(key, state);
                    } else {
                        keys::handle_input_key(key, state, input_tx, cancel_tx, resume_tx, compact_tx).await?;
                    }
                }
                Event::Resize(_w, _h) => {}
                _ => {}
            }
        }
    }
}
