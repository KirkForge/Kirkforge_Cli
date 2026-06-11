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

/// One-shot probe of the configured Ollama endpoint.
///
/// Returns `Connected { model, since: now }` if `${ollama_host}/api/tags`
/// responds with 2xx within a 2-second budget, `Error(msg)` on transport
/// failure or non-2xx status, and `Disconnected` only if the host string
/// is empty or unparseable.
///
/// We hit `/api/tags` rather than `/api/version` because it doubles as a
/// "do we have this model?" check, and the response (a JSON object
/// listing the available models) is what the executor's smart router
/// will consult anyway. A 2s budget is generous — local Ollama answers
/// in <50ms, and the cloud route usually <500ms.
///
/// Failure modes are non-fatal: the TUI starts up either way, the user
/// can still type, and the first turn will surface the underlying
/// connection error via the executor's normal error path.
async fn probe_ollama_connection(config: &Config) -> ConnectionState {
    let host = config.ollama_host.trim_end_matches('/');
    if host.is_empty() {
        return ConnectionState::Error("empty ollama_host in config".into());
    }
    let url = format!("{}/api/tags", host);
    let model = config.default_model.clone();
    let since = Instant::now();

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(e) => return ConnectionState::Error(format!("client build failed: {}", e)),
    };

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => ConnectionState::Connected { model, since },
        Ok(resp) => ConnectionState::Error(format!("{}: HTTP {}", url, resp.status().as_u16())),
        Err(e) => ConnectionState::Error(format!("{}: {}", url, e)),
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
    // Hook for sessions that need a connection indicator.
    //
    // Probes Ollama at startup so the status bar reflects reality
    // instead of lying on `Disconnected` for the entire session
    // (2026-06-11 incident — the original code set `Disconnected`
    // once at construction and never updated it, so the status
    // bar said "Disconnected" even on a fully-working install).
    //
    // One-shot probe: doesn't poll. If Ollama goes down mid-session,
    // the bar will continue to show the last-known state. A v2
    // improvement would be a periodic background probe driven by
    // the SIGHUP / reconnect signal path.
    state.connection = probe_ollama_connection(&config).await;

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
    // Slow-tick: drives time-based UI elements (spinner, the
    // 4Hz refresh of the status bar's elapsed-time display).
    // 250ms = 4Hz, which is smooth enough for the spinner
    // (12-frame animation, full cycle every 3s) and slow enough
    // that the slow-tick never dominates idle CPU.
    let mut slow_tick = tokio::time::interval(std::time::Duration::from_millis(250));
    slow_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

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
        &mut slow_tick,
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
    slow_tick: &mut tokio::time::Interval,
) -> anyhow::Result<()> {
    loop {
        // Check for exit signal
        if state.should_exit {
            break Ok(());
        }

        // ── Frame-pacing v2: render-on-state-change ───────────────
        //
        // The earlier pattern (v1, 2026-06-11) was:
        //   1. drain events
        //   2. render
        //   3. drain keys
        //   4. sleep 16ms
        //
        // That worked but burned ~5% CPU per idle session because
        // step 2 re-rendered the same frame every iteration even
        // when nothing had changed. The v2 pattern is event-driven:
        // we `select!` on the four things that can cause a redraw
        // (kb event, executor event, approval event, 4Hz slow-tick)
        // and only render when `state.dirty` is set.
        //
        // `drain_*` calls below mutate state and `mark_dirty()`
        // internally. The slow-tick `interval.tick()` always sets
        // dirty (drives the spinner). Key handling sets dirty
        // implicitly via the state mutations inside
        // `handle_input_key` / `handle_approval_key`. Resize
        // events also mark dirty.
        //
        // If `state.dirty` is still false after all of the above,
        // we skip the render entirely. This is the case in 99% of
        // iterations during a quiet session — the loop is mostly
        // `select!` waiting, with no work to do.

        let mut kb_event: Option<Event> = None;
        let mut had_executor_event = false;
        let mut had_approval_event = false;
        let mut had_approval_pending = state.pending_approval.is_some() || state.pending_bang.is_some();
        let mut dirty_from_tick = false;

        tokio::select! {
            // Bias the select! slightly toward real events so we
            // don't drop a kb event when the slow-tick happens to
            // fire at the same instant. `tokio::select!` polls
            // branches top-to-bottom; the slow-tick is the lowest
            // priority, so it'll only fire when nothing else is
            // ready.
            ev = kb_rx.recv() => {
                kb_event = ev;
            }
            ev = event_rx.recv() => {
                if ev.is_some() {
                    had_executor_event = true;
                }
            }
            ev = approval_rx.recv() => {
                if ev.is_some() {
                    had_approval_event = true;
                }
            }
            _ = slow_tick.tick() => {
                dirty_from_tick = true;
            }
        }

        // ── Drain events that have accumulated since last loop ──
        // The `select!` above waits on the *first* of each channel
        // to become ready; everything queued after that is also
        // drained here in a tight loop. This is the same work the
        // v1 loop did on every iteration — now it only happens
        // when at least one event source is actually ready.
        if had_executor_event {
            drain_turn_events(state, event_rx);
            state.mark_dirty();
        }
        if had_approval_event {
            drain_approval_requests(state, approval_rx);
            state.mark_dirty();
        }
        // Jobs and kb events are also work that may have been
        // waiting. We always drain jobs (cheap) and process any
        // kb event we just got. If nothing happened, none of this
        // marks the state dirty.

        // notify_completed_jobs mutates state when it pushes a
        // notification, and is cheap (O(n) in jobs, single lock).
        // It returns `true` only when it actually pushed something;
        // we use that to set the dirty flag (so a no-op
        // notification pass doesn't schedule an unnecessary redraw).
        if notify_completed_jobs(state).await {
            state.mark_dirty();
        }

        // Process the kb event (if any). The handlers
        // (`handle_input_key` / `handle_approval_key` /
        // `handle_bang_approval_key`) call `state.mark_dirty()`
        // internally via their state mutations, but we also
        // explicitly mark dirty here because a no-op key event
        // (e.g. Shift held down) shouldn't redraw, and the
        // explicit mark ensures the redraw still happens on the
        // first key event regardless of which handler ran.
        if let Some(ev) = kb_event {
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
                Event::Resize(_w, _h) => {
                    // Terminal size changed — the layout has to
                    // recompute. Mark dirty so the next render
                    // uses the new size even if no other state
                    // changed.
                    state.mark_dirty();
                }
                _ => {}
            }
        }

        // Also drain any other kb events that arrived in the same
        // burst (e.g. a paste that's multiple key events). These
        // were already there in the v1 loop; we keep the
        // try_recv loop for them.
        while let Ok(ev) = kb_rx.try_recv() {
            match ev {
                Event::Key(key) => {
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
                Event::Resize(_w, _h) => state.mark_dirty(),
                _ => {}
            }
        }

        // ── Approval dialog appeared mid-iteration ─────────────
        // The drain functions above set `state.pending_approval` /
        // `state.pending_bang` if a new approval arrived. Track
        // this so the next render (even if it would otherwise be
        // skipped) draws the dialog. Mirrors the v1 behavior of
        // always rendering so the dialog appears immediately.
        if state.pending_approval.is_some() || state.pending_bang.is_some() {
            had_approval_pending = true;
        }
        if had_approval_pending {
            state.mark_dirty();
        }

        // ── Slow-tick: drive the spinner + any other clock-driven UI ──
        if dirty_from_tick {
            state.spinner_tick = state.spinner_tick.wrapping_add(1);
            state.mark_dirty();
        }

        // ── Render (only if dirty) ──────────────────────────────
        if !state.dirty {
            // Nothing to draw. The `select!` above already
            // incorporated a 250ms wait (slow_tick interval), so
            // the loop is naturally rate-limited. The CPU
            // profile at idle is essentially zero on this path.
            continue;
        }
        state.dirty = false;

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
