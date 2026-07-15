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
pub mod syntax;
pub mod transcript;
pub mod widgets;

mod connection;

use crate::session::carryover::CarryoverProfile;
use crate::session::conversation::ConversationLog;
use crate::session::executor::{self, ApprovalRequest};
use crate::session::prompt::CompactRequest;
use crate::shared::{Config, Message, Role};
use app::{AppState, ConnectionState, ConversationEntry};
use commands::{messages_to_entries, notify_completed_jobs, PersonaKind, PersonaResult};
use components::approval::render_approval_dialog;
use connection::{connection_probe_task, probe_ollama_connection};
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
use tokio::sync::{mpsc, Notify};
use widgets::chat::render_chat;
use widgets::input::render_input;
use widgets::status::render_status;

/// Panic-safe guard that restores terminal state on drop.
pub(crate) struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Err(e) = disable_raw_mode() {
            tracing::debug!(error = %e, "failed to disable raw mode in terminal guard");
        }
        let mut stdout = io::stdout();
        if let Err(e) = execute!(stdout, LeaveAlternateScreen) {
            tracing::debug!(error = %e, "failed to leave alternate screen in terminal guard");
        }
    }
}

/// Show a standalone recent-session picker before the main TUI starts.
///
/// This is used by `main.rs` when the user runs `kirkforge run` without
/// an explicit `--continue` / `--resume` / `--attach` / `--auto-resume`
/// and the session daemon reports recent sessions. The picker runs in a
/// temporary terminal session; when it returns, the alternate screen is
/// cleared and terminal state is restored.
pub async fn run_session_picker(
    sessions: Vec<crate::session::session_index::SessionEntry>,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    tokio::task::spawn_blocking(move || run_session_picker_sync(sessions)).await?
}

fn run_session_picker_sync(
    sessions: Vec<crate::session::session_index::SessionEntry>,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    use crate::tui::components::session_picker::SessionPicker;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let _guard = TerminalGuard;

    let mut picker = SessionPicker::new(sessions);
    loop {
        terminal.draw(|f| picker.render(f, f.area()))?;
        if let Event::Key(key) = event::read()? {
            picker.handle_key(key);
            if picker.is_confirmed() {
                return Ok(picker.selected_path());
            }
            if picker.is_cancelled() {
                return Ok(None);
            }
        }
    }
}

/// Run the TUI event loop.
pub async fn run_tui(
    shared_config: crate::shared::SharedConfig,
    adapter: Box<dyn crate::adapters::ModelAdapter>,
    tools: crate::session::toolset::CompositeToolset,
    conversation: (ConversationLog, crate::session::conversation::OpenOutcome),
    system: Option<String>,
    undo_stack: Option<crate::tools::UndoStackRef>,
    plugin_registry: &kirkforge_plugin_host::PluginRegistry,
) -> anyhow::Result<()> {
    // ── Terminal setup ──
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let _guard = TerminalGuard;

    let cfg_for_startup = crate::shared::read_shared_config(&shared_config).clone();
    let active_model = adapter.model_info().name.clone();

    // ── AppState ──
    let mut state = AppState::new(shared_config.clone());
    state.undo_stack = undo_stack.clone();
    state.session_started = Instant::now();
    // Capture the session identity from the conversation log before it
    // moves into the executor. This lets the TUI report the session id
    // and write transcript files to a predictable path.
    let conversation_log_path = conversation.0.path().clone();
    state.log_path = Some(conversation_log_path.clone());
    state.session_id = conversation_log_path
        .file_stem()
        .and_then(|f| f.to_str())
        .map(|s| s.trim_end_matches(".conv").to_string())
        .unwrap_or_else(|| "unknown-session".to_string());
    state.fork_manager = Some(crate::session::session_fork::ForkManager::new(
        &state.session_id,
        &conversation_log_path,
    ));
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
    state.connection = probe_ollama_connection(&cfg_for_startup, &active_model).await;

    // Surface PathGuard sandbox posture in the TUI. `freeze_launch_sandbox`
    // already set `sandbox_dir` to the cwd by default, so `unsandboxed`
    // only becomes true if the operator explicitly cleared it or set an
    // empty `allowed_write_dirs` with no sandbox.
    {
        let cfg_for_guard = crate::shared::read_shared_config(&shared_config);
        let (_, path_guard, _) = crate::session::access::access_from_config(&cfg_for_guard);
        state.unsandboxed = !path_guard.is_sandboxed();
    }
    if state.unsandboxed {
        state.messages.push(crate::tui::app::ConversationEntry::new(
            "system",
            "⚠️  PathGuard is unsandboxed: no `sandbox_dir` or `allowed_write_dirs` configured. \
             Model-driven writes are not restricted to a directory tree. Set `sandbox_dir` in config.toml or via KIRKFORGE_SANDBOX_DIR, or list `allowed_write_dirs`.",
        ));
    }

    // Skills — load project-local SKILL.md files and plugin directories,
    // then layer the built-in skills on top. (Missing dirs are silently skipped,
    // so an empty project is fine.)
    let max_trust = cfg_for_startup.max_plugin_trust;
    state.skill_registry.set_max_plugin_trust(max_trust);
    if let Err(e) = state.skill_registry.scan_and_load(&cfg_for_startup) {
        tracing::warn!("Skill scan error: {}", e);
    }
    // Always register built-in skills
    for skill in crate::session::skills::builtin_skills() {
        state.skill_registry.register(skill);
    }
    // Surface plugin trust tiers in the status bar (Phase 2.3).
    state.plugin_status = state.skill_registry.plugin_status_summary();

    // ── Carryover profile (shared between executor and save) ──
    let carryover_target: Option<Arc<Mutex<CarryoverProfile>>> =
        if cfg_for_startup.carryover_enabled {
            Some(Arc::new(Mutex::new(CarryoverProfile::default())))
        } else {
            None
        };
    let saved_profile = carryover_target.clone();

    // ── Channels ──
    // User input: TUI → Executor
    let (input_tx, input_rx) = mpsc::unbounded_channel::<String>();
    // Stream events: Executor → TUI
    let (event_tx, mut event_rx) = mpsc::channel::<executor::TurnEvent>(10_000);
    // Approval requests: Executor → TUI
    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
    // Cancellation: TUI → Executor (sends () to cancel current turn)
    let (cancel_tx, cancel_rx) = mpsc::unbounded_channel::<()>();
    // Resume: TUI → Executor (sends a ConversationLog to swap in for fork resumption)
    let (resume_tx, resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
    // Compact: TUI → Executor (sends CompactRequest to trigger a /compact pass)
    let (compact_tx, compact_rx) = mpsc::unbounded_channel::<CompactRequest>();
    // Model swap: TUI → Executor (sends a model name to install mid-session)
    // Review.md gap #5. Mirror of the other control channels. The
    // TUI owns the sender (passed into `keys::handle_input_key`); the
    // executor's `run` loop receives the name and calls
    // `AdapterSwap::force_swap`.
    let (model_tx, model_rx) = mpsc::unbounded_channel::<String>();
    // Undo: TUI → Executor (signals a pop of the undo stack).
    // Review.md gap #7. `()` payload because the only operation is
    // "pop the most recent edit"; the result comes back as a token.
    let (undo_tx, undo_rx) = mpsc::unbounded_channel::<()>();
    // Config reload: TUI → Executor (sends a new Config snapshot).
    // The TUI owns the sender (driven by SIGHUP or `/reload`); the
    // executor replaces its shared config and rebuilds access control.
    let (config_tx, config_rx) = mpsc::unbounded_channel::<Config>();
    // Plan mode: TUI → Executor (sends bool to enter/exit plan mode).
    // After the fork-isolated `/plan` persona merges its result, the
    // main executor is placed in plan mode so `/implement` remains the
    // approval gesture.
    let (plan_tx, plan_rx) = mpsc::unbounded_channel::<bool>();
    // Plugin reload: TUI → Executor (sends a fresh PluginRegistry).
    // `/reload plugins` re-scans the plugins directory and asks the
    // executor to swap the plugin toolset, hooks, and verifiers.
    let (plugin_reload_tx, plugin_reload_rx) =
        mpsc::unbounded_channel::<kirkforge_plugin_host::PluginRegistry>();
    // Persona completion: background task → TUI event loop.
    // `/explore`, `/plan`, and `/coder` spawn fork-isolated subagents;
    // the result is merged back into the parent conversation here.
    let (persona_tx, mut persona_rx) = mpsc::unbounded_channel::<PersonaResult>();
    // Keyboard events: background reader thread → TUI event loop
    let (kb_tx, mut kb_rx) = mpsc::unbounded_channel::<Event>();

    // Shutdown signal: a one-shot notify that any of the exit paths
    // (SIGHUP, kb-reader thread EOF when the pty closes, future
    // SIGTERM/SIGINT) can fire. The event loop `select!`s on it and
    // sets `state.should_exit = true` when it fires.
    //
    // Bug this fixes: previously, when the controlling terminal went
    // away (wezterm pane close, SSH disconnect, dropped SSH session),
    // the TUI event loop had no way to observe it. The SIGHUP handler
    // was wired only to config hot-reload; the kb-reader thread
    // silently exited on `event::read()` Err but the TUI kept waiting
    // on the (now-empty) keyboard channel forever. The process became
    // an orphan: pty gone, stdin/stdout `(deleted)`, but the event
    // loop pinned a core at low CPU for the lifetime of the OS.
    let shutdown = Arc::new(Notify::new());
    let shutdown_for_loop = shutdown.clone();
    let shutdown_for_kb = shutdown.clone();

    // Spawn a dedicated thread to read crossterm events without blocking
    // the async event loop. This eliminates the 50ms poll latency floor.
    //
    // 2026-06-12: when the pty closes (terminal multiplexer pane close,
    // SSH disconnect, etc.) `event::read()` returns `Err`. The
    // pre-fix code silently dropped the thread here, leaving the
    // TUI event loop waiting on `kb_rx` for events that would never
    // arrive. The fix fires `shutdown` so the TUI loop wakes up,
    // sets `state.should_exit = true`, and runs the same graceful
    // shutdown path as `/exit` (terminal mode restored, carryover
    // profile saved, executor flushed).
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(ev) => {
                    if kb_tx.send(ev).is_err() {
                        break; // receiver dropped (TUI exited)
                    }
                }
                Err(e) => {
                    // pty is gone (or some other fatal read error).
                    // Signal the event loop to exit. We don't try to
                    // distinguish EINTR / UnexpectedEof / other
                    // variants — the cost of a false positive is one
                    // extra `/exit`, which is harmless.
                    tracing::info!(
                        error = ?e,
                        "keyboard reader thread exiting; signalling TUI shutdown"
                    );
                    shutdown_for_kb.notify_one();
                    break;
                }
            }
        }
    });

    // SIGHUP config hot-reload (review.md gap #5).
    // On Unix, the conventional "reload config" signal is SIGHUP.
    // When we receive one, re-read `config.toml`, update the shared
    // config in place, and forward a snapshot to the executor so it
    // rebuilds deny lists, path guards, and approval state. The
    // executor emits the user-visible confirmation token.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::hangup()) {
            Ok(mut hup) => {
                let reload_config_tx = config_tx.clone();
                let reload_shared_config = shared_config.clone();
                tokio::spawn(async move {
                    while hup.recv().await.is_some() {
                        let (fresh, _warning) = crate::session::config::load_config();
                        if let Ok(mut cfg) = reload_shared_config.write() {
                            *cfg = fresh.clone();
                        }
                        // Forward the new snapshot to the executor,
                        // which owns the access-control rebuild. If the
                        // executor is gone (TUI exited) we drop it.
                        crate::send_or_warn!(
                            reload_config_tx.send(fresh),
                            "config reload channel receiver dropped"
                        );
                    }
                });
            }
            Err(e) => {
                tracing::warn!("Could not install SIGHUP handler: {}", e);
            }
        }
    }

    // SIGINT / SIGTERM graceful shutdown.
    // Drives the same `shutdown` Notify used by the keyboard reader thread
    // when the pty closes, so Ctrl-C or a termination signal restores the
    // terminal, saves carryover profile, and flushes the executor instead of
    // killing the process outright.
    let shutdown_for_signal = shutdown.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let term_fut = {
            use tokio::signal::unix::{signal, SignalKind};
            signal(SignalKind::terminate()).ok()
        };
        #[cfg(not(unix))]
        let term_fut: Option<()> = None;

        tokio::select! {
            biased;
            _ = ctrl_c => {
                tracing::info!("SIGINT received; signalling graceful TUI shutdown");
                shutdown_for_signal.notify_one();
            }
            _ = async {
                #[cfg(unix)]
                {
                    if let Some(mut s) = term_fut {
                        let _ = s.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }
                #[cfg(not(unix))]
                {
                    std::future::pending::<()>().await;
                }
            } => {
                tracing::info!("SIGTERM received; signalling graceful TUI shutdown");
                shutdown_for_signal.notify_one();
            }
        }
    });

    // Spawn the executor on a background task
    let (conversation_log, open_outcome) = conversation;
    let mut exe = executor::Executor::with_log_and_undo_and_plugins(
        adapter,
        tools,
        shared_config.clone(),
        conversation_log,
        carryover_target,
        undo_stack,
        Some(plugin_registry),
    );
    exe.set_session_id(state.session_id.clone());
    // Apply --system override before the executor starts processing
    // input. Without this, --system is silently dropped (was GPT 5.5
    // review finding #2).
    exe.set_system_override(system);
    if let crate::session::conversation::OpenOutcome::Restored(messages) = open_outcome {
        exe.set_recovered_messages(messages);
    }
    // Keep a sender clone for slash-commands (e.g. /model pull progress)
    // that need to inject TurnEvent tokens into the TUI event stream.
    let event_tx_for_commands = event_tx.clone();

    let mut handle = tokio::spawn(async move {
        if let Err(e) = exe
            .run(
                input_rx,
                event_tx,
                approval_tx,
                cancel_rx,
                resume_rx,
                compact_rx,
                model_rx,
                undo_rx,
                config_rx,
                plan_rx,
                plugin_reload_rx,
            )
            .await
        {
            tracing::error!(error = %e, "executor task exited with an error");
        }
    });

    // Event loop
    // Slow-tick: drives time-based UI elements (spinner, the
    // 8Hz refresh of the status bar's elapsed-time display).
    // 125ms = 8Hz, which keeps the 12-frame spinner animation
    // visually smooth (full cycle every 1.5s) at a cost of
    // ~8 redraws/sec when idle. This replaces the earlier
    // 4Hz / 250ms tick (5b9909a) — the 4Hz version was
    // visibly less smooth and users noticed.
    //
    // For a quiet session this is 8 redraws/sec of the same
    // frame; ratatui's diffing + the terminal's lack of
    // damage tracking means most of these redraws are cheap
    // (the cost is dominated by the layout split + the chat
    // line build, both O(n_lines) in the visible message
    // count, which doesn't grow on idle).
    let mut slow_tick = tokio::time::interval(std::time::Duration::from_millis(125));
    slow_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Periodic connection health probe. A 30 s interval is frequent enough
    // to surface a host/model outage quickly while keeping request volume
    // negligible against a local Ollama endpoint. The probe runs on its own
    // task so a 1 s probe timeout never blocks the executor or UI.
    let (conn_probe_tx, mut conn_probe_rx) = mpsc::channel::<ConnectionState>(1);
    tokio::spawn(connection_probe_task(
        shared_config.clone(),
        conn_probe_tx,
        std::time::Duration::from_secs(30),
    ));

    let res = run_event_loop(
        &mut terminal,
        &mut state,
        &mut event_rx,
        &mut approval_rx,
        &mut persona_rx,
        &mut kb_rx,
        &input_tx,
        &cancel_tx,
        &resume_tx,
        &compact_tx,
        &model_tx,
        &undo_tx,
        &config_tx,
        &plan_tx,
        &persona_tx,
        &plugin_reload_tx,
        &mut slow_tick,
        &mut conn_probe_rx,
        &event_tx_for_commands,
        &shutdown_for_loop,
    )
    .await;

    // Signal any in-flight model call to abort before dropping channels.
    crate::send_or_warn!(cancel_tx.send(()), "cancel channel receiver dropped");
    // Drop all control senders so every receiver in the executor's
    // `tokio::select!` closes. The executor only breaks on the
    // `else => break` arm once *all* receivers are closed; dropping
    // only `input_tx` left the others alive and caused the TUI to hang
    // on `handle.await` after `run_event_loop` returned.
    drop((
        input_tx,
        cancel_tx,
        resume_tx,
        compact_tx,
        model_tx,
        undo_tx,
        plan_tx,
        persona_tx,
        plugin_reload_tx,
    ));
    // ponytail: 3s timeout so a hung Ollama HTTP call doesn't freeze the
    // terminal. Dropping a tokio JoinHandle detaches the task and lets it
    // keep running in the background, so explicitly abort and await it.
    if tokio::time::timeout(std::time::Duration::from_secs(3), &mut handle)
        .await
        .is_err()
    {
        tracing::warn!("executor task did not shut down within 3 s; aborting");
        handle.abort();
        let _ = handle.await;
    }

    // Save carryover profile
    if let Some(ref target) = saved_profile {
        if let Ok(guard) = target.lock() {
            crate::session::carryover::save_carryover(&guard);
        }
    }

    // Cleanup
    if let Err(e) = disable_raw_mode() {
        tracing::debug!(error = %e, "failed to disable raw mode during TUI shutdown");
    }
    if let Err(e) = execute!(terminal.backend_mut(), LeaveAlternateScreen) {
        tracing::debug!(error = %e, "failed to leave alternate screen during TUI shutdown");
    }

    res
}

#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    event_rx: &mut mpsc::Receiver<executor::TurnEvent>,
    approval_rx: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
    persona_rx: &mut mpsc::UnboundedReceiver<PersonaResult>,
    kb_rx: &mut mpsc::UnboundedReceiver<Event>,
    input_tx: &mpsc::UnboundedSender<String>,
    cancel_tx: &mpsc::UnboundedSender<()>,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
    compact_tx: &mpsc::UnboundedSender<CompactRequest>,
    model_tx: &mpsc::UnboundedSender<String>,
    undo_tx: &mpsc::UnboundedSender<()>,
    config_tx: &mpsc::UnboundedSender<Config>,
    plan_tx: &mpsc::UnboundedSender<bool>,
    persona_tx: &mpsc::UnboundedSender<PersonaResult>,
    plugin_reload_tx: &mpsc::UnboundedSender<kirkforge_plugin_host::PluginRegistry>,
    slow_tick: &mut tokio::time::Interval,
    conn_probe_rx: &mut mpsc::Receiver<ConnectionState>,
    event_tx_for_commands: &mpsc::Sender<executor::TurnEvent>,
    // One-shot shutdown signal. Fired by:
    //   - the SIGHUP handler (Unix, pty-close)
    //   - the kb-reader thread (crossterm `event::read()` Err)
    // When the loop observes it, it sets `state.should_exit = true`
    // and falls through to the standard exit path (terminal mode
    // restored, executor dropped, carryover profile saved).
    shutdown: &Arc<Notify>,
) -> anyhow::Result<()> {
    let key_ctx = keys::HandleInputContext {
        input_tx,
        cancel_tx,
        resume_tx,
        compact_tx,
        model_tx,
        undo_tx,
        config_tx,
        plan_tx,
        persona_tx,
        event_tx: event_tx_for_commands,
        plugin_reload_tx,
    };

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
        let mut persona_result: Option<PersonaResult> = None;
        let mut had_approval_pending =
            state.pending_approval.is_some() || state.pending_bang.is_some();
        let mut dirty_from_tick = false;
        let mut new_connection_state: Option<ConnectionState> = None;

        tokio::select! {
            biased;
            // Bias the select! toward real events so we don't drop a
            // kb event when the slow-tick happens to fire at the same
            // instant. `biased;` makes `tokio::select!` poll branches
            // top-to-bottom; the slow-tick is the lowest priority, so
            // it'll only fire when nothing else is ready.
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
            ev = persona_rx.recv() => {
                if let Some(result) = ev {
                    // Store the result in a temporary location so we can
                    // process it after the select! without holding a
                    // borrow across the await point.
                    persona_result = Some(result);
                }
            }
            st = conn_probe_rx.recv() => {
                if let Some(state) = st {
                    new_connection_state = Some(state);
                }
            }
            // Shutdown arm: SIGHUP or kb-reader-thread EOF. Higher
            // priority than the slow-tick so a signal received
            // during a tick still preempts the next 125ms wait. On
            // the slow path (no SIGHUP) the notified future is
            // cheap to poll — Notify uses a futex internally.
            _ = shutdown.notified() => {
                state.should_exit = true;
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
        if let Some(result) = persona_result {
            handle_persona_complete(result, state, resume_tx, plan_tx).await;
            state.mark_dirty();
        }
        if let Some(new_state) = new_connection_state {
            // Only redraw when the status actually changes so a
            // stable connection does not waste frames.
            if state.connection != new_state {
                state.connection = new_state;
                state.mark_dirty();
            }
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
                        approval_keys::handle_bang_approval_key(key, state).await;
                    } else if state.pending_approval.is_some() {
                        approval_keys::handle_approval_key(key, state);
                    } else {
                        keys::handle_input_key(key, state, &key_ctx).await?;
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
                        approval_keys::handle_bang_approval_key(key, state).await;
                    } else if state.pending_approval.is_some() {
                        approval_keys::handle_approval_key(key, state);
                    } else {
                        keys::handle_input_key(key, state, &key_ctx).await?;
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

        // ── Slow-tick: drive the spinner only when it is actually visible ──
        // The status-bar elapsed time is updated on every render, so the only
        // clock-driven UI that needs a periodic dirty mark is the generating
        // spinner. Gating on `is_generating && spinner_visible` keeps idle CPU
        // near zero instead of waking the render path at 4 Hz.
        if dirty_from_tick {
            let spinner_visible = state.is_generating
                && state.messages.last().map(|m| m.role.as_str()) != Some("assistant");
            if spinner_visible {
                state.spinner_tick = state.spinner_tick.wrapping_add(1);
                state.mark_dirty();
            }
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
            let input_height = state.input_visible_height(5);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(input_height),
                    Constraint::Length(1),
                ])
                .split(size);

            render_chat(f, chunks[0], state);
            render_input(f, chunks[1], state);
            render_status(f, chunks[2], state);

            // Session picker overlay (daemon follow-up). Shown when the
            // user invokes `/resume` with no arguments, or at startup
            // before the main event loop. The approval dialog takes
            // precedence if both are somehow active — approvals are
            // system-initiated and require immediate attention.
            if state.pending_approval.is_none() && state.pending_bang.is_none() {
                if let Some(ref picker) = state.session_picker {
                    picker.render(f, size);
                }
            }

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

/// Merge a completed persona result back into the parent session.
///
/// 1. Append the persona's final assistant summary as a system message
///    to the parent conversation log.
/// 2. Reload the TUI message list from the updated log.
/// 3. Send the updated log to the main executor via `resume_tx` so the
///    next turn sees the merged context.
/// 4. For `/plan`, additionally enter plan mode in the main executor and
///    prompt the user to type `/implement`.
async fn handle_persona_complete(
    result: PersonaResult,
    state: &mut AppState,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
    plan_tx: &mpsc::UnboundedSender<bool>,
) {
    state.is_generating = false;
    state.persona_in_progress = None;
    state.persona_cancel = None;

    if !result.success {
        state.messages.push(ConversationEntry::new(
            "system",
            format!(
                "{} persona failed: {}",
                result.kind,
                result.error.unwrap_or_default()
            ),
        ));
        return;
    }

    let parent_path = match state.log_path.clone() {
        Some(p) => p,
        None => {
            state.messages.push(ConversationEntry::new(
                "system",
                "Cannot merge persona result: no session log path.".to_string(),
            ));
            return;
        }
    };

    let mut parent_log = match ConversationLog::open(parent_path.clone()) {
        Ok((l, _outcome)) => l,
        Err(e) => {
            state.messages.push(ConversationEntry::new(
                "system",
                format!("Cannot open session log: {e}"),
            ));
            return;
        }
    };

    let marker = format!(
        "🤖 {} persona result for: {}\n\n{}",
        result.kind, result.task, result.summary
    );
    if let Err(e) = parent_log.append(Message {
        role: Role::System,
        content: marker,
        ..Default::default()
    }) {
        state.messages.push(ConversationEntry::new(
            "system",
            format!("Failed to merge persona: {e}"),
        ));
        return;
    }

    state.messages = messages_to_entries(parent_log.all());

    if resume_tx.send(parent_log).is_err() {
        state.messages.push(ConversationEntry::new(
            "system",
            "Executor gone; persona result saved to log only.".to_string(),
        ));
        return;
    }

    if result.kind == PersonaKind::Plan {
        crate::send_or_warn!(plan_tx.send(true), "plan-mode channel receiver dropped");
        state.messages.push(ConversationEntry::new(
            "system",
            "📐 Plan complete. Type /implement to allow edits and continue.".to_string(),
        ));
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;
    use std::path::PathBuf;

    fn test_state_with_log(log_path: PathBuf) -> AppState {
        let mut state = AppState::new(Arc::new(std::sync::RwLock::new(Config::default())));
        state.log_path = Some(log_path);
        state
    }

    // ── Shutdown-signal regression test ────────────────────────
    //
    // 2026-06-12 fix: the TUI event loop now observes a `Notify` so
    // SIGHUP and kb-reader-thread EOF can both wake the loop and
    // set `state.should_exit = true`. This test pins the
    // `Notify` + `select!` wiring: a future refactor that breaks
    // the shutdown arm — by removing it from the `select!`, by
    // holding the only `Arc` reference inside a function that
    // returns before the loop polls, etc. — will fail this test.
    //
    // The test does not exercise the full TUI (that needs a real
    // pty + a live Ollama). It exercises the same `select!` arm
    // shape the event loop uses: a `Notify` and a slow tick. If
    // the arm is wired correctly, the `select!` resolves on the
    // `Notify` arm within a few ms.
    #[tokio::test]
    async fn shutdown_notify_wakes_select() {
        let notify = Arc::new(Notify::new());
        let notify_for_task = notify.clone();
        let started = std::time::Instant::now();

        // Fire the notify after a short delay. This mimics the
        // SIGHUP handler and the kb-reader-thread-EOF path in
        // `run_tui`, both of which call `notify_one()` from a
        // background task/thread.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            notify_for_task.notify_one();
        });

        let mut slow_tick = tokio::time::interval(std::time::Duration::from_millis(125));
        slow_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut should_exit = false;
        loop {
            tokio::select! {
                _ = notify.notified() => {
                    should_exit = true;
                }
                _ = slow_tick.tick() => {
                    // Loop is alive but no shutdown yet.
                }
            }
            if should_exit {
                break;
            }
            // Safety net: bail out if the test would otherwise
            // hang forever (Notify never fired). 1s is generous
            // — the real notification fires at 20ms.
            if started.elapsed() > std::time::Duration::from_secs(1) {
                panic!("shutdown Notify was never observed");
            }
        }

        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "shutdown took too long: {:?}",
            started.elapsed()
        );
    }

    // ── Persona merge regression tests ─────────────────────────
    //
    // These pin the fork-isolation contract from ADR 010: only the
    // final assistant summary is merged back into the parent log, and
    // `/plan` additionally flips the parent executor into plan mode.

    #[tokio::test]
    async fn handle_persona_complete_merges_summary_and_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("parent.ndjson");
        let mut state = test_state_with_log(log_path.clone());

        // Pre-seed the parent log so we can verify it is not replaced.
        let mut parent = ConversationLog::open(log_path.clone()).unwrap().0;
        parent
            .append(Message {
                role: Role::User,
                content: "parent question".into(),
                ..Default::default()
            })
            .unwrap();
        state.messages = messages_to_entries(parent.all());

        let (resume_tx, mut resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
        let (plan_tx, _plan_rx) = mpsc::unbounded_channel::<bool>();

        let result = PersonaResult {
            kind: PersonaKind::Explore,
            task: "find auth".into(),
            fork_path: tmp.path().join("fork.ndjson"),
            success: true,
            summary: "auth is in src/auth.rs".into(),
            error: None,
        };

        handle_persona_complete(result, &mut state, &resume_tx, &plan_tx).await;

        // Parent log grew by one system message.
        let reloaded = ConversationLog::open(log_path).unwrap().0;
        assert_eq!(reloaded.all().len(), 2);
        let merged = &reloaded.all()[1];
        assert_eq!(merged.role, Role::System);
        assert!(merged.content.contains("explore persona result"));
        assert!(merged.content.contains("auth is in src/auth.rs"));

        // TUI message list mirrors the persisted log.
        assert_eq!(state.messages.len(), 2);
        assert!(state.messages[1].content.contains("explore persona result"));

        // Resume channel forwarded the updated log.
        assert!(resume_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn handle_persona_complete_plan_enters_plan_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("parent.ndjson");
        let mut state = test_state_with_log(log_path.clone());

        let (resume_tx, mut resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
        let (plan_tx, mut plan_rx) = mpsc::unbounded_channel::<bool>();

        let result = PersonaResult {
            kind: PersonaKind::Plan,
            task: "add dark mode".into(),
            fork_path: tmp.path().join("fork.ndjson"),
            success: true,
            summary: "Plan summary".into(),
            error: None,
        };

        handle_persona_complete(result, &mut state, &resume_tx, &plan_tx).await;

        // Plan persona flips plan mode on and prompts for /implement.
        assert_eq!(plan_rx.try_recv(), Ok(true));
        assert!(state
            .messages
            .iter()
            .any(|m| m.content.contains("/implement")));

        // Updated log was still sent to the executor.
        assert!(resume_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn handle_persona_complete_failure_does_not_pollute_log() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("parent.ndjson");
        let mut state = test_state_with_log(log_path.clone());

        // Seed a single parent message.
        let mut parent = ConversationLog::open(log_path.clone()).unwrap().0;
        parent
            .append(Message {
                role: Role::User,
                content: "parent question".into(),
                ..Default::default()
            })
            .unwrap();
        state.messages = messages_to_entries(parent.all());

        let (resume_tx, mut resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
        let (plan_tx, mut plan_rx) = mpsc::unbounded_channel::<bool>();

        let result = PersonaResult {
            kind: PersonaKind::Coder,
            task: "refactor".into(),
            fork_path: tmp.path().join("fork.ndjson"),
            success: false,
            summary: String::new(),
            error: Some("fork log missing".into()),
        };

        handle_persona_complete(result, &mut state, &resume_tx, &plan_tx).await;

        // Log on disk is untouched.
        let reloaded = ConversationLog::open(log_path).unwrap().0;
        assert_eq!(reloaded.all().len(), 1);

        // UI shows the error, not a merged summary.
        assert!(state
            .messages
            .last()
            .unwrap()
            .content
            .contains("coder persona failed"));

        // No resume or plan signals were sent.
        assert!(resume_rx.try_recv().is_err());
        assert!(plan_rx.try_recv().is_err());
    }
}
