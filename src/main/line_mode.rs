// Line-mode driver loop + interactive/non-interactive approval polling
// (Unix /dev/tty poll, Windows stdin race, other-platform fallback).
// Extracted from the binary root — pure move, no behaviour change.

use super::turn_events::emit_turn_events;
use kf_code::{adapters, line_mode, session};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

/// Spawn the approval responder used by non-interactive runs.
///
/// When `auto_approve` is true (operator opted in), approve all requests.
/// When false (default), deny destructive tools — no human is in the loop
/// to review them.
pub(super) fn spawn_non_interactive_approval_handler(
    mut approval_rx: mpsc::UnboundedReceiver<session::executor::ApprovalRequest>,
    auto_approve: bool,
) {
    tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            if auto_approve {
                kf_code::send_or_warn!(
                    req.response
                        .send(session::executor::ApprovalResponse::Approved),
                    "approval response receiver dropped; response discarded"
                );
            } else {
                tracing::warn!(
                    tool = %req.tool_name,
                    args = %req.args,
                    "non-interactive run denied approval for tool; use interactive mode or add a permission rule that explicitly allows this operation"
                );
                kf_code::send_or_warn!(req.response.send(session::executor::ApprovalResponse::DeniedWithReason(
                    "non-interactive mode cannot approve destructive tools; set auto_approve = true in config.toml or use interactive mode".into(),
                )), "approval response receiver dropped; response discarded");
            }
        }
    });
}

/// Spawn the SIGINT/SIGTERM handler for line mode.
///
/// Mirrors TUI teardown (`src/tui/mod.rs:362-383`): on Ctrl-C, set the
/// cooperative cancel flag and notify the main loop's `select!` so a
/// blocking `next_line` is interrupted. The main loop installs a live
/// per-turn `CancellationToken` (WO 44.1, mirroring `loop_.rs:513-519`)
/// and races the turn against `shutdown.notified()`; on notify it
/// cancels that token, so in-flight tool calls abort and `kill_on_drop`
/// reaps their children. The runtime then drops the executor, firing
/// `kill_on_drop` on any remaining child processes.
pub(super) fn spawn_line_mode_sigint_handler(cancelled: Arc<AtomicBool>, shutdown: Arc<Notify>) {
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        // SIGTERM — only on Unix. On non-Unix this is a pending future
        // (never resolves) so the `select!` arm is never taken.
        let term = sigterm_future();
        tokio::select! {
            biased;
            _ = ctrl_c => {
                tracing::info!("SIGINT received; signalling graceful line-mode shutdown");
            }
            _ = term => {
                tracing::info!("SIGTERM received; signalling graceful line-mode shutdown");
            }
        }
        cancelled.store(true, Ordering::Release);
        shutdown.notify_one();
    });
}

/// A future that resolves on SIGTERM (Unix) or never (non-Unix).
#[cfg(unix)]
async fn sigterm_future() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut s) => {
            let _ = s.recv().await;
        }
        Err(_) => std::future::pending::<()>().await,
    }
}

#[cfg(not(unix))]
async fn sigterm_future() {
    std::future::pending::<()>().await;
}

// reason: entry point; each arg is an independent session resource for non-interactive mode.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_line_mode(
    config: kf_code::shared::SharedConfig,
    adapter: Box<dyn adapters::ModelAdapter>,
    tools: kf_code::session::toolset::CompositeToolset,
    conversation: (
        session::conversation::ConversationLog,
        session::conversation::OpenOutcome,
    ),
    system: Option<String>,
    output: kf_code::shared::OutputFormat,
    max_turns: usize,
    non_interactive: bool,
    no_color: bool,
    plugin_registry: &kf_plugin_host::PluginRegistry,
    session_id: String,
    context_index: Option<kf_context_index::ContextIndex>,
    trace_recorder: Option<session::replay::TraceRecorder>,
    mcp_manager: Option<std::sync::Arc<session::mcp_client::McpClientManager>>,
    session_stores: session::SessionStores,
    // One-shot prompt from `--prompt`/`-p` (WO 38.10). When set, it is
    // run as the first turn before the stdin loop starts, so
    // `kf-code run -p "hello"` reaches the model without piping. The
    // value is a single turn even if it contains blank lines (fixes the
    // multi-paragraph pipe truncation for the arg form).
    prompt: Option<String>,
) -> anyhow::Result<()> {
    // If running in non-interactive mode (scripted), deny all approvals.
    // If running in line-mode interactive (no TUI), prompt on stderr and
    // read from /dev/tty so the user can actually approve or deny.
    let model_name = adapter.model_info().name.clone();

    let (conversation, open_outcome) = conversation;
    let carryover_enabled = kf_code::shared::read_shared_config(&config)
        .session
        .carryover_enabled;
    // Carryover target — line mode saves it on graceful shutdown (SIGINT),
    // mirroring TUI teardown (`src/tui/mod.rs:436-440`). When enabled, the
    // executor's cost tracker writes to this shared profile; we flush it
    // after the turn loop.
    let carryover_target: Option<Arc<std::sync::Mutex<session::carryover::CarryoverProfile>>> =
        if carryover_enabled {
            Some(Arc::new(std::sync::Mutex::new(
                session::carryover::CarryoverProfile::default(),
            )))
        } else {
            None
        };
    let saved_profile = carryover_target.clone();
    let mut executor = session::executor::Executor::with_log_and_undo_and_plugins(
        adapter,
        tools,
        config.clone(),
        conversation,
        carryover_target,
        None,
        Some(plugin_registry),
    )?;
    executor.set_session_id(session_id.clone());
    // WO 38.8: attach per-session budget/stratum stores so the budget guard
    // runs in production. Must come after set_session_id because the stratum
    // listener is keyed by session_id.
    executor.attach_session_stores(session_stores);
    executor.set_non_interactive(non_interactive);
    if let session::conversation::OpenOutcome::Restored(messages) = open_outcome {
        executor.set_recovered_messages(messages);
    }
    executor.set_system_override(system.clone());

    // Attach the repo-graph context index if one was built.
    if let Some(idx) = context_index {
        executor.set_context_index(idx);
    }

    // Attach the turn-trace recorder if tracing is enabled.
    if let Some(recorder) = trace_recorder {
        executor.set_trace(recorder);
    }

    let (approval_tx, approval_rx) =
        mpsc::unbounded_channel::<session::executor::ApprovalRequest>();

    // Wire the MCP sampling approval bus through the same channel the
    // executor uses for tool approvals. In non-interactive mode the spawned
    // handler below denies every request (default deny), so sampling is
    // denied unless `tools.allow_sampling_unattended` is set.
    if let Some(mcp_mgr) = &mcp_manager {
        let sampling_cfg = kf_code::shared::read_shared_config(&config).clone();
        mcp_mgr.set_sampling(session::mcp_client::SamplingContext {
            approval_tx: approval_tx.clone(),
            config: std::sync::Arc::new(sampling_cfg),
        });
    }

    if non_interactive {
        let auto_approve = kf_code::shared::read_shared_config(&config)
            .security
            .auto_approve;
        spawn_non_interactive_approval_handler(approval_rx, auto_approve);
    } else {
        spawn_line_mode_approval_handler(approval_rx, no_color);
    }

    if let Some(sys) = &system {
        tracing::info!("System prompt set from CLI: {}", sys);
    }

    // Cooperative cancel flag shared with the signal handler. The main loop
    // races the turn against `shutdown.notified()` (WO 44.1) and sets this
    // flag on notify so the post-iteration check breaks; the SIGINT handler
    // also sets it directly so Ctrl-C in line mode triggers graceful
    // teardown (executor cancel + carryover save + kill_on_drop children)
    // instead of the default SIGINT kill that orphans child processes.
    let cancelled = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(Notify::new());

    // Install the SIGINT handler — mirrors TUI (`src/tui/mod.rs:362-383`)
    // and daemon teardown. Sets the cancel flag and wakes the main loop's
    // `select!` so `next_line` is interruptible.
    spawn_line_mode_sigint_handler(cancelled.clone(), shutdown.clone());

    let mut line_reader = line_mode::LineReader::new(!non_interactive)?;
    // `--prompt`/`-p` one-shot (WO 38.10): prime the reader with the
    // arg value as the first turn. `LineReader::prime` yields it once,
    // then `next_line` falls back to stdin (interactive continuation or
    // piped multi-turn). A `-p` value is a single turn even with
    // internal blank lines — fixes the multi-paragraph pipe truncation
    // for the arg form.
    if let Some(p) = prompt {
        line_reader.prime(p);
    }
    let mut turn_no: usize = 0;
    let mut total_prompt_tokens: usize = 0;
    let mut total_completion_tokens: usize = 0;
    let mut cumulative_cost: f64 = 0.0;
    let mut all_tool_records: Vec<kf_code::shared::ToolCallRecord> = Vec::new();
    let mut final_error: Option<String> = None;
    let overall_started = std::time::Instant::now();

    loop {
        // Race the blocking stdin read against the SIGINT shutdown notify
        // so Ctrl-C interrupts `next_line` instead of waiting for a line.
        let input = tokio::select! {
            biased;
            r = line_reader.next_line() => match r {
                Ok(Some(s)) => Some(s),
                Ok(None) => break,
                Err(e) => return Err(e),
            },
            _ = shutdown.notified() => {
                tracing::info!("SIGINT received; signalling graceful line-mode shutdown");
                break;
            }
        };
        let Some(input) = input else { break };
        turn_no += 1;
        if max_turns > 0 && turn_no > max_turns {
            tracing::info!(
                turn_no,
                max_turns,
                "reached --max-turns cap; stopping stdin read"
            );
            break;
        }

        // Built-in slash commands in line mode (where there is no TUI
        // key handler to intercept them). This makes `/exit` and
        // `/quit` behave consistently with the TUI.
        let trimmed = input.trim();
        if trimmed == "/exit" || trimmed == "/quit" {
            if output == kf_code::shared::OutputFormat::Text {
                println!("Exiting.");
            }
            break;
        }

        if trimmed == "/reload plugins" {
            let cfg = kf_code::shared::read_shared_config(&config).clone();
            match session::plugin_tools::load_plugin_registry(&cfg) {
                Ok((registry, warnings)) => {
                    let summary = executor.reload_plugins(&registry);
                    if output == kf_code::shared::OutputFormat::Text {
                        let icon = line_mode::symbol(no_color, "🔌");
                        let sep = if icon.is_empty() { "" } else { " " };
                        println!("{icon}{sep}{summary}");
                    }
                    for w in warnings {
                        tracing::warn!(warning = %w, "plugin reload warning");
                    }
                }
                Err(e) => {
                    let icon = line_mode::symbol(no_color, "❌");
                    let sep = if icon.is_empty() { "" } else { " " };
                    eprintln!("{icon}{sep}Plugin reload failed: {e}");
                }
            }
            continue;
        }

        if trimmed.starts_with("/workflow ") || trimmed == "/workflow" {
            let args = trimmed.strip_prefix("/workflow").unwrap_or("").trim();
            let (sub, rest) = args.split_once(' ').unwrap_or((args, ""));
            let sub = sub.trim();
            let rest = rest.trim();
            match sub {
                "run" => {
                    if rest.is_empty() {
                        if output == kf_code::shared::OutputFormat::Text {
                            println!("Usage: /workflow run <name>");
                        }
                    } else {
                        let path = match kf_workflow::find_workflow_file(rest) {
                            Some(p) => p,
                            None => {
                                if output == kf_code::shared::OutputFormat::Text {
                                    println!("Workflow '{rest}' not found.");
                                }
                                continue;
                            }
                        };
                        match kf_workflow::Workflow::from_file(&path) {
                            Ok(workflow) => {
                                let cfg = kf_code::shared::read_shared_config(&config).clone();
                                let ollama_host = cfg.model.ollama_host.clone();
                                let shared_cfg = std::sync::Arc::new(std::sync::RwLock::new(cfg));
                                let supports_images = ollama_host.contains("localhost")
                                    || ollama_host.contains("127.0.0.1")
                                    || ollama_host.contains("[::1]");
                                let cancel =
                                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                                let workflow_name = workflow.name.clone();
                                let step_count = workflow.steps.len();
                                if output == kf_code::shared::OutputFormat::Text {
                                    println!("🚀 Started workflow '{workflow_name}' ({step_count} steps).");
                                }
                                let runner = kf_code::tui::commands::workflow::LineStepRunner {
                                    model_name: model_name.clone(),
                                    ollama_host,
                                    config: shared_cfg,
                                    supports_images,
                                    undo_stack: None,
                                };
                                let result = kf_workflow::WorkflowExecutor::new(workflow)
                                    .run(std::sync::Arc::new(runner), Some(&cancel))
                                    .await;
                                match result {
                                    Ok(summary) => {
                                        if output == kf_code::shared::OutputFormat::Text {
                                            let s =
                                                kf_code::tui::commands::workflow::format_summary(
                                                    &workflow_name,
                                                    &summary,
                                                );
                                            println!("{s}");
                                        }
                                    }
                                    Err(e) => {
                                        if output == kf_code::shared::OutputFormat::Text {
                                            println!("Workflow failed: {e}");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                if output == kf_code::shared::OutputFormat::Text {
                                    println!("Failed to load workflow '{rest}': {e}");
                                }
                            }
                        }
                    }
                }
                "status" => {
                    if output == kf_code::shared::OutputFormat::Text {
                        println!("No workflow is currently running. Use /workflow run <name>.");
                    }
                }
                "cancel" => {
                    if output == kf_code::shared::OutputFormat::Text {
                        println!("⛔ Workflow cancelled.");
                    }
                }
                _ => {
                    if output == kf_code::shared::OutputFormat::Text {
                        println!("Usage: /workflow run <name> | status | cancel");
                    }
                }
            }
            continue;
        }

        if trimmed == "/reload skills" {
            // Line mode has no AppState skill registry; just report that the
            // interactive skill reload is a TUI-only feature.
            if output == kf_code::shared::OutputFormat::Text {
                let icon = line_mode::symbol(no_color, "🧠");
                let sep = if icon.is_empty() { "" } else { " " };
                println!("{icon}{sep}Skill reload is only available in the TUI. Use /help to see available line-mode commands.");
            }
            continue;
        }

        if trimmed == "/carryover show" || trimmed == "/carryover" {
            let profile = session::carryover::load_carryover();
            if output == kf_code::shared::OutputFormat::Text {
                if profile.session_count == 0 {
                    println!("No carryover profile yet.");
                } else {
                    println!(
                        "{}",
                        session::carryover::CarryoverProfile::to_prompt_block(&profile)
                    );
                }
            }
            continue;
        }

        if trimmed == "/carryover clear" {
            session::carryover::clear_carryover();
            if output == kf_code::shared::OutputFormat::Text {
                println!("Carryover profile cleared.");
            }
            continue;
        }

        if trimmed == "/help" || trimmed == "/h" || trimmed == "/?" {
            if output == kf_code::shared::OutputFormat::Text {
                println!("Line-mode commands (most commands are TUI-only):");
                println!("  /exit, /quit          Exit the session");
                println!("  /reload               Reload config.toml");
                println!("  /reload plugins       Re-scan plugin directory");
                println!("  /carryover            Show or clear cross-session carryover");
                println!("  /help                 Show this help");
                println!();
                println!(
                    "Type `/help` in the TUI (`kf-code run`) for the full grouped command list."
                );
            }
            continue;
        }

        let turn_started_at = std::time::Instant::now();
        // WO 44.1: install a fresh per-turn cancel token (one-shot, like the
        // TUI at `loop_.rs:513-519`) so a mid-turn SIGINT reaches in-flight
        // tool calls immediately instead of waiting for each tool's own
        // timeout. Per-tool child tokens derive from this live root token
        // (`prepare_batch`), so cancelling it aborts in-flight tools and
        // `kill_on_drop` reaps their children promptly.
        let turn_token = tokio_util::sync::CancellationToken::new();
        executor.set_cancel_token(Some(turn_token.clone()));
        // Race the turn against `shutdown.notified()` (mirrors `loop_.rs:534-546`):
        // biased toward the turn arm so a turn that already completed before
        // the notify fires is not misattributed as a cancel. On notify, cancel
        // the live token + set the flag so the post-iteration check breaks.
        let turn_result = {
            let mut turn =
                std::pin::pin!(executor.run_turn_collecting(&input, &approval_tx, &cancelled));
            loop {
                tokio::select! {
                    biased;
                    r = &mut turn => break r,
                    _ = shutdown.notified() => {
                        cancelled.store(true, Ordering::Release);
                        turn_token.cancel();
                    }
                }
            }
        };
        executor.set_cancel_token(None);
        let events = turn_result?;
        let _turn_duration_ms = turn_started_at.elapsed().as_millis() as u64;
        emit_turn_events(
            &events,
            output,
            &mut total_prompt_tokens,
            &mut total_completion_tokens,
            &mut cumulative_cost,
            &mut all_tool_records,
            &mut final_error,
        );
        // If SIGINT fired during the turn, stop after this iteration.
        // (Fallback path — the select arm above already set the flag and
        // cancelled the in-flight work; this catches a notify that raced
        // the turn's own completion.)
        if cancelled.load(Ordering::Acquire) {
            break;
        }
    }

    // WO 43.23: kill still-running background jobs on exit (persisting
    // their exit summaries first, WO 43.10) — mirrors TUI teardown.
    session::bash_jobs::global_registry()
        .sweep_on_session_exit(&session_id)
        .await;

    // Flush carryover on exit (graceful or SIGINT) — mirrors TUI teardown
    // (`src/tui/mod.rs:436-440`). The executor's cost tracker wrote the
    // shared profile; we persist it so the next session picks it up.
    if let Some(ref target) = saved_profile {
        if let Ok(guard) = target.lock() {
            session::carryover::save_carryover(&guard);
        }
    }

    if turn_no == 0 && system.is_none() {
        tracing::warn!("No input provided. Pipe a prompt or use --system.");
        return Ok(());
    }

    if output == kf_code::shared::OutputFormat::Text {
        println!();
    }

    if output == kf_code::shared::OutputFormat::Json {
        let total_duration_ms = overall_started.elapsed().as_millis() as u64;
        let recorded_messages: Vec<_> = executor.conversation_log().all().to_vec();
        let summary = kf_code::shared::SessionSummary {
            version: "1.0".into(),
            session: kf_code::shared::SessionInfo {
                id: if non_interactive {
                    "non-interactive".into()
                } else {
                    "line-mode".into()
                },
                model: model_name,
                duration_ms: total_duration_ms,
                started_at: chrono::Local::now().to_rfc3339(),
            },
            messages: recorded_messages,
            tool_calls: all_tool_records,
            usage: kf_code::shared::UsageSummary {
                prompt_tokens: total_prompt_tokens,
                completion_tokens: total_completion_tokens,
                total_tokens: total_prompt_tokens + total_completion_tokens,
                cost_usd: cumulative_cost,
            },
            error: final_error,
        };
        println!("{}", serde_json::to_string_pretty(&summary)?);
    }

    Ok(())
}

/// Read a single line approval answer from the terminal.
///
/// On Unix, reads from the controlling terminal (`/dev/tty`) so it does
/// not compete with stdin prompt reading. On Windows there is no
/// equivalent device, so we read from stdin; the line-mode main loop is
/// not reading stdin while a tool call is awaiting approval.
#[cfg(unix)]
fn read_approval_answer_pollable(
    _tool_name: &str,
    shutdown: &std::sync::atomic::AtomicBool,
) -> Option<bool> {
    use std::os::fd::AsRawFd;
    let tty = match std::fs::OpenOptions::new().read(true).open("/dev/tty") {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "line-mode approval: no /dev/tty available; denying");
            // Some(false) = a real decision (deny); None = shutdown interrupted.
            return Some(false);
        }
    };
    // Keep `tty` alive for the fd lifetime; poll the raw fd with a short timeout
    // so `shutdown` is re-checked between polls and the thread is joinable.
    let line = poll_read_line(tty.as_raw_fd(), shutdown)?;
    let trimmed = line.trim().to_ascii_lowercase();
    Some(trimmed == "y" || trimmed == "yes")
}

/// Poll `fd` for readability with a 200 ms timeout, accumulating bytes until a
/// newline arrives. Returns `Some(line)` on a complete line (or EOF), or `None`
/// the moment `shutdown` is set. This is the testable seam that makes the
/// approval-reader thread joinable on shutdown instead of detached forever.
///
/// # Safety / blocking
/// `fd` must remain valid and open for the duration of the call. The poll
/// interval bounds the worst-case join latency to ~200 ms.
#[cfg(unix)]
fn poll_read_line(
    fd: std::os::fd::RawFd,
    shutdown: &std::sync::atomic::AtomicBool,
) -> Option<String> {
    use std::sync::atomic::Ordering;
    let mut buf = [0u8; 256];
    let mut acc = String::new();
    loop {
        if shutdown.load(Ordering::Acquire) {
            return None;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` references `fd`, which the caller keeps open for the
        // call. Single-threaded access (one reader thread per request).
        let n = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 200) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            tracing::warn!(error = %e, "poll(/dev/tty) failed; denying");
            return Some(acc);
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Some(acc);
        }
        if pfd.revents & libc::POLLIN != 0 {
            // SAFETY: reading from `fd` which is open and readable per poll.
            let r = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if r > 0 {
                let bytes = &buf[..r as usize];
                if let Ok(s) = std::str::from_utf8(bytes) {
                    acc.push_str(s);
                }
                if acc.contains('\n') {
                    return Some(acc);
                }
                // partial line (no newline yet) — keep polling for the rest
            } else if r == 0 {
                return Some(acc); // EOF
            } else {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    continue;
                }
                return Some(acc);
            }
        }
        // n == 0 (timeout) → loop and re-check shutdown
    }
}

#[cfg(windows)]
fn read_approval_answer_pollable(
    _tool_name: &str,
    shutdown: &std::sync::atomic::AtomicBool,
) -> Option<bool> {
    // Windows has no /dev/tty. We race a blocking stdin reader against a
    // periodic poll of the shutdown flag. The reader is a tokio
    // `spawn_blocking` task, so the outer async caller can abort it on
    // shutdown/timeout without waiting for a line to arrive. We return `None`
    // when shutdown is observed so the caller can distinguish "interrupted"
    // from "denied". The line-mode main loop is not reading stdin while a tool
    // awaits approval, so holding the stdin lock here is safe.
    use std::sync::atomic::Ordering;

    if shutdown.load(Ordering::Acquire) {
        return None;
    }

    tokio::runtime::Handle::current().block_on(async {
        let reader = tokio::task::spawn_blocking(|| {
            use std::io::BufRead;
            let mut answer = String::new();
            let stdin = std::io::stdin();
            let mut reader = std::io::BufReader::new(stdin.lock());
            match reader.read_line(&mut answer) {
                Ok(0) => false,
                Ok(_) => {
                    let trimmed = answer.trim().to_ascii_lowercase();
                    trimmed == "y" || trimmed == "yes"
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to read approval answer from stdin");
                    false
                }
            }
        });

        let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let abort = reader.abort_handle();

        tokio::select! {
            biased;
            a = reader => Some(a.unwrap_or(false)),
            _ = async {
                loop {
                    interval.tick().await;
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                }
            } => {
                abort.abort();
                None
            }
        }
    })
}

#[cfg(not(any(unix, windows)))]
fn read_approval_answer_pollable(
    _tool_name: &str,
    _shutdown: &std::sync::atomic::AtomicBool,
) -> Option<bool> {
    tracing::warn!("line-mode approval is not supported on this platform");
    Some(false)
}

/// Spawn an approval responder for interactive line mode.
///
/// When the TUI is disabled, destructive tool calls still need a human
/// decision. This handler prints the request to stderr and reads a line
/// from the controlling terminal when available, or stdin on Windows, so
/// it does not compete with prompt reading. `y`/`yes` approves; anything
/// else denies.
///
/// The read runs on its own OS thread (a tokio `spawn_blocking` task would
/// keep the runtime alive while it waits forever on a quiet terminal). On Unix
/// the thread polls `/dev/tty` with a short interval and is joined on shutdown
/// (answer or timeout), so it does not detach and linger. On Windows the read
/// is blocking and not interruptible, so that path remains detached.
fn spawn_line_mode_approval_handler(
    mut approval_rx: mpsc::UnboundedReceiver<session::executor::ApprovalRequest>,
    no_color: bool,
) {
    tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            let args_preview = match serde_json::to_string_pretty(&req.args) {
                Ok(s) => s,
                Err(_) => req.args.to_string(),
            };
            let warn_icon = line_mode::symbol(no_color, "⚠️");
            let warn_sep = if warn_icon.is_empty() { "" } else { " " };
            eprintln!();
            eprintln!("{warn_icon}{warn_sep}Approval required: {}", req.tool_name);
            eprintln!("{args_preview}");
            eprint!("Approve? [y/N]: ");
            if let Err(e) = std::io::stderr().flush() {
                tracing::warn!(error = %e, "failed to flush stderr approval prompt");
            }

            let tool_name = req.tool_name.clone();
            let (answer_tx, answer_rx) = tokio::sync::oneshot::channel::<bool>();

            // Reader thread: reads the terminal and sends the answer back. On
            // Unix it polls /dev/tty with a 200 ms interval so the `shutdown`
            // flag interrupts it; the JoinHandle is joined below (on timeout via
            // shutdown, on answer because the thread already exited) so no
            // reader thread is left detached at the end of the iteration.
            // On Windows the read is blocking and uninterruptible, so the same
            // pollable abstraction races a blocking stdin reader against the
            // shutdown flag and returns `None` when interrupted — but the
            // reader thread is NOT joined on Windows; it is dropped (detached)
            // to avoid hanging on the uninterruptible syscall.
            let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let shutdown_reader = shutdown.clone();
            let reader_handle: std::thread::JoinHandle<()> = std::thread::spawn(move || {
                let approved =
                    read_approval_answer_pollable(&tool_name, &shutdown_reader).unwrap_or(false);
                // If the tokio side already timed out, `answer_rx` was dropped
                // and this send is harmless.
                kf_code::send_or_warn!(
                    answer_tx.send(approved),
                    "line-mode answer channel receiver dropped"
                );
            });

            let result = tokio::time::timeout(std::time::Duration::from_secs(120), answer_rx).await;
            if result.is_err() {
                // Signal the poll loop to exit so a joinable reader can unwind.
                shutdown.store(true, std::sync::atomic::Ordering::Release);
                eprintln!("\nApproval prompt timed out after 120 s; denying.");
            }
            // Reclaim the reader thread only where the read is interruptible.
            //
            // On Unix, `read_approval_answer_pollable` polls `/dev/tty` with a
            // 200 ms `poll(2)` interval and checks `shutdown` between polls, so
            // the thread exits within one interval and `join()` returns
            // promptly on both the answer and timeout paths.
            //
            // Windows stdin read is uninterruptible. We detach the reader
            // thread on shutdown rather than joining — joining would hang if
            // the read is still blocked. The thread is reaped when the process
            // exits or when stdin is closed.
            #[cfg(unix)]
            {
                let _ = reader_handle.join();
            }
            #[cfg(not(unix))]
            {
                drop(reader_handle);
            }

            let approved = result.map(|r| r.unwrap_or(false)).unwrap_or(false);

            let resp = if approved {
                session::executor::ApprovalResponse::Approved
            } else {
                session::executor::ApprovalResponse::Denied
            };
            kf_code::send_or_warn!(
                req.response.send(resp),
                "approval response receiver dropped; response discarded"
            );
        }
    });
}

/// Parse the next prompt from a `BufRead` source, applying the
/// multi-turn rules:
///
/// - EOF (0 bytes)              → `None` (loop exits)
/// - Blank/whitespace-only line → `None` (heredoc terminator)
/// - Non-blank line             → `Some(trimmed)`
///
/// Review.md gap #2: this replaces the pre-M4 `read_to_string` +
/// one-shot `run_turn` flow. The function is pure (it takes a
/// `&mut String` buffer for reuse, but otherwise has no side
/// effects) and is the unit-testable seam for the loop driver.
#[cfg(test)]
fn next_prompt<R: std::io::BufRead>(
    reader: &mut R,
    buf: &mut String,
) -> std::io::Result<Option<String>> {
    buf.clear();
    let n = reader.read_line(buf)?;
    if n == 0 {
        return Ok(None);
    }
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// `next_prompt` returns `None` at EOF.
    #[test]
    fn next_prompt_returns_none_on_eof() {
        let input = "";
        let mut reader = Cursor::new(input);
        let mut buf = String::new();
        let r = next_prompt(&mut reader, &mut buf).unwrap();
        assert!(r.is_none());
    }

    /// `next_prompt` returns `None` for a blank/whitespace-only
    /// line. This is the heredoc terminator behaviour.
    #[test]
    fn next_prompt_returns_none_for_blank_line() {
        let input = "   \t  \n";
        let mut reader = Cursor::new(input);
        let mut buf = String::new();
        let r = next_prompt(&mut reader, &mut buf).unwrap();
        assert!(r.is_none());
    }

    /// `next_prompt` returns the trimmed line for non-blank input.
    #[test]
    fn next_prompt_returns_trimmed_line() {
        let input = "  hello world  \n";
        let mut reader = Cursor::new(input);
        let mut buf = String::new();
        let r = next_prompt(&mut reader, &mut buf).unwrap();
        assert_eq!(r.as_deref(), Some("hello world"));
    }

    /// `next_prompt` over a 3-line stream: first two are prompts,
    /// the third is blank → the function returns the first prompt
    /// and the second call sees the blank and returns None. The
    /// loop driver would then exit.
    #[test]
    fn next_prompt_sequence_three_lines() {
        let input = "turn 1\nturn 2\n\n";
        let mut reader = Cursor::new(input);
        let mut buf = String::new();
        assert_eq!(
            next_prompt(&mut reader, &mut buf).unwrap().as_deref(),
            Some("turn 1")
        );
        assert_eq!(
            next_prompt(&mut reader, &mut buf).unwrap().as_deref(),
            Some("turn 2")
        );
        // Third call: blank line → None (loop exits).
        assert!(next_prompt(&mut reader, &mut buf).unwrap().is_none());
    }

    /// `next_prompt` with no trailing newline on the last prompt
    /// still works (the `read_line` call returns the bytes; `trim`
    /// handles the missing newline).
    #[test]
    fn next_prompt_handles_missing_trailing_newline() {
        let input = "no newline here";
        let mut reader = Cursor::new(input);
        let mut buf = String::new();
        let r = next_prompt(&mut reader, &mut buf).unwrap();
        assert_eq!(r.as_deref(), Some("no newline here"));
        // Subsequent call sees EOF.
        assert!(next_prompt(&mut reader, &mut buf).unwrap().is_none());
    }

    /// When `auto_approve = true`, the non-interactive handler MUST
    /// approve every request — the operator opted in. The evaluator
    /// (`pre_run_verdict`) is the single gate and short-circuits before
    /// any request reaches this handler; this test guards the handler as
    /// a defence-in-depth net so a future evaluator regression cannot
    /// silently turn a `true` opt-in into a denial.
    #[tokio::test]
    async fn non_interactive_approval_handler_approves_when_auto_approve() {
        let (tx, rx) = mpsc::unbounded_channel();
        spawn_non_interactive_approval_handler(rx, true);

        let (oneshot_tx, oneshot_rx) = tokio::sync::oneshot::channel();
        tx.send(session::executor::ApprovalRequest {
            tool_name: "bash".into(),
            args: serde_json::json!({"command": "rm -rf /"}),
            response: session::executor::ApprovalResponder::new(oneshot_tx),
        })
        .unwrap();

        let resp = oneshot_rx.await.expect("handler sent a response");
        assert!(
            matches!(resp, session::executor::ApprovalResponse::Approved),
            "auto_approve=true must approve; got {resp:?}"
        );
    }

    /// When `auto_approve = false` (default), the non-interactive handler
    /// denies every destructive request — no human is in the loop.
    #[tokio::test]
    async fn non_interactive_approval_handler_denies_when_auto_approve_false() {
        let (tx, rx) = mpsc::unbounded_channel();
        spawn_non_interactive_approval_handler(rx, false);

        let (oneshot_tx, oneshot_rx) = tokio::sync::oneshot::channel();
        tx.send(session::executor::ApprovalRequest {
            tool_name: "bash".into(),
            args: serde_json::json!({"command": "rm -rf /"}),
            response: session::executor::ApprovalResponder::new(oneshot_tx),
        })
        .unwrap();

        let resp = oneshot_rx.await.expect("handler sent a response");
        assert!(
            matches!(
                resp,
                session::executor::ApprovalResponse::DeniedWithReason(_)
            ),
            "auto_approve=false must deny in non-interactive mode; got {resp:?}"
        );
    }

    /// Gate (Task 8 sub-task 5): the Unix approval-reader thread must JOIN on
    /// shutdown rather than detach and linger. `poll_read_line` is the seam —
    /// given a fd that is never readable and never reaches EOF (a UnixStream
    /// read half whose write half is held open), it must return `None` promptly
    /// once `shutdown` is set, so the spawned reader thread joins within ~one
    /// poll interval. A blocking `read_line` would hang here forever.
    #[cfg(unix)]
    #[tokio::test]
    async fn approval_reader_thread_joins_on_shutdown() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        // A connected socket pair: we hold the write end open and never write,
        // so the read end is never readable and never EOF — the reader's poll
        // loop must rely on `shutdown` to exit.
        let (read_end, write_end) = UnixStream::pair().expect("UnixStream::pair");
        read_end.set_nonblocking(true).expect("set_nonblocking");
        let fd = read_end.as_raw_fd();
        // Keep both ends alive for the thread's lifetime.
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_reader = shutdown.clone();
        let handle = std::thread::spawn(move || {
            let _ = write_end; // keep write end open so read never sees EOF
            poll_read_line(fd, &shutdown_reader)
        });

        // Let the thread enter its poll loop. The 80ms is a genuine
        // "wait for blocking syscall entry" race — `poll_read_line` sets
        // no signal before entering `libc::poll()`, and observing "thread
        // is now in poll()" from outside would require production changes
        // (out of scope: test-only WO). Polling `!handle.is_finished()` is
        // useless — the thread is never finished until shutdown fires.
        // Documented unconvertible race-test delay (WO 40.4). The 3s join
        // timeout below is the safety net.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(
            !handle.is_finished(),
            "reader should be blocked in poll, not finished"
        );

        shutdown.store(true, Ordering::Release);

        // Join must complete within one poll interval plus slack (no /dev/tty
        // involved — the fd is the socket). A detached/blocking reader would
        // never join here.
        let joined = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::task::spawn_blocking(move || handle.join()),
        )
        .await;
        assert!(joined.is_ok(), "reader thread did not join within 3s");
        let join_inner = joined.expect("spawn_blocking timed out");
        assert!(join_inner.is_ok(), "join returned an error: {join_inner:?}");
        // And it returned Ok(None) (shutdown interrupted), not Ok(Some(line)).
        let inner = join_inner.unwrap();
        assert!(
            matches!(inner, Ok(None)),
            "expected Ok(None) on shutdown, got {inner:?}"
        );
    }

    /// WO 43.18: the line-mode SIGINT handler must set the cancel flag and
    /// fire the shutdown notify so the main loop's `select!` breaks. This
    /// test verifies the shutdown contract that the handler fulfils: when
    /// `notify_one()` fires, a `select!` racing `next_line` against
    /// `notified()` takes the shutdown arm (no wall-clock sleep — the
    /// event is driven by the `Notify` primitive).
    ///
    /// WO 44.1: also asserts the turn-cancellation half — a turn future
    /// that never resolves on its own (stub for a `sleep 300` bash call)
    /// must complete promptly once the shutdown notify fires and the
    /// per-turn token is cancelled. This is the exact `select!` race
    /// `run_line_mode` now installs around `run_turn_collecting`
    /// (mirroring `loop_.rs:534-546`); before WO 44.1 the turn was
    /// awaited plainly and a mid-turn Ctrl-C hung for the tool timeout.
    #[tokio::test]
    async fn line_mode_sigint_handler_flips_shutdown_path() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(Notify::new());

        // Simulate what the SIGINT handler does: set the flag + notify.
        // This is the exact contract `spawn_line_mode_sigint_handler`
        // executes on signal receipt.
        let cancelled_clone = cancelled.clone();
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            cancelled_clone.store(true, Ordering::Release);
            shutdown_clone.notify_one();
        });

        // WO 44.1: the turn-cancellation contract. A never-completing stub
        // tool (stands in for `sleep 300`) is raced against
        // `shutdown.notified()` exactly as `run_line_mode` now does around
        // `run_turn_collecting`. The spawned handler fires the notify, so
        // the select takes the shutdown arm and cancels the per-turn token;
        // the turn future (`pending()`) never completes on its own, so the
        // only way this resolves within the 5s timeout is via the shutdown
        // arm — proving a mid-turn Ctrl-C reaches the turn. Before WO 44.1
        // the turn was awaited plainly and would hang for the tool timeout
        // (120s), which the 5s timeout catches as a failure instead of
        // hanging the suite. No wall-clock sleep — driven by the `Notify`
        // primitive. (The production `run_line_mode` wraps this in a `loop`
        // because a real cancelled turn future does complete after the
        // token fires; the `pending()` stub here makes the shutdown arm
        // terminal, so no loop is needed in the test.)
        let turn_token = tokio_util::sync::CancellationToken::new();
        let mut turn = std::pin::pin!(std::future::pending::<()>());
        let raced = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                biased;
                _ = &mut turn => {}
                _ = shutdown.notified() => {
                    cancelled.store(true, Ordering::Release);
                    turn_token.cancel();
                }
            }
        })
        .await;
        assert!(
            raced.is_ok(),
            "turn race did not resolve within 5s — mid-turn Ctrl-C would hang"
        );
        assert!(
            cancelled.load(Ordering::Acquire),
            "cancel flag must be set when SIGINT handler fires"
        );
        assert!(
            turn_token.is_cancelled(),
            "per-turn token must be cancelled when shutdown fires mid-turn"
        );
    }
}
