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
pub mod clipboard;
pub mod commands;
pub mod components;
pub mod events;
pub mod keys;
pub mod rendering;
pub mod search;
pub mod widgets;

use crate::session::carryover::CarryoverProfile;
use crate::session::conversation::ConversationLog;
use crate::session::executor::{self, ApprovalRequest};
use crate::shared::Config;
use crate::tools::Tool;
use app::{AppState, ConnectionState};
use commands::notify_completed_jobs;
use components::approval::render_approval_dialog;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use events::{drain_approval_requests, drain_turn_events};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;
use widgets::chat::render_chat;
use widgets::input::render_input;
use widgets::status::render_status;

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
    system: Option<String>,
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
    // Model swap: TUI → Executor (sends a model name to install mid-session)
    // Review.md gap #5. Mirror of the other control channels. The
    // TUI owns the sender (passed into `keys::handle_input_key`); the
    // executor's `run` loop receives the name and calls
    // `AdapterSwap::force_swap`.
    let (model_tx, model_rx) = mpsc::unbounded_channel::<String>();
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

    // SIGHUP config hot-reload (review.md gap #5, second half).
    // On Unix, the conventional "reload config" signal is SIGHUP.
    // When we receive one, re-read `config.toml` and emit a token
    // through `event_tx` so the user sees the reload happen. The
    // reloaded config is *display-only* — the executor captured its
    // Config by value at construction and is not externally
    // mutable, so the executor keeps using the launch-time config
    // for routing, auto-approve, etc. What the user does see is the
    // new config in `/status` (after the next render) and in any
    // dialog that re-queries Config (e.g. approval text). Full
    // hot-reload of the executor's behavior would require
    // Arc<RwLock<Config>> plumbing; deferred to a follow-up.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::hangup()) {
            Ok(mut hup) => {
                let reload_event_tx = event_tx.clone();
                let mut last_known = state.config.clone();
                tokio::spawn(async move {
                    while hup.recv().await.is_some() {
                        let fresh = crate::session::config::load_config();
                        let diff_summary = config_diff_summary(&last_known, &fresh);
                        last_known = fresh;
                        let msg = if diff_summary.is_empty() {
                            "🔄 Reloaded config (no changes)\n".to_string()
                        } else {
                            format!("🔄 Reloaded config: {}\n", diff_summary)
                        };
                        // Best-effort: the TUI's event drain renders
                        // tokens as system lines. If the receiver is
                        // gone (TUI exited) we just drop the message.
                        let _ = reload_event_tx
                            .send(executor::TurnEvent::Token(msg));
                    }
                });
            }
            Err(e) => {
                tracing::warn!("Could not install SIGHUP handler: {}", e);
            }
        }
    }

    // Spawn the executor on a background task
    let mut exe =
        executor::Executor::with_log(adapter, tools, config, conversation_log, carryover_target);
    // Apply --system override before the executor starts processing
    // input. Without this, --system is silently dropped (was GPT 5.5
    // review finding #2).
    exe.set_system_override(system);
    let handle = tokio::spawn(async move {
        let _ = exe
            .run(
                input_rx,
                event_tx,
                approval_tx,
                cancel_rx,
                resume_rx,
                compact_rx,
                model_rx,
            )
            .await;
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
        &model_tx,
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
    model_tx: &mpsc::UnboundedSender<String>,
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
            //
            // The bang-approval gate (review.md arch concern #1) uses
            // the same dialog shape via `pending_bang`. We render it
            // identically — only the key handler knows the difference
            // (see `approval_keys::handle_bang_approval_key`).
            let pending_taken = state.pending_approval.take();
            if let Some(ref approval) = pending_taken {
                render_approval_dialog(f, size, approval, state);
            } else if let Some(ref bang) = state.pending_bang {
                // Synthesize a transient `PendingApproval` view of the
                // bang command so the dialog renders the same way. The
                // `responder` is `None` because bang is a local flow
                // (no executor oneshot).
                let synthetic = crate::tui::app::PendingApproval {
                    tool_name: "!bash".into(),
                    args: serde_json::json!({ "command": bang.cmd }),
                    responder: None,
                };
                render_approval_dialog(f, size, &synthetic, state);
            }
            state.pending_approval = pending_taken;
        })?;

        // ── Handle keyboard events (non-blocking, from background thread) ──
        while let Ok(ev) = kb_rx.try_recv() {
            match ev {
                Event::Key(key) => {
                    // Order matters: the bang-approval gate (review.md
                    // arch concern #1) takes priority over the model
                    // approval because its response is purely local.
                    if state.pending_bang.is_some() {
                        approval_keys::handle_bang_approval_key(key, state);
                    } else if state.pending_approval.is_some() {
                        approval_keys::handle_approval_key(key, state);
                    } else {
                        keys::handle_input_key(
                            key, state, input_tx, cancel_tx, resume_tx, compact_tx, model_tx,
                        )
                        .await?;
                    }
                }
                Event::Resize(_w, _h) => {}
                _ => {}
            }
        }
    }
}

/// Pure helper: produce a one-line summary of the differences between
/// two `Config` values, used by the SIGHUP reload path to tell the
/// user what changed (or that nothing did).
///
/// We deliberately compare a small, *user-facing* subset of fields
/// — not the full struct equality. Showing changes to internal
/// knobs (truncation_strategy, deny_paths, etc.) would be noisy and
/// could leak security-sensitive details in a chat pane. The
/// high-impact fields the operator usually tweaks are: model,
/// host, auto_approve, bang_requires_approval, sandbox_dir.
///
/// Returns an empty string when the two configs are equal on this
/// subset, so the caller can show "no changes" instead of a
/// confusing "0 changes" line.
fn config_diff_summary(before: &crate::shared::Config, after: &crate::shared::Config) -> String {
    let mut diffs: Vec<String> = Vec::new();
    if before.default_model != after.default_model {
        diffs.push(format!(
            "default_model: {} → {}",
            before.default_model, after.default_model
        ));
    }
    if before.ollama_host != after.ollama_host {
        diffs.push(format!(
            "ollama_host: {} → {}",
            before.ollama_host, after.ollama_host
        ));
    }
    if before.auto_approve != after.auto_approve {
        diffs.push(format!(
            "auto_approve: {} → {}",
            before.auto_approve, after.auto_approve
        ));
    }
    if before.bang_requires_approval != after.bang_requires_approval {
        diffs.push(format!(
            "bang_requires_approval: {} → {}",
            before.bang_requires_approval, after.bang_requires_approval
        ));
    }
    if before.sandbox_dir != after.sandbox_dir {
        diffs.push(format!(
            "sandbox_dir: {:?} → {:?}",
            before.sandbox_dir, after.sandbox_dir
        ));
    }
    if before.routing_enabled != after.routing_enabled {
        diffs.push(format!(
            "routing_enabled: {} → {}",
            before.routing_enabled, after.routing_enabled
        ));
    }
    if before.summarize_enabled != after.summarize_enabled {
        diffs.push(format!(
            "summarize_enabled: {} → {}",
            before.summarize_enabled, after.summarize_enabled
        ));
    }
    diffs.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;

    #[test]
    fn test_config_diff_summary_empty_for_equal() {
        let a = Config::default();
        let b = Config::default();
        assert!(config_diff_summary(&a, &b).is_empty());
    }

    #[test]
    fn test_config_diff_summary_model_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.default_model = "qwen2.5:3b".into();
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("default_model"), "got: {}", s);
        assert!(s.contains("→ qwen2.5:3b"), "got: {}", s);
    }

    #[test]
    fn test_config_diff_summary_multiple_fields() {
        let a = Config::default();
        let mut b = Config::default();
        b.default_model = "qwen2.5:3b".into();
        b.auto_approve = true;
        b.ollama_host = "http://example.com:11434".into();
        let s = config_diff_summary(&a, &b);
        // All three should appear; order is the field order above.
        assert!(s.contains("default_model"), "got: {}", s);
        assert!(s.contains("auto_approve"), "got: {}", s);
        assert!(s.contains("ollama_host"), "got: {}", s);
    }

    #[test]
    fn test_config_diff_summary_ignores_internal_fields() {
        // deny_paths and friends should NOT show up in the diff
        // even if they differ — those are internal/security knobs.
        let a = Config::default();
        let mut b = Config::default();
        b.deny_paths = vec!["/secret".into()];
        b.allowed_write_dirs = vec!["/tmp".into()];
        let s = config_diff_summary(&a, &b);
        assert!(
            !s.contains("deny_paths") && !s.contains("allowed_write_dirs"),
            "internal fields leaked: {}",
            s
        );
        assert!(s.is_empty());
    }
}
