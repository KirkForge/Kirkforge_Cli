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
pub mod replay;
pub mod search;
pub mod syntax;
pub mod theme;
pub mod transcript;
pub mod widgets;

#[cfg(test)]
mod selftest;

mod connection;
#[cfg(unix)]
mod daemon_events;

use crate::session::carryover::CarryoverProfile;
use crate::session::conversation::ConversationLog;
use crate::session::executor::{self, ApprovalRequest};
use crate::session::prompt::CompactRequest;
use crate::shared::{Config, Message, Role};
use app::{AppState, ConnectionState, ConversationEntry};
use commands::{
    messages_to_entries, notify_completed_jobs, notify_completed_scheduled_jobs, PersonaKind,
    PersonaResult,
};
use components::approval::render_approval_dialog;
use connection::{connection_probe_task, probe_ollama_connection};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use events::{drain_approval_requests, drain_turn_events, handle_mouse_event};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Frame, Terminal,
};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{mpsc, Notify};
use widgets::chat::render_chat;
use widgets::input::render_input;
use widgets::status::render_status;

/// How many slow-ticks (125 ms each) the "📋 pasted" title indicator
/// stays visible after a bracketed paste before fading on its own.
const PASTE_FLASH_TICKS: u8 = 8;

/// Panic-safe guard that restores terminal state on drop.
pub(crate) struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Err(e) = disable_raw_mode() {
            tracing::warn!(error = %e, "failed to disable raw mode in terminal guard");
        }
        let mut stdout = io::stdout();
        if let Err(e) = execute!(stdout, DisableBracketedPaste) {
            tracing::warn!(error = %e, "failed to disable bracketed paste in terminal guard");
        }
        if let Err(e) = execute!(stdout, DisableMouseCapture) {
            tracing::warn!(error = %e, "failed to disable mouse capture in terminal guard");
        }
        if let Err(e) = execute!(stdout, LeaveAlternateScreen) {
            tracing::warn!(error = %e, "failed to leave alternate screen in terminal guard");
        }
    }
}

/// Show a standalone recent-session picker before the main TUI starts.
///
/// This is used by `main.rs` when the user runs `kf-code run` without
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

async fn init_app_state(
    shared_config: &crate::shared::SharedConfig,
    cfg: &Config,
    active_model: &str,
    conversation_log_path: &std::path::Path,
    undo_stack: &Option<crate::tools::UndoStackRef>,
) -> AppState {
    use crate::tui::theme::Theme;
    let mut state = AppState::new(shared_config.clone());
    state.ui.theme = Theme::from_name(&cfg.display.theme);
    state.session.undo_stack = undo_stack.clone();
    state.session.session_started = Instant::now();
    state.session.log_path = Some(conversation_log_path.to_path_buf());
    state.session.session_id = conversation_log_path
        .file_stem()
        .and_then(|f| f.to_str())
        .map(|s| s.trim_end_matches(".conv").to_string())
        .unwrap_or_else(|| "unknown-session".to_string());
    state.session.fork_manager = Some(crate::session::session_fork::ForkManager::new(
        &state.session.session_id,
        conversation_log_path,
    ));
    state.provider.connection = probe_ollama_connection(cfg, active_model).await;
    {
        let (_, path_guard, _) = crate::session::access::access_from_config(cfg);
        state.provider.unsandboxed = !path_guard.is_sandboxed();
    }
    if state.provider.unsandboxed {
        state.conversation.messages.push_back(ConversationEntry::new(
            "system",
            "⚠️  PathGuard is unsandboxed: no `sandbox_dir` or `allowed_write_dirs` configured. \
             Model-driven writes are not restricted to a directory tree. Set `sandbox_dir` in config.toml or via KF_CODE_SANDBOX_DIR, or list `allowed_write_dirs`.",
        ));
    }
    state
}

fn spawn_kb_reader(kb_tx: mpsc::UnboundedSender<Event>, shutdown: Arc<Notify>) {
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(ev) => {
                if kb_tx.send(ev).is_err() {
                    break;
                }
            }
            Err(e) => {
                tracing::info!(
                    error = ?e,
                    "keyboard reader thread exiting; signalling TUI shutdown"
                );
                shutdown.notify_one();
                break;
            }
        }
    });
}

#[cfg(unix)]
fn install_signal_handlers(
    shared_config: &crate::shared::SharedConfig,
    config_tx: &mpsc::UnboundedSender<Config>,
    shutdown: Arc<Notify>,
) {
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

    let shutdown_for_signal = shutdown.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        let term = async {
            if let Ok(mut s) = signal(SignalKind::terminate()) {
                let _ = s.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            biased;
            _ = ctrl_c => {
                tracing::info!("SIGINT received; signalling graceful TUI shutdown");
                shutdown_for_signal.notify_one();
            }
            _ = term => {
                tracing::info!("SIGTERM received; signalling graceful TUI shutdown");
                shutdown_for_signal.notify_one();
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_ctrl_c_handler(shutdown: Arc<Notify>) {
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("SIGINT received; signalling graceful TUI shutdown");
        shutdown.notify_one();
    });
}

#[allow(clippy::too_many_arguments)]
async fn teardown(
    shared_config: &crate::shared::SharedConfig,
    saved_profile: &Option<Arc<Mutex<CarryoverProfile>>>,
    cancel_tx: mpsc::UnboundedSender<()>,
    input_tx: mpsc::UnboundedSender<String>,
    resume_tx: mpsc::UnboundedSender<ConversationLog>,
    compact_tx: mpsc::UnboundedSender<CompactRequest>,
    model_tx: mpsc::UnboundedSender<String>,
    undo_tx: mpsc::UnboundedSender<()>,
    plan_tx: mpsc::UnboundedSender<bool>,
    persona_tx: mpsc::UnboundedSender<PersonaResult>,
    plugin_reload_tx: mpsc::UnboundedSender<kf_plugin_host::PluginRegistry>,
    handle: &mut tokio::task::JoinHandle<()>,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) {
    crate::send_or_warn!(cancel_tx.send(()), "cancel channel receiver dropped");
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
    const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 3;
    let shutdown_secs = crate::shared::read_shared_config(shared_config)
        .session
        .shutdown_timeout_secs
        .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_SECS);
    if tokio::time::timeout(std::time::Duration::from_secs(shutdown_secs), &mut *handle)
        .await
        .is_err()
    {
        tracing::warn!("executor task did not shut down within 3 s; aborting");
        handle.abort();
        let _ = handle.await;
    }
    if let Some(ref target) = saved_profile {
        if let Ok(guard) = target.lock() {
            crate::session::carryover::save_carryover(&guard);
        }
    }
    if let Err(e) = disable_raw_mode() {
        tracing::warn!(error = %e, "failed to disable raw mode during TUI shutdown");
    }
    if let Err(e) = execute!(terminal.backend_mut(), DisableBracketedPaste) {
        tracing::warn!(error = %e, "failed to disable bracketed paste during TUI shutdown");
    }
    if let Err(e) = execute!(terminal.backend_mut(), DisableMouseCapture) {
        tracing::warn!(error = %e, "failed to disable mouse capture during TUI shutdown");
    }
    if let Err(e) = execute!(terminal.backend_mut(), LeaveAlternateScreen) {
        tracing::warn!(error = %e, "failed to leave alternate screen during TUI shutdown");
    }
}

fn spawn_plugin_watcher(
    shared_config: &crate::shared::SharedConfig,
    reload_tx: mpsc::UnboundedSender<kf_plugin_host::PluginRegistry>,
) {
    let (watch_tx, mut watch_rx) = mpsc::unbounded_channel::<()>();
    let plugins_dir = crate::session::plugin_tools::plugins_dir();
    let _watcher = crate::session::plugin_tools::spawn_plugin_watcher(plugins_dir, watch_tx);
    let watch_cfg = shared_config.clone();
    tokio::spawn(async move {
        while watch_rx.recv().await.is_some() {
            let cfg = crate::shared::read_shared_config(&watch_cfg).clone();
            match crate::session::plugin_tools::load_plugin_registry(&cfg) {
                Ok((registry, warnings)) => {
                    for w in &warnings {
                        tracing::warn!(warning = %w, "plugin hot-reload warning");
                    }
                    let _ = reload_tx.send(registry);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "plugin hot-reload failed");
                }
            }
        }
    });
}

#[cfg(unix)]
async fn spawn_daemon_reader(state: &mut AppState) {
    let daemon_flags = std::sync::Arc::new(std::sync::Mutex::new(
        crate::tui::daemon_events::DaemonEventFlags::default(),
    ));
    state.session.daemon_flags = Some(daemon_flags.clone());
    match crate::tui::daemon_events::spawn_daemon_event_reader(daemon_flags).await {
        Ok(Some(handle)) => {
            tracing::trace!("daemon instance channel connected");
            tokio::spawn(async move {
                let _ = handle.await;
            });
        }
        Ok(None) => {
            tracing::info!("daemon not running; instance channel not opened");
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to open daemon instance channel");
        }
    }
}

/// Run the TUI event loop.
// reason: entry point; each arg is an independent session resource that the loop owns.
#[allow(clippy::too_many_arguments)]
pub async fn run_tui(
    shared_config: crate::shared::SharedConfig,
    adapter: Box<dyn crate::adapters::ModelAdapter>,
    tools: crate::session::toolset::CompositeToolset,
    conversation: (ConversationLog, crate::session::conversation::OpenOutcome),
    system: Option<String>,
    undo_stack: Option<crate::tools::UndoStackRef>,
    plugin_registry: &kf_plugin_host::PluginRegistry,
    context_index: Option<kf_context_index::ContextIndex>,
    trace_recorder: Option<crate::session::replay::TraceRecorder>,
    mcp_manager: Option<std::sync::Arc<crate::session::mcp_client::McpClientManager>>,
) -> anyhow::Result<()> {
    // ── Terminal setup ──
    // Read the startup config before terminal init so the mouse-capture
    // gate (`display.mouse_enabled`) is honored on the very first frame.
    let cfg_for_startup = crate::shared::read_shared_config(&shared_config).clone();
    let active_model = adapter.model_info().name.clone();
    let mouse_enabled = cfg_for_startup.display.mouse_enabled;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    if mouse_enabled {
        execute!(stdout, EnableMouseCapture)?;
    }
    // Bracketed paste is independent of mouse capture and always desirable:
    // it lets the loop distinguish a paste from typed keystrokes (WO 30.0.11).
    execute!(stdout, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let _guard = TerminalGuard;

    let mut state = init_app_state(
        &shared_config,
        &cfg_for_startup,
        &active_model,
        conversation.0.path(),
        &undo_stack,
    )
    .await;

    let max_trust = cfg_for_startup.tools.max_plugin_trust;
    state
        .services
        .skill_registry
        .set_max_plugin_trust(max_trust);
    if let Err(e) = state
        .services
        .skill_registry
        .scan_and_load(&cfg_for_startup)
    {
        tracing::warn!("Skill scan error: {}", e);
    }
    for skill in crate::session::skills::builtin_skills() {
        state.services.skill_registry.register(skill);
    }
    state.provider.plugin_status = state.services.skill_registry.plugin_status_summary();

    let carryover_target: Option<Arc<Mutex<CarryoverProfile>>> =
        if cfg_for_startup.session.carryover_enabled {
            Some(Arc::new(Mutex::new(CarryoverProfile::default())))
        } else {
            None
        };
    let saved_profile = carryover_target.clone();

    let (input_tx, input_rx) = mpsc::unbounded_channel::<String>();
    let (event_tx, mut event_rx) = mpsc::channel::<executor::TurnEvent>(10_000);
    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

    // Wire the MCP sampling approval bus: incoming `sampling/createMessage`
    // requests route through the same approval channel as tool calls.
    if let Some(mcp_mgr) = &mcp_manager {
        let sampling_cfg = crate::shared::read_shared_config(&shared_config).clone();
        mcp_mgr.set_sampling(crate::session::mcp_client::SamplingContext {
            approval_tx: approval_tx.clone(),
            config: std::sync::Arc::new(sampling_cfg),
        });
    }

    let (cancel_tx, cancel_rx) = mpsc::unbounded_channel::<()>();
    let (resume_tx, resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
    let (compact_tx, compact_rx) = mpsc::unbounded_channel::<CompactRequest>();
    let (model_tx, model_rx) = mpsc::unbounded_channel::<String>();
    let (undo_tx, undo_rx) = mpsc::unbounded_channel::<()>();
    let (config_tx, config_rx) = mpsc::unbounded_channel::<Config>();
    let (plan_tx, plan_rx) = mpsc::unbounded_channel::<bool>();
    let (plugin_reload_tx, plugin_reload_rx) =
        mpsc::unbounded_channel::<kf_plugin_host::PluginRegistry>();

    spawn_plugin_watcher(&shared_config, plugin_reload_tx.clone());

    let (persona_tx, mut persona_rx) = mpsc::unbounded_channel::<PersonaResult>();
    let (kb_tx, mut kb_rx) = mpsc::unbounded_channel::<Event>();

    let shutdown = Arc::new(Notify::new());
    let shutdown_for_loop = shutdown.clone();
    spawn_kb_reader(kb_tx, shutdown.clone());

    #[cfg(unix)]
    install_signal_handlers(&shared_config, &config_tx, shutdown.clone());
    #[cfg(not(unix))]
    spawn_ctrl_c_handler(shutdown.clone());

    // Spawn the executor on a background task
    let (conversation_log, open_outcome) = conversation;
    let event_tx_for_commands = event_tx.clone();
    let mut handle = spawn_executor(
        adapter,
        tools,
        shared_config.clone(),
        conversation_log,
        open_outcome,
        carryover_target,
        undo_stack,
        plugin_registry,
        &state,
        system,
        context_index,
        trace_recorder,
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
    );

    // Slow-tick: drives time-based UI elements (spinner, 8Hz status bar).
    let mut slow_tick = tokio::time::interval(std::time::Duration::from_millis(125));
    slow_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let (conn_probe_tx, mut conn_probe_rx) = mpsc::channel::<ConnectionState>(1);
    tokio::spawn(connection_probe_task(
        shared_config.clone(),
        conn_probe_tx,
        std::time::Duration::from_secs(30),
    ));

    #[cfg(unix)]
    spawn_daemon_reader(&mut state).await;

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

    teardown(
        &shared_config,
        &saved_profile,
        cancel_tx,
        input_tx,
        resume_tx,
        compact_tx,
        model_tx,
        undo_tx,
        plan_tx,
        persona_tx,
        plugin_reload_tx,
        &mut handle,
        &mut terminal,
    )
    .await;

    res
}

#[allow(clippy::too_many_arguments)]
fn spawn_executor(
    adapter: Box<dyn crate::adapters::ModelAdapter>,
    tools: crate::session::toolset::CompositeToolset,
    shared_config: crate::shared::SharedConfig,
    conversation_log: ConversationLog,
    open_outcome: crate::session::conversation::OpenOutcome,
    carryover_target: Option<Arc<Mutex<CarryoverProfile>>>,
    undo_stack: Option<crate::tools::UndoStackRef>,
    plugin_registry: &kf_plugin_host::PluginRegistry,
    state: &AppState,
    system: Option<String>,
    context_index: Option<kf_context_index::ContextIndex>,
    trace_recorder: Option<crate::session::replay::TraceRecorder>,
    input_rx: mpsc::UnboundedReceiver<String>,
    event_tx: mpsc::Sender<executor::TurnEvent>,
    approval_tx: mpsc::UnboundedSender<ApprovalRequest>,
    cancel_rx: mpsc::UnboundedReceiver<()>,
    resume_rx: mpsc::UnboundedReceiver<ConversationLog>,
    compact_rx: mpsc::UnboundedReceiver<CompactRequest>,
    model_rx: mpsc::UnboundedReceiver<String>,
    undo_rx: mpsc::UnboundedReceiver<()>,
    config_rx: mpsc::UnboundedReceiver<Config>,
    plan_rx: mpsc::UnboundedReceiver<bool>,
    plugin_reload_rx: mpsc::UnboundedReceiver<kf_plugin_host::PluginRegistry>,
) -> tokio::task::JoinHandle<()> {
    let mut exe = executor::Executor::with_log_and_undo_and_plugins(
        adapter,
        tools,
        shared_config,
        conversation_log,
        carryover_target,
        undo_stack,
        Some(plugin_registry),
    )
    .expect("executor construction failed");
    // ponytail: wo/20.3.0 changed Executor::new/with_log_and_undo_and_plugins
    // to return Result for sandbox-config validation. spawn_executor returns
    // JoinHandle<()>, so we expect() here instead of propagating. Upgrade path:
    // change spawn_executor to return Result<JoinHandle<()>> and propagate
    // through run_tui (the audit's X1/X4 sandbox refusal surface).
    exe.set_session_id(state.session.session_id.clone());
    exe.set_system_override(system);
    if let Some(idx) = context_index {
        exe.set_context_index(idx);
    }
    if let crate::session::conversation::OpenOutcome::Restored(messages) = open_outcome {
        exe.set_recovered_messages(messages);
    }
    if let Some(recorder) = trace_recorder {
        exe.set_trace(recorder);
    }
    tokio::spawn(async move {
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
    })
}

// reason: each arg is a distinct mpsc channel end; grouping would obscure the wiring.
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
    plugin_reload_tx: &mpsc::UnboundedSender<kf_plugin_host::PluginRegistry>,
    slow_tick: &mut tokio::time::Interval,
    conn_probe_rx: &mut mpsc::Receiver<ConnectionState>,
    event_tx_for_commands: &mpsc::Sender<executor::TurnEvent>,
    // One-shot shutdown signal. Fired by:
    //   - the SIGHUP handler (Unix, pty-close)
    //   - the kb-reader thread (crossterm `event::read()` Err)
    // When the loop observes it, it sets `state.session.should_exit = true`
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
        if state.session.should_exit {
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
        let mut first_executor_event: Option<executor::TurnEvent> = None;
        let mut first_approval_event: Option<ApprovalRequest> = None;
        let mut persona_result: Option<PersonaResult> = None;
        let mut had_approval_pending =
            state.approval.pending_approval.is_some() || state.approval.pending_bang.is_some();
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
                first_executor_event = ev;
            }
            ev = approval_rx.recv() => {
                first_approval_event = ev;
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
                state.session.should_exit = true;
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
        //
        // The event consumed by `select!` is dispatched FIRST, then
        // the remaining queue is drained via `try_recv`. The prior
        // code stored only a boolean and dropped the selected event,
        // which lost the first chunk of every burst (and every token
        // in slow-stream scenarios).
        if first_executor_event.is_some() {
            drain_turn_events(state, first_executor_event, event_rx);
            state.mark_dirty();
        }
        if first_approval_event.is_some() {
            drain_approval_requests(state, first_approval_event, approval_rx);
            state.mark_dirty();
        }
        if let Some(result) = persona_result {
            handle_persona_complete(result, state, resume_tx, plan_tx).await;
            state.mark_dirty();
        }
        if let Some(new_state) = new_connection_state {
            // Only redraw when the status actually changes so a
            // stable connection does not waste frames.
            if state.provider.connection != new_state {
                state.provider.connection = new_state;
                state.mark_dirty();
            }
        }

        // Jobs and kb events are also work that may have been
        // waiting. We always drain jobs (cheap) and process any
        // kb event we just got. If nothing happened, none of this
        // marks the state dirty.
        drain_daemon_flags(state);
        if state.session.sessions_dirty {
            state.session.sessions_dirty = false;
            crate::tui::commands::refresh_sessions(state).await;
            state.mark_dirty();
        }
        if state.session.jobs_dirty {
            state.session.jobs_dirty = false;
            let out = crate::tui::commands::refresh_jobs_output(state).await;
            state.session.cached_jobs_output = Some(out);
            state.mark_dirty();
        }
        if notify_completed_jobs(state).await {
            state.mark_dirty();
        }
        if notify_completed_scheduled_jobs(state).await {
            state.mark_dirty();
        }

        dispatch_kb_events(state, &key_ctx, kb_event, kb_rx).await?;

        // ── Approval dialog appeared mid-iteration ─────────────
        // The drain functions above set `state.approval.pending_approval` /
        // `state.approval.pending_bang` if a new approval arrived. Track
        // this so the next render (even if it would otherwise be
        // skipped) draws the dialog. Mirrors the v1 behavior of
        // always rendering so the dialog appears immediately.
        if state.approval.pending_approval.is_some() || state.approval.pending_bang.is_some() {
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
            let spinner_visible = state.generation.is_generating
                && state.conversation.messages.back().map(|m| m.role.as_str()) != Some("assistant");
            let tool_streaming = state
                .conversation
                .messages
                .back()
                .map(|m| m.role == "tool" && m.streaming)
                .unwrap_or(false);
            if spinner_visible || tool_streaming {
                state.generation.spinner_tick = state.generation.spinner_tick.wrapping_add(1);
                state.mark_dirty();
            }
            // Fade the "📋 pasted" title indicator: decrement each slow-tick
            // (125 ms) and keep rendering until it expires (WO 30.0.11).
            if state.ui.paste_flash > 0 {
                state.ui.paste_flash -= 1;
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

        render_frame(terminal, state)?;
    }
}

async fn dispatch_kb_events<'a>(
    state: &mut AppState,
    key_ctx: &keys::HandleInputContext<'a>,
    first: Option<Event>,
    kb_rx: &mut mpsc::UnboundedReceiver<Event>,
) -> anyhow::Result<()> {
    async fn dispatch_one<'a>(
        state: &mut AppState,
        key: event::KeyEvent,
        key_ctx: &keys::HandleInputContext<'a>,
    ) -> anyhow::Result<()> {
        // Any keystroke dismisses the "📋 pasted" title indicator (WO 30.0.11).
        state.ui.paste_flash = 0;
        if state.approval.pending_bang.is_some() {
            approval_keys::handle_bang_approval_key(key, state).await;
        } else if state.approval.pending_approval.is_some() {
            approval_keys::handle_approval_key(key, state);
        } else {
            keys::handle_input_key(key, state, key_ctx).await?;
        }
        Ok(())
    }

    if let Some(ev) = first {
        match ev {
            Event::Key(key) => dispatch_one(state, key, key_ctx).await?,
            Event::Paste(content) => {
                state.apply_paste(&content);
                state.ui.paste_flash = PASTE_FLASH_TICKS;
                state.mark_dirty();
            }
            Event::Mouse(mouse) => handle_mouse_event(state, mouse),
            Event::Resize(_w, _h) => state.mark_dirty(),
            _ => {}
        }
    }

    while let Ok(ev) = kb_rx.try_recv() {
        match ev {
            Event::Key(key) => dispatch_one(state, key, key_ctx).await?,
            Event::Paste(content) => {
                state.apply_paste(&content);
                state.ui.paste_flash = PASTE_FLASH_TICKS;
                state.mark_dirty();
            }
            Event::Mouse(mouse) => handle_mouse_event(state, mouse),
            Event::Resize(_w, _h) => state.mark_dirty(),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
fn drain_daemon_flags(state: &mut AppState) {
    // Drain the shared flags set by the daemon event reader into
    // the local AppState. The reader sets the flags; we clear
    // them after mirroring so we never miss an event.
    let (sessions_flag, jobs_flag) = if let Some(ref flags) = state.session.daemon_flags {
        if let Ok(mut f) = flags.lock() {
            let s = f.sessions_dirty;
            let j = f.jobs_dirty;
            f.sessions_dirty = false;
            f.jobs_dirty = false;
            (s, j)
        } else {
            (false, false)
        }
    } else {
        (false, false)
    };
    if sessions_flag {
        state.session.sessions_dirty = true;
        state.mark_dirty();
    }
    if jobs_flag {
        state.session.jobs_dirty = true;
        state.mark_dirty();
    }
}

#[cfg(not(unix))]
fn drain_daemon_flags(_state: &mut AppState) {}

fn render_frame(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    terminal.draw(|f| render_app(f, state))?;
    Ok(())
}

/// The full TUI render pipeline: tab bar, main content (chat / models /
/// plugins / jobs / settings / threads), slash menu, file completer, input
/// bar, status bar, session picker overlay, approval dialog, and doom-loop
/// banner.
///
/// Extracted from `render_frame`'s closure so the selftest harness can drive
/// the EXACT same layout against a `TestBackend` (WO 31.6). `render_frame`
/// remains the production entry point (it owns the `terminal.draw` call and
/// the `CrosstermBackend` type); this function is the backend-agnostic core.
pub(crate) fn render_app(f: &mut Frame, state: &mut AppState) {
    let size = f.area();
    // Input box content width ≈ terminal width minus the two border columns.
    // Used so a long line wraps and the box grows to fit (WO 30.0.12).
    let content_width = size.width.saturating_sub(2) as usize;
    let input_height = state.input_visible_height(5, content_width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(1),    // main content
            Constraint::Length(input_height),
            Constraint::Length(1), // status bar
        ])
        .split(size);

    // ── Top tab bar: F1–F6 labels, active tab highlighted ──
    crate::tui::widgets::tabs::render_tab_bar(f, chunks[0], state);

    // Render main content area based on active tab.
    // Chat (F1) shows the conversation; other tabs show their
    // own panel content in the same area.
    use crate::tui::app::ActiveTab;
    match state.ui.active_tab {
        ActiveTab::Chat => {
            // Welcome screen when no messages and no input
            if state.conversation.messages.is_empty() && state.conversation.input.is_empty() {
                crate::tui::widgets::welcome::render_welcome(f, chunks[1], state);
            } else {
                render_chat(f, chunks[1], state);
            }
        }
        ActiveTab::Models => {
            crate::tui::widgets::tabs::render_models(f, chunks[1], state);
        }
        ActiveTab::Plugins => {
            crate::tui::widgets::tabs::render_plugins(f, chunks[1], state);
        }
        ActiveTab::Jobs => {
            crate::tui::widgets::tabs::render_jobs(f, chunks[1], state);
        }
        ActiveTab::Settings => {
            crate::tui::widgets::tabs::render_settings(f, chunks[1], state);
        }
        ActiveTab::Sessions => {
            crate::tui::widgets::tabs::render_sessions(f, chunks[1], state);
        }
    }

    // ── Slash menu popup (above input) ──
    if let Some(ref menu) = state.ui.slash_menu {
        crate::tui::widgets::slash_menu::render_slash_menu(f, chunks[2], menu);
    }

    // ── File completer popup (above input) ──
    if let Some(ref completer) = state.ui.file_completer {
        crate::tui::widgets::file_completer::render_file_completer(f, chunks[2], completer);
    }

    // Show input and status for all tabs; the main content area
    // already rendered above.
    render_input(f, chunks[2], state);
    // Stash the input rect so the mouse handler can hit-test clicks
    // against the input box (WO 32.12).
    state.ui.last_input_rect = Some(chunks[2]);
    render_status(f, chunks[3], state);

    // Session picker overlay (daemon follow-up). Shown when the
    // user invokes `/resume` with no arguments, or at startup
    // before the main event loop. The approval dialog takes
    // precedence if both are somehow active — approvals are
    // system-initiated and require immediate attention.
    if state.approval.pending_approval.is_none() && state.approval.pending_bang.is_none() {
        if let Some(ref picker) = state.session.session_picker {
            picker.render(f, size);
        }
    }

    // Directory picker overlay (Ctrl+O) is rendered by the
    // file_completer above — it uses FileCompleter with
    // pick_directory=true instead of a separate widget.

    // Approval dialog overlay.
    //
    // `render_approval_dialog` needs both a `&PendingApproval` (to
    // display the args preview) and `&mut state` (to clamp
    // `state.approval.approval_scroll` / `state.approval.approval_max_scroll`). We
    // can't hold both borrows simultaneously because the immutable
    // borrow of `state.approval.pending_approval` would extend through the
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
    let pending_taken = state.approval.pending_approval.take();
    if let Some(ref approval) = pending_taken {
        render_approval_dialog(f, size, approval, state);
    } else if let Some(ref bang) = state.approval.pending_bang {
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
    state.approval.pending_approval = pending_taken;

    // Doom-loop warning banner. Renders last so it sits on top
    // of any other overlay. Skipped when acknowledged or when
    // the underlying state hasn't crossed the threshold.
    crate::tui::widgets::doom_banner::render_if_active(f, size, state);
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
    state.generation.is_generating = false;
    state.generation.persona_in_progress = None;
    state.generation.persona_cancel = None;

    if result.task.starts_with("workflow ") {
        state.generation.workflow_in_progress = None;
        state.generation.workflow_cancel = None;
        let msg = if result.success {
            result.summary
        } else {
            format!("Workflow failed: {}", result.error.unwrap_or_default())
        };
        state
            .conversation
            .messages
            .push_back(ConversationEntry::new("system", msg));
        return;
    }

    if !result.success {
        state
            .conversation
            .messages
            .push_back(ConversationEntry::new(
                "system",
                format!(
                    "{} persona failed: {}",
                    result.kind,
                    result.error.unwrap_or_default()
                ),
            ));
        return;
    }

    let parent_path = match state.session.log_path.clone() {
        Some(p) => p,
        None => {
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new(
                    "system",
                    "Cannot merge persona result: no session log path.".to_string(),
                ));
            return;
        }
    };

    let mut parent_log = match ConversationLog::open(parent_path.clone()) {
        Ok((l, _outcome)) => l,
        Err(e) => {
            state
                .conversation
                .messages
                .push_back(ConversationEntry::new(
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
        state
            .conversation
            .messages
            .push_back(ConversationEntry::new(
                "system",
                format!("Failed to merge persona: {e}"),
            ));
        return;
    }

    state.conversation.messages = messages_to_entries(parent_log.all());

    if resume_tx.send(parent_log).is_err() {
        state
            .conversation
            .messages
            .push_back(ConversationEntry::new(
                "system",
                "Executor gone; persona result saved to log only.".to_string(),
            ));
        return;
    }

    if result.kind == PersonaKind::Plan {
        crate::send_or_warn!(plan_tx.send(true), "plan-mode channel receiver dropped");
        state
            .conversation
            .messages
            .push_back(ConversationEntry::new(
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
/// knobs (deny_paths, etc.) would be noisy and
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
    use crate::shared::test_util::app_state_with_log;
    use std::path::PathBuf;

    fn test_state_with_log(log_path: PathBuf) -> AppState {
        app_state_with_log(log_path)
    }

    // ── Shutdown-signal regression test ────────────────────────
    //
    // 2026-06-12 fix: the TUI event loop now observes a `Notify` so
    // SIGHUP and kb-reader-thread EOF can both wake the loop and
    // set `state.session.should_exit = true`. This test pins the
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
        state.conversation.messages = messages_to_entries(parent.all());

        let (resume_tx, mut resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
        let (plan_tx, _plan_rx) = mpsc::unbounded_channel::<bool>();

        let result = PersonaResult {
            kind: PersonaKind::Explore,
            task: "find auth".into(),
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
        assert_eq!(state.conversation.messages.len(), 2);
        assert!(state.conversation.messages[1]
            .content
            .contains("explore persona result"));

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
            success: true,
            summary: "Plan summary".into(),
            error: None,
        };

        handle_persona_complete(result, &mut state, &resume_tx, &plan_tx).await;

        // Plan persona flips plan mode on and prompts for /implement.
        assert_eq!(plan_rx.try_recv(), Ok(true));
        assert!(state
            .conversation
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
        state.conversation.messages = messages_to_entries(parent.all());

        let (resume_tx, mut resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
        let (plan_tx, mut plan_rx) = mpsc::unbounded_channel::<bool>();

        let result = PersonaResult {
            kind: PersonaKind::Coder,
            task: "refactor".into(),
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
            .conversation
            .messages
            .back()
            .unwrap()
            .content
            .contains("coder persona failed"));

        // No resume or plan signals were sent.
        assert!(resume_rx.try_recv().is_err());
        assert!(plan_rx.try_recv().is_err());
    }
}
