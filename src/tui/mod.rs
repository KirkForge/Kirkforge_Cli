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
    messages_to_entries, notify_completed_jobs, notify_completed_scheduled_jobs, should_poll,
    PersonaKind, PersonaResult,
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
        // Best-effort raw ANSI reset. The crossterm commands above may
        // fail when the terminal is already in a corrupted state (the
        // "failed to disable raw mode / bracketed paste / mouse / alt
        // screen" log spam). Writing raw escape sequences directly to
        // stdout does not depend on crossterm tracking mode state, so
        // it works even when the crossterm-layer is confused. This is
        // the symptom-side fix for the "terminal in a corrupted state"
        // reports: the root cause (kb-reader panic, approval dialog
        // crash) is fixed in bugs 1+2, but this ensures the user's
        // terminal is usable even if something else corrupts it later.
        force_terminal_reset(&mut io::stdout());
    }
}

/// Best-effort terminal reset via raw ANSI escape sequences.
///
/// Writes directly to the writer (no crossterm command layer), so it
/// works even when the terminal is in a corrupted state and the
/// crossterm `disable_raw_mode` / `LeaveAlternateScreen` / etc.
/// commands fail. The sequences:
///   - `\x1b[?2004l` — disable bracketed paste
///   - `\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l` — disable mouse
///   - `\x1b[?1049l` — leave alternate screen (xterm)
///   - `\x1b[?47l`   — leave alternate screen (vt100 fallback)
///   - `\x1b[?1l`    — disable cursor-key application mode
///   - `\x1b[0m`     — reset all text attributes (color, bold, etc.)
///   - `\x1b[?25h`   — show cursor
///   - `\x1b[2J\x1b[H` — clear screen + home cursor (visible screen)
///
/// Each sequence is written independently so a partial write still
/// resets what it can. Errors are swallowed (best-effort — there is
/// nothing useful to do with a write error at this point).
fn force_terminal_reset<W: io::Write>(w: &mut W) {
    // Disable bracketed paste.
    let _ = w.write_all(b"\x1b[?2004l");
    // Disable mouse capture (all the modes crossterm might have enabled).
    let _ = w.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l");
    // Leave alternate screen (xterm + vt100 fallback).
    let _ = w.write_all(b"\x1b[?1049l\x1b[?47l");
    // Disable cursor-key application mode.
    let _ = w.write_all(b"\x1b[?1l");
    // Reset all text attributes.
    let _ = w.write_all(b"\x1b[0m");
    // Show cursor.
    let _ = w.write_all(b"\x1b[?25h");
    // Clear the visible screen and home the cursor so the user is not
    // left looking at the last render's artifacts.
    let _ = w.write_all(b"\x1b[2J\x1b[H");
    let _ = w.flush();
}

static PANIC_HOOK_ONCE: std::sync::Once = std::sync::Once::new();

/// Install the terminal-restoring panic hook. Must run BEFORE
/// `enable_raw_mode()` so it is active for every later panic.
///
/// Why a hook and not just `TerminalGuard`: the release profile uses
/// `panic = "abort"`, and a Drop guard never runs on the abort path —
/// but the panic hook does (the runtime invokes it, then aborts). The
/// hook mirrors what `TerminalGuard::drop` does: disable raw mode and
/// write the raw reset sequences, so the user's terminal survives any
/// panic in the shipped binary.
pub fn install_panic_hook() {
    PANIC_HOOK_ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(panic_hook_with(previous, io::stdout()));
    });
}

/// Build the panic hook body over an injectable writer.
///
/// Split from `install_panic_hook` so tests can drive it with an
/// in-memory buffer: reset the terminal FIRST, then chain to the
/// previous hook so the panic message is printed on a clean screen
/// (message AFTER reset, not before).
fn panic_hook_with<W: io::Write + Send + Sync + 'static>(
    previous: Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static>,
    w: W,
) -> Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync + 'static> {
    let w = Mutex::new(w);
    Box::new(move |info| {
        let _ = disable_raw_mode();
        // Mutex so the boxed `Fn` can take `&mut` to the writer.
        let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
        force_terminal_reset(&mut *w);
        previous(info);
    })
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

    install_panic_hook();
    enable_raw_mode()?;
    // Guard must exist before any fallible terminal setup so its drop
    // restores raw mode even when EnterAlternateScreen/Terminal::new
    // fails (EPIPE, pty teardown race). Drop is idempotent (mod.rs:86).
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

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
    std::thread::spawn(move || {
        // crossterm's `event::read()` can return transient errors
        // (resize race, EAGAIN on some platforms, a write to stdout
        // that briefly holds the terminal lock). The prior code shut
        // down the TUI on the FIRST error — so a single transient
        // read failure mid-tool-execution (right after an approval
        // response, when the render path was writing heavily to
        // stdout) yeeted the user out of the session. Retry up to
        // `MAX_CONSECUTIVE_READ_ERRORS` times before giving up; a
        // real EOF (the pty closed) returns Err repeatedly, so we
        // still exit promptly on a genuine terminal teardown.
        const MAX_CONSECUTIVE_READ_ERRORS: u32 = 3;
        let mut consecutive_errors = 0u32;
        loop {
            match event::read() {
                Ok(ev) => {
                    consecutive_errors = 0;
                    if kb_tx.send(ev).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;
                    tracing::info!(
                        error = ?e,
                        consecutive_errors,
                        "keyboard reader thread got a read error; retrying"
                    );
                    if should_shutdown_after_errors(consecutive_errors, MAX_CONSECUTIVE_READ_ERRORS)
                    {
                        tracing::info!(
                            error = ?e,
                            "keyboard reader thread exiting after {consecutive_errors} \
                             consecutive errors; signalling TUI shutdown"
                        );
                        shutdown.notify_one();
                        break;
                    }
                    // Brief backoff so a tight error loop doesn't spin
                    // the CPU. 10ms is short enough that the user does
                    // not notice a hiccup, long enough to let a
                    // transient condition clear.
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    });
}

/// Decide whether the kb-reader thread should shut down after a run of
/// consecutive `event::read()` errors.
///
/// Pure function (no I/O) so the retry contract is unit-testable without
/// a real terminal / pty. Returns `true` once `consecutive` reaches
/// `max` — i.e. the threshold is the shutdown trigger. A successful read
/// resets the counter (handled by the caller). This is the root-cause
/// fix for the "yeeted on approval" bug: the prior code shut down on
/// the FIRST error, so a single transient crossterm read failure
/// mid-tool-execution killed the session.
fn should_shutdown_after_errors(consecutive: u32, max: u32) -> bool {
    consecutive >= max
}

// WO 44.36: surface executor-task death. A closed `event_rx` means the
// executor task exited (propagated error logged to kf-code.log). Push a
// system entry so the user sees why the session is quitting — the
// alternate screen hides the log file. Returns true so the caller sets
// `should_exit` and falls through to the standard teardown path. Pure
// over `AppState` so the contract is unit-testable without a live TUI.
fn handle_executor_channel_closed(state: &mut AppState) -> bool {
    state
        .conversation
        .messages
        .push_back(ConversationEntry::new(
            "system",
            "⚠️ Session executor exited unexpectedly (see kf-code.log for the error). Exiting.",
        ));
    state.mark_dirty();
    true
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
    // Best-effort raw ANSI reset as a fallback. The crossterm commands
    // above may fail when the terminal is already corrupted (the
    // "failed to disable..." log spam). Raw escape sequences write
    // directly to the backend, bypassing crossterm's state tracking,
    // so they work even when the crossterm-layer is confused. This is
    // the symptom-side fix for "terminal in a corrupted state" — the
    // root cause (kb-reader panic, approval crash) is fixed in bugs
    // 1+2, but this guarantees the user's terminal is usable on exit
    // regardless of how the corruption happened.
    force_terminal_reset(terminal.backend_mut());
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
    session_stores: crate::session::SessionStores,
) -> anyhow::Result<()> {
    // ── Terminal setup ──
    // Read the startup config before terminal init so the mouse-capture
    // gate (`display.mouse_enabled`) is honored on the very first frame.
    let cfg_for_startup = crate::shared::read_shared_config(&shared_config).clone();
    let active_model = adapter.model_info().name.clone();
    let mouse_enabled = cfg_for_startup.display.mouse_enabled;

    install_panic_hook();
    enable_raw_mode()?;
    // Guard must exist before any fallible terminal setup so its drop
    // restores raw mode even when EnterAlternateScreen/mouse/paste/
    // Terminal::new fails (EPIPE, pty teardown race). Drop is
    // idempotent and writes disable/reset sequences unconditionally
    // (mod.rs:86-150), so creating it before those modes are enabled is safe.
    let _guard = TerminalGuard;
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
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<crate::tui::commands::BgCmdDone>();
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
    // WO 38.6: surface StartedEmpty in the TUI with a banner naming the
    // corrupt original path, mirroring the Restored banner emitted via
    // TurnEvent::Recovered. The pre-TUI eprintln from run_session is lost
    // when the alternate screen takes over, so the TUI needs its own notice.
    let started_empty_banner = matches!(
        open_outcome,
        crate::session::conversation::OpenOutcome::StartedEmpty
    )
    .then(|| state.session.log_path.clone());
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
        session_stores,
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

    if let Some(path) = started_empty_banner {
        let display = path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(path unknown)".to_string());
        state.conversation.messages.push_back(ConversationEntry::new(
            "system",
            format!("⚠️ Session log was corrupt and had no usable checkpoint; started a new empty session. The corrupt original was left in place at {display} for manual recovery."),
        ));
    }

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
        &bg_tx,
        &mut bg_rx,
        &mut slow_tick,
        &mut conn_probe_rx,
        &event_tx_for_commands,
        &shutdown_for_loop,
    )
    .await;

    // WO 43.23: kill still-running background jobs on session teardown
    // (persisting their exit summaries first, WO 43.10) so a normal
    // quit leaves no live orphaned process groups behind.
    crate::session::bash_jobs::global_registry()
        .sweep_on_session_exit(&state.session.session_id)
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
    session_stores: crate::session::SessionStores,
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
    // WO 45.1: stamp the canonical run_id on the global bash job registry
    // so background jobs spawned by this session carry it. Idempotent.
    crate::session::bash_jobs::global_registry().set_run_id(state.session.session_id.clone());
    // WO 38.8: attach the per-session budget/stratum stores to the executor
    // so the budget guard runs in production. Must come after set_session_id
    // because the stratum listener is keyed by session_id.
    exe.attach_session_stores(session_stores);
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
    // WO 38.3: background slash-command completions (gh / jobs run-now /
    // commit --push / test / bang). Handlers spawn their work and report
    // through `bg_tx`; the loop drains `bg_rx` and re-renders.
    bg_tx: &mpsc::UnboundedSender<crate::tui::commands::BgCmdDone>,
    bg_rx: &mut mpsc::UnboundedReceiver<crate::tui::commands::BgCmdDone>,
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
        bg_tx,
    };

    // WO 44.37: throttle the scheduled-job completion poll. Without this
    // `notify_completed_scheduled_jobs` did a synchronous read_dir +
    // per-job JSON parse on every event-loop iteration (8×/s idle). The
    // daemon push path (`NotifyJobsChanged` → `jobs_dirty`) is the primary
    // driver; this poll is a fallback, so 1 Hz is plenty. `None` means
    // "never polled" → the first iteration always runs (covers jobs that
    // finished before the TUI started).
    // ponytail: ceiling — if job counts grow large, move the poll to
    // spawn_blocking instead of widening the interval.
    let mut last_scheduled_poll: Option<Instant> = None;

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
        let mut bg_event: Option<crate::tui::commands::BgCmdDone> = None;
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
                // WO 44.36: a closed event channel means the executor
                // task has exited (e.g. conversation-log IO error at
                // loop_.rs:480-487). `recv() == None` is permanent —
                // with `biased;` polling this arm would win every
                // iteration and busy-spin at 100% CPU, dropping all
                // input silently. Treat it as fatal: surface a system
                // entry and fall through to the standard exit path
                // (terminal restored, carryover saved), the same path
                // the shutdown arm below uses.
                if let Some(ev) = ev {
                    first_executor_event = Some(ev);
                } else if handle_executor_channel_closed(state) {
                    state.session.should_exit = true;
                }
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
            ev = bg_rx.recv() => {
                bg_event = ev;
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
        if let Some(done) = bg_event {
            apply_bg_result(state, done);
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
        // WO 44.37: throttle the scheduled-job poll to 1 Hz (see
        // `last_scheduled_poll` above). The in-memory bash-job poll just
        // above is cheap and stays unthrottled.
        if should_poll(last_scheduled_poll, Instant::now()) {
            if notify_completed_scheduled_jobs(state).await {
                state.mark_dirty();
            }
            last_scheduled_poll = Some(Instant::now());
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
        // Doom-loop banner takes key precedence over the approval
        // dialog when unacknowledged. The banner is rendered ON TOP of
        // the approval dialog (z-order: doom last in `render_app`), so
        // keys must route to the banner too — otherwise Esc dismisses
        // the approval (denying the tool call) instead of the banner,
        // and arrow keys scroll the approval preview instead of moving
        // the banner highlight. The banner handler no-ops when the
        // banner is acknowledged or absent, so this is safe to run
        // unconditionally (WO 38.11).
        if doom_banner_is_active(state) {
            keys::handle_input_key(key, state, key_ctx).await?;
            return Ok(());
        }
        if state.approval.pending_bang.is_some() {
            approval_keys::handle_bang_approval_key(key, state, key_ctx.bg_tx).await;
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

/// True when the doom-loop banner is both present and unacknowledged —
/// i.e. it is the topmost overlay and should capture keys. Mirrors the
/// render predicate in `doom_banner::render_if_active` so key routing
/// and z-order agree (WO 38.11).
fn doom_banner_is_active(state: &AppState) -> bool {
    state.doom.doom_loop.as_ref().is_some_and(|dl| {
        dl.count >= crate::session::executor::DoomLoopTracker::THRESHOLD && !dl.acknowledged
    })
}

fn render_frame(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    terminal.draw(|f| render_app(f, state))?;
    Ok(())
}

/// The full TUI render pipeline: header, chat surface (permanent),
/// overlay panels on top of chat, slash menu, file completer, input
/// bar, status bar, session picker overlay, approval dialog, command
/// palette, and doom-loop banner.
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
            Constraint::Length(1), // header
            Constraint::Min(1),    // main content (chat)
            Constraint::Length(input_height),
            Constraint::Length(1), // status bar
        ])
        .split(size);

    // ── Top header: app name + model + ready/busy (replaces tab bar, WO 34.1) ──
    crate::tui::widgets::tabs::render_header(f, chunks[0], state);

    // Render the chat surface as the permanent primary content. Overlays
    // (Models/Plugins/Jobs/Settings/Threads) render ON TOP of the chat
    // when active — see the overlay block below. Chat is always visible
    // underneath so the conversation never disappears.
    use crate::tui::app::ActiveTab;
    // Welcome screen when no messages and no input
    if state.conversation.messages.is_empty() && state.conversation.input.is_empty() {
        crate::tui::widgets::welcome::render_welcome(f, chunks[1], state);
    } else {
        render_chat(f, chunks[1], state);
    }

    // ── Overlay panels (former tabs) render on top of the chat ──
    // ponytail: overlays currently render in the main content area
    // (replacing the chat view), matching the pre-34.1 behavior. True
    // overlay-on-top-of-chat (chat stays visible underneath) is the
    // WO 34.1 step-5 goal; deferred because centered-popup layout over
    // a live chat surface needs a Clear+popup composition pass that
    // interacts with the approval-dialog / doom-banner z-ordering.
    // Ceiling: overlays still hide the chat while open. Upgrade path:
    // render overlays into a centered Rect via Layout inside chunks[1]
    // (or a right-docked pane), preceded by Clear so the chat shows
    // through the borders. Tracked in WO 34.1 step 5 / state.md pending.
    match state.ui.active_tab {
        ActiveTab::None | ActiveTab::Chat => {}
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

    // Command palette (Ctrl+K). Renders as a centered overlay on top of
    // the chat surface but under the doom banner (the banner is the
    // topmost layer so a doom-loop warning is never hidden).
    if state.ui.command_palette_visible {
        crate::tui::widgets::command_palette::render_command_palette(
            f,
            size,
            &state.ui.command_palette_query,
            state.ui.command_palette_selected,
        );
    }

    // ── /help overlay (WO 34.2) ────────────────────────────────
    // Drawn after the approval dialog (approvals take precedence) and
    // before the doom-loop banner (doom stays on top of everything).
    if state.ui.help_overlay_visible {
        crate::tui::widgets::help_overlay::render_help_overlay(f, size, state);
    }

    // Doom-loop warning banner. Renders last so it sits on top
    // of any other overlay. Skipped when acknowledged or when
    // the underlying state hasn't crossed the threshold.
    crate::tui::widgets::doom_banner::render_if_active(f, size, state);
}

/// Apply a background slash-command completion (WO 38.3): append the
/// entry to the conversation and clear the `/test` in-flight flag when
/// the spawned run was the test suite. Called by the event loop for
/// every `BgCmdDone` drained off the channel.
pub(crate) fn apply_bg_result(state: &mut AppState, done: crate::tui::commands::BgCmdDone) {
    if done.test_finished {
        state.generation.test_in_progress = false;
    }
    state.conversation.messages.push_back(done.entry);
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
        state.generation.workflow_orchestrator = None;
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

    fn test_state() -> AppState {
        crate::shared::test_util::app_state()
    }

    // ── Bug 2: kb-reader retry contract ────────────────────────────
    //
    // The prior `spawn_kb_reader` shut down the TUI on the FIRST
    // `event::read()` error. crossterm can return transient errors
    // (resize race, EAGAIN, a stdout write holding the terminal lock),
    // so a single hiccup mid-tool-execution yeeted the user out of
    // the session. The fix retries up to `MAX_CONSECUTIVE_READ_ERRORS`
    // (3) consecutive errors before shutting down. These tests pin
    // the retry-decision contract via the pure
    // `should_shutdown_after_errors` helper.

    /// A single transient error (1 of 3) does NOT shut down — the
    /// whole point of the fix. The prior code shut down here.
    #[test]
    fn kb_reader_single_error_does_not_shut_down() {
        assert!(
            !should_shutdown_after_errors(1, 3),
            "a single transient read error must not shut down the TUI"
        );
    }

    /// Two consecutive errors still retry — under the threshold.
    #[test]
    fn kb_reader_two_errors_still_retries() {
        assert!(
            !should_shutdown_after_errors(2, 3),
            "two consecutive errors must still retry (threshold is 3)"
        );
    }

    /// Three consecutive errors shut down — a genuine EOF (pty
    /// closed) returns Err repeatedly, so we exit promptly.
    #[test]
    fn kb_reader_three_errors_shut_down() {
        assert!(
            should_shutdown_after_errors(3, 3),
            "three consecutive errors must shut down (genuine terminal teardown)"
        );
    }

    /// The threshold is inclusive: `consecutive >= max` triggers
    /// shutdown. Guards against an off-by-one (e.g. `> max` would
    /// require 4 errors, letting one more transient through than
    /// intended).
    #[test]
    fn kb_reader_threshold_is_inclusive() {
        assert!(!should_shutdown_after_errors(2, 3));
        assert!(should_shutdown_after_errors(3, 3));
        assert!(should_shutdown_after_errors(4, 3));
    }

    /// Zero errors never shuts down (the happy path).
    #[test]
    fn kb_reader_zero_errors_never_shuts_down() {
        assert!(!should_shutdown_after_errors(0, 3));
    }

    // ── Bug 3: best-effort terminal reset ──────────────────────────
    //
    // When the terminal is already corrupted, the crossterm cleanup
    // commands (disable_raw_mode, LeaveAlternateScreen, etc.) can
    // fail. `force_terminal_reset` writes raw ANSI escape sequences
    // directly to the writer, bypassing crossterm's state tracking,
    // so it works even when crossterm is confused. These tests pin
    // the contract: it writes the reset sequences, and it is
    // best-effort (does not panic on a closed writer).

    /// `force_terminal_reset` writes the key reset sequences to the
    /// output. We capture them and assert each is present. This pins
    /// the contract: disable bracketed paste, disable mouse, leave
    /// alt screen, reset attributes, show cursor, clear screen.
    #[test]
    fn force_terminal_reset_writes_ansi_escape_sequences() {
        let mut buf = Vec::new();
        force_terminal_reset(&mut buf);
        let written = String::from_utf8(buf).expect("reset writes valid UTF-8 (ANSI is ASCII)");
        // Disable bracketed paste.
        assert!(
            written.contains("\x1b[?2004l"),
            "missing disable-bracketed-paste; got: {written:?}"
        );
        // Disable mouse capture (at least one of the mouse modes).
        assert!(
            written.contains("\x1b[?1000l"),
            "missing disable-mouse; got: {written:?}"
        );
        // Leave alternate screen (xterm).
        assert!(
            written.contains("\x1b[?1049l"),
            "missing leave-alt-screen; got: {written:?}"
        );
        // Reset all text attributes.
        assert!(
            written.contains("\x1b[0m"),
            "missing reset-attributes; got: {written:?}"
        );
        // Show cursor.
        assert!(
            written.contains("\x1b[?25h"),
            "missing show-cursor; got: {written:?}"
        );
        // Clear screen + home cursor.
        assert!(
            written.contains("\x1b[2J"),
            "missing clear-screen; got: {written:?}"
        );
        assert!(
            written.contains("\x1b[H"),
            "missing home-cursor; got: {written:?}"
        );
    }

    /// `force_terminal_reset` is best-effort: a closed/broken writer
    /// does NOT cause a panic. This is the whole point — the function
    /// is called during shutdown when the terminal may already be in
    /// a bad state, so it must never propagate an error.
    #[test]
    fn force_terminal_reset_does_not_panic_on_closed_writer() {
        // A writer that always errors (simulates a closed pty / broken
        // terminal). The function must swallow the error and return.
        struct BrokenWriter;
        impl io::Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken"))
            }
        }
        let mut w = BrokenWriter;
        // Must not panic.
        force_terminal_reset(&mut w);
    }

    // ── Panic hook (WO 38.2) ────────────────────────────────────────
    //
    // The release profile uses panic="abort", so TerminalGuard::drop
    // never runs on a panic — the hook installed by
    // `install_panic_hook` is the only thing standing between a panic
    // and a corrupted terminal. The contract: reset sequences are
    // written FIRST, then the previous hook reports the panic.

    /// Writer adapter over a shared buffer so the hook's reset output
    /// and the chained "previous hook" output land in one ordered
    /// stream we can assert on.
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl io::Write for SharedBuf {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The hook resets the terminal BEFORE the chained hook reports
    /// the panic — the message must land on a clean cooked-mode
    /// screen, not inside a corrupted raw-mode alt-screen.
    #[test]
    fn panic_hook_resets_terminal_before_reporting() {
        use std::io::Write as _;

        let buf = Arc::new(Mutex::new(Vec::new()));
        // Fake "previous hook" that writes a marker into the shared
        // buffer; ordering between marker and reset sequences is the
        // assertion target.
        let marker_buf = Arc::clone(&buf);
        let previous: Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync> =
            Box::new(move |_info| {
                let mut w = SharedBuf(Arc::clone(&marker_buf));
                let _ = w.write_all(b"<PANIC-REPORTED>");
            });

        let prior = std::panic::take_hook();
        std::panic::set_hook(panic_hook_with(previous, SharedBuf(Arc::clone(&buf))));
        let caught = std::panic::catch_unwind(|| panic!("hook-test-boom"));
        std::panic::set_hook(prior);

        assert!(caught.is_err(), "catch_unwind must catch the test panic");
        let written = String::from_utf8(buf.lock().unwrap().clone())
            .expect("hook output is valid UTF-8 (ANSI is ASCII)");
        let report_at = written
            .find("<PANIC-REPORTED>")
            .expect("chained previous hook must run");
        let before_report = &written[..report_at];
        assert!(
            before_report.contains("\x1b[?1049l"),
            "leave-alt-screen must precede the panic report; got: {written:?}"
        );
        assert!(
            before_report.contains("\x1b[?25h"),
            "show-cursor must precede the panic report; got: {written:?}"
        );
        assert!(
            before_report.contains("\x1b[2J"),
            "clear-screen must precede the panic report; got: {written:?}"
        );
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

        // Fire the notify immediately. The 20ms wall-clock sleep this
        // replaced was artificial — the test proves the `select!` resolves
        // on the Notify arm, not that there's a delay. The 1s safety net
        // and 500ms assertion ceiling below still bound the test.
        tokio::spawn(async move {
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
            // hang forever (Notify never fired).
            if started.elapsed() > std::time::Duration::from_secs(1) {
                panic!("shutdown Notify was never observed");
            }
        }

        // The 1s hang guard above already bounds the worst case. The
        // redundant 500ms elapsed bound was a second wall-clock margin
        // that added flake surface without catching a distinct failure
        // mode — the should_exit==true assert is the real correctness
        // check; the hang guard is the safety net.
        assert!(should_exit, "shutdown Notify was never observed");
    }

    // ── WO 44.36: closed event channel must not busy-spin ────────
    //
    // When the executor task exits, `event_tx` drops and `event_rx.recv()`
    // returns `Ready(None)` permanently. With `biased;` select polling
    // the `event_rx.recv()` arm wins every iteration, the body no-ops
    // (drain is gated on `is_some()`), and the loop spins at 100% CPU —
    // UI looks alive but silently drops all input. This test documents
    // the spin hazard by reproducing the select shape: a closed
    // `mpsc::Receiver::recv()` arm must resolve immediately, NOT wait
    // for the slow tick. A regression that re-introduces the no-op
    // handling (e.g. storing `None` and looping) would make this test
    // hang until the 1s safety net trips.
    #[tokio::test]
    async fn closed_event_rx_resolves_immediately() {
        let (_tx, mut rx) = mpsc::channel::<()>(1);
        // Drop the sender so `recv()` returns `None` permanently.
        drop(_tx);

        let mut slow_tick = tokio::time::interval(std::time::Duration::from_millis(125));
        slow_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let started = std::time::Instant::now();
        let mut saw_closed = false;
        while !saw_closed {
            tokio::select! {
                biased;
                ev = rx.recv() => {
                    // None = channel closed = executor task exited.
                    // The loop MUST take this arm, not the tick arm.
                    assert!(ev.is_none(), "expected closed channel, got a value");
                    saw_closed = true;
                }
                _ = slow_tick.tick() => {
                    // If the closed arm ever loses to the tick, the
                    // spin bug is back. Fail fast.
                    panic!("closed event_rx arm lost to slow_tick (busy-spin regression)");
                }
            }
            // Safety net: bail out if the test would otherwise hang.
            if started.elapsed() > std::time::Duration::from_secs(1) {
                panic!("closed event_rx was never observed");
            }
        }
        assert!(saw_closed, "closed event_rx was never observed");
    }

    // WO 44.36: the decision helper pushes a visible system entry and
    // signals exit so the loop falls through to the standard teardown
    // path (terminal restored, carryover saved). Pure over AppState so
    // it tests without a live TUI.
    #[test]
    fn handle_executor_channel_closed_marks_exit() {
        let mut state = test_state();
        let before = state.conversation.messages.len();

        // Mirror the select-arm call site: helper returns the decision,
        // caller sets should_exit and breaks to the standard teardown.
        let should_exit = handle_executor_channel_closed(&mut state);
        if should_exit {
            state.session.should_exit = true;
        }

        assert!(
            should_exit,
            "helper must signal should_exit on closed channel"
        );
        assert!(
            state.session.should_exit,
            "caller must set should_exit from the return value"
        );
        assert_eq!(state.conversation.messages.len(), before + 1);
        let entry = state.conversation.messages.back().unwrap();
        assert_eq!(entry.role, "system");
        assert!(
            entry.content.contains("executor exited unexpectedly"),
            "system entry should explain the exit; got: {}",
            entry.content
        );
        assert!(
            entry.content.contains("kf-code.log"),
            "system entry should point at the log file; got: {}",
            entry.content
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

    // WO 38.3: a background-completion event appends its entry and, for
    // /test completions, clears the in-flight flag the event loop uses
    // to gate concurrent runs (state flag set at dispatch time).
    #[tokio::test]
    async fn apply_bg_result_appends_entry_and_clears_test_flag() {
        let mut state = test_state();

        state.generation.test_in_progress = true;
        apply_bg_result(
            &mut state,
            crate::tui::commands::BgCmdDone {
                entry: crate::tui::app::ConversationEntry::new("system", "tests done"),
                test_finished: true,
            },
        );
        assert!(!state.generation.test_in_progress);
        assert_eq!(state.conversation.messages.len(), 1);
        assert_eq!(state.conversation.messages[0].content, "tests done");

        apply_bg_result(
            &mut state,
            crate::tui::commands::BgCmdDone::system("gh done"),
        );
        // Non-test completions leave the flag alone.
        assert!(!state.generation.test_in_progress);
        assert_eq!(state.conversation.messages.len(), 2);
        assert_eq!(state.conversation.messages[1].role, "system");
    }
}
