mod adapters;
mod daemon;
mod session;
mod shared;
mod tools;
mod tui;

use clap::{Parser, Subcommand};
use kirkforge_plugin::TrustTier;
use kirkforge_plugin_host::TrustPolicy;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(name = "kirkforge", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Run {
        #[arg(short, long)]
        model: Option<String>,

        #[arg(long)]
        host: Option<String>,

        #[arg(long)]
        model_type: Option<String>,

        #[arg(long)]
        auto_approve: bool,

        #[arg(short, long)]
        system: Option<String>,

        #[arg(short, long)]
        resume: Option<String>,

        #[arg(long)]
        non_interactive: bool,

        #[arg(long, default_value = "text")]
        output: crate::shared::OutputFormat,

        /// Cap on the number of turns in non-interactive mode. Each
        /// non-empty line on stdin is one turn. 0 = unlimited (run
        /// until EOF or a blank line). Defaults to 0. Review.md
        /// gap #2: the previous one-shot read-and-exit made it
        /// impossible to script multi-turn sessions.
        #[arg(long, default_value_t = 0)]
        max_turns: usize,

        /// Resume a prior session by id prefix (or full path). When
        /// set, the existing `*.conv.ndjson` is reopened and the new
        /// turns are appended to it. Path is preferred if the value
        /// contains a `/`; otherwise it's treated as a session-id
        /// prefix and resolved via `session::session_index`.
        #[arg(long)]
        continue_session: Option<String>,

        /// Resume the most recent session via the session daemon.
        /// If the daemon is not running, falls back to creating a new session.
        #[arg(long, conflicts_with = "continue_session", conflicts_with = "resume")]
        auto_resume: bool,

        /// Resume a specific recent session by id or prefix via the daemon.
        #[arg(
            long,
            conflicts_with = "continue_session",
            conflicts_with = "resume",
            conflicts_with = "auto_resume"
        )]
        attach: Option<String>,
    },
    /// Run the background session daemon.
    Daemon {
        /// Stay in the foreground instead of detaching.
        #[arg(long)]
        foreground: bool,

        /// Stop a running daemon.
        #[arg(long, conflicts_with = "foreground")]
        stop: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            model,
            host,
            model_type,
            auto_approve,
            system,
            resume,
            non_interactive,
            output,
            max_turns,
            continue_session,
            auto_resume,
            attach,
        } => {
            run_session(RunArgs {
                model,
                host,
                model_type,
                auto_approve,
                system,
                resume,
                non_interactive,
                output,
                max_turns,
                continue_session,
                auto_resume,
                attach,
            })
            .await
        }
        Command::Daemon { foreground, stop } => daemon::server::run_daemon(foreground, stop).await,
    }
}

struct RunArgs {
    model: Option<String>,
    host: Option<String>,
    model_type: Option<String>,
    auto_approve: bool,
    system: Option<String>,
    resume: Option<String>,
    non_interactive: bool,
    output: crate::shared::OutputFormat,
    max_turns: usize,
    continue_session: Option<String>,
    auto_resume: bool,
    attach: Option<String>,
}

async fn run_session(args: RunArgs) -> anyhow::Result<()> {
    let RunArgs {
        model,
        host,
        model_type,
        auto_approve,
        system,
        resume,
        non_interactive,
        output,
        max_turns,
        continue_session,
        auto_resume,
        attach,
    } = args;

    let mut config = session::config::load_or_create_config();

    if let Some(host) = &host {
        config.ollama_host = host.clone();
    }
    let model = model.unwrap_or_else(|| config.default_model.clone());
    if auto_approve {
        config.auto_approve = true;
    }

    if let Err(e) = session::config::save_config(&config) {
        tracing::warn!(error = %e, "failed to persist updated config");
    }

    // Resolve the launch-time cwd exactly once, then freeze it on the
    // Config. Review.md arch concern #3: previously, `Config::default()`
    // did this resolution, which (a) ran before any validation, and
    // (b) allowed a deletion-after-launch race to silently widen the
    // sandbox to `None`. `freeze_launch_sandbox` is the new single
    // resolution site: resolves `current_dir()` once, captures the
    // value, and honors the operator's explicit-escape-hatch policy.
    let _frozen_cwd = session::config::freeze_launch_sandbox(&mut config);

    let ollama_host = &config.ollama_host;

    let data_dir = session::data_dir()?;
    std::fs::create_dir_all(&data_dir)?;

    let session_id = session::new_session_id();

    // Resolve the log path. Priority order:
    //   1. `--continue-session <value>` — id prefix OR full path
    //   2. `--resume <path>`            — legacy path-only flag
    //   3. `--attach <id-or-prefix>`    — via session daemon
    //   4. `--auto-resume`              — most recent session via daemon
    //   5. TUI startup picker (if daemon has recent sessions)
    //   6. brand-new session id
    let log_path = if let Some(cont) = &continue_session {
        resolve_continue_path(cont)?
    } else if let Some(resume) = &resume {
        std::path::PathBuf::from(resume)
    } else if let Some(id) = &attach {
        match daemon::client::try_resolve_id(id).await? {
            Some(path) => path,
            None => {
                anyhow::bail!(
                    "daemon could not resolve session '{}'. Run `/sessions` to see available ids.",
                    id
                );
            }
        }
    } else if auto_resume {
        match daemon::client::try_resolve_recent().await? {
            Some(path) => {
                tracing::info!(path = %path.display(), "auto-resuming most recent session");
                path
            }
            None => {
                tracing::info!("no recent sessions found; starting a new session");
                let sessions_dir = data_dir.join("sessions");
                std::fs::create_dir_all(&sessions_dir)?;
                sessions_dir.join(format!("{}.conv.ndjson", session_id))
            }
        }
    } else {
        // Try the daemon for a startup picker in TUI mode, or a hint in
        // non-interactive mode.
        match daemon::client::try_list_recent().await? {
            Some(sessions) if !sessions.is_empty() && !non_interactive => {
                match tui::run_session_picker(sessions).await? {
                    Some(path) => {
                        tracing::info!(path = %path.display(), "resuming selected session");
                        path
                    }
                    None => {
                        tracing::info!("user chose new session");
                        let sessions_dir = data_dir.join("sessions");
                        std::fs::create_dir_all(&sessions_dir)?;
                        sessions_dir.join(format!("{}.conv.ndjson", session_id))
                    }
                }
            }
            Some(sessions) if !sessions.is_empty() => {
                print_recent_sessions_hint(&sessions);
                let sessions_dir = data_dir.join("sessions");
                std::fs::create_dir_all(&sessions_dir)?;
                sessions_dir.join(format!("{}.conv.ndjson", session_id))
            }
            _ => {
                let sessions_dir = data_dir.join("sessions");
                std::fs::create_dir_all(&sessions_dir)?;
                sessions_dir.join(format!("{}.conv.ndjson", session_id))
            }
        }
    };

    // Tell the daemon this session is now active.
    let touch_id = log_path
        .file_stem()
        .and_then(|f| f.to_str())
        .map(|s| s.trim_end_matches(".conv").to_string())
        .unwrap_or_else(|| session_id.to_string());
    daemon::client::try_touch(&touch_id, log_path.clone()).await;

    let conversation = session::conversation::ConversationLog::open(log_path)?;

    let adapter = adapters::adapter_for(&model, ollama_host, model_type.as_deref());

    // ── Undo stack (review.md gap #7) ──
    // Per-session edit undo. Constructed here so the EditFile and
    // WriteFile tools can capture pre-edit bytes for `/undo`.
    // Wrapped in `Arc<Mutex<_>>` because the executor and the TUI's
    // `/undo` handler both touch it. The critical sections are tiny
    // (push a snapshot, pop a file) so contention is not a concern.
    //
    // We log a warning and proceed without undo if the data dir
    // can't be resolved — better than refusing the edit.
    let undo_stack = match session::undo::UndoStack::for_session(&session_id.to_string()) {
        Ok(s) => Some(std::sync::Arc::new(std::sync::Mutex::new(s))),
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = ?e,
                "could not open undo stack — edits will not be undoable this session"
            );
            None
        }
    };

    // ── Toolset assembly (Phase 2.2) ──
    // Compose built-in, MCP, and plugin tools into a single source-aware
    // collection. The executor receives the flattened vector, but order and
    // duplicate-name resolution are controlled here: built-ins win over MCP,
    // and MCP wins over plugins.
    let mut toolset = session::toolset::CompositeToolset::empty();
    toolset.add(Box::new(session::toolset::VecToolset::new(
        "builtin",
        tools::all_tools(undo_stack.clone(), adapter.model_info().supports_images),
    )));

    // ── Shared config (hot-reload foundation) ──
    // Wrap the launch-time Config in an Arc<RwLock> so both TUI and
    // executor can observe live updates from SIGHUP or `/reload`.
    let shared_config = std::sync::Arc::new(std::sync::RwLock::new(config));

    // --- MCP tools ---
    let cfg_for_mcp = crate::shared::read_shared_config(&shared_config).clone();
    if !cfg_for_mcp.mcp_servers.is_empty() {
        let mcp_mgr = session::mcp_client::McpClientManager::new(&cfg_for_mcp.mcp_servers).await;
        let mcp_tool_count = mcp_mgr.tool_count();
        if mcp_tool_count > 0 {
            let mcp_mgr = std::sync::Arc::new(mcp_mgr);
            toolset.add(Box::new(session::toolset::VecToolset::new(
                "mcp",
                session::mcp_tools::all_mcp_tools(mcp_mgr),
            )));
            tracing::info!(count = mcp_tool_count, "MCP tools registered");
        }
    }

    // ── Plugin tools ──
    let plugins_dir = session::data_dir()
        .map(|d| d.join("plugins"))
        .unwrap_or_else(|_| PathBuf::from(".local/share/kirkforge/plugins"));
    let mut plugin_registry = kirkforge_plugin_host::PluginRegistry::new();
    let plugin_warnings = plugin_registry
        .load_from_dir(&plugins_dir, TrustPolicy::up_to(TrustTier::Shell))
        .unwrap_or_default();
    let plugin_tools = session::plugin_tools::all_plugin_tools(&plugin_registry);
    if !plugin_tools.is_empty() {
        toolset.add(Box::new(session::toolset::VecToolset::new(
            "plugin",
            plugin_tools,
        )));
        tracing::info!(
            count = plugin_registry.active_count(),
            "plugin tools registered"
        );
    }
    for w in plugin_warnings {
        tracing::warn!(warning = %w, "plugin load warning");
    }

    let tools = toolset.into_tools();

    if let Some(sys) = &system {
        // Wired into the executor's PromptBuilder before the first turn
        // (see tui::run_tui and run_non_interactive). Kept as an info
        // log so operators can confirm the override took effect.
        tracing::info!("System prompt set from CLI: {}", sys);
    }

    if non_interactive {
        run_non_interactive(
            shared_config,
            adapter,
            tools,
            conversation,
            system,
            output,
            max_turns,
        )
        .await
    } else {
        tui::run_tui(
            shared_config,
            adapter,
            tools,
            conversation,
            system,
            undo_stack,
        )
        .await
    }
}

async fn run_non_interactive(
    config: crate::shared::SharedConfig,
    adapter: Box<dyn adapters::ModelAdapter>,
    tools: Vec<Arc<dyn tools::Tool>>,
    conversation: session::conversation::ConversationLog,
    system: Option<String>,
    output: crate::shared::OutputFormat,
    max_turns: usize,
) -> anyhow::Result<()> {
    let model_name = adapter.model_info().name.clone();

    let mut executor = session::executor::Executor::with_log_and_undo(
        adapter,
        tools,
        config.clone(),
        conversation,
        None,
        None,
    );
    // Apply --system override before run_turn. Without this the
    // override is silently dropped (was GPT 5.5 review finding #2).
    executor.set_system_override(system.clone());

    let (approval_tx, mut approval_rx) =
        mpsc::unbounded_channel::<session::executor::ApprovalRequest>();
    tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            let response = if crate::shared::read_shared_config(&config).auto_approve {
                session::executor::ApprovalResponse::Approved
            } else {
                // Non-interactive mode has no human in the loop, so a
                // tool that requires approval cannot be allowed. Log
                // a warning that names the tool and tells the operator
                // how to opt in to automatic approval.
                tracing::warn!(
                    tool = %req.tool_name,
                    args = %req.args,
                    "non-interactive run denied approval for tool; pass --auto-approve or set auto_approve=true to allow destructive tools without interaction"
                );
                session::executor::ApprovalResponse::Denied
            };

            if let Err(e) = req.response.send(response) {
                tracing::warn!(
                    tool = %req.tool_name,
                    error = ?e,
                    "approval responder dropped before send (executor may have cancelled or shut down)"
                );
            }
        }
    });

    if let Some(sys) = &system {
        tracing::info!("System prompt set from CLI: {}", sys);
    }

    let cancelled = std::sync::atomic::AtomicBool::new(false);

    // Read prompts from stdin line-by-line. Review.md gap #2: the
    // previous `read_to_string` + one-shot `run_turn` made scripting
    // multi-turn sessions impossible. Newline-delimited input is
    // pipe- and heredoc-friendly:
    //
    //   $ printf 'turn 1\nturn 2\nturn 3\n' | kirkforge run --non-interactive --max-turns 3
    //
    // Blank line ends input. `--max-turns 0` (the default) means
    // "unlimited until EOF or a blank line."
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut line_buf = String::new();
    let mut turn_no: usize = 0;
    let mut total_prompt_tokens: usize = 0;
    let mut total_completion_tokens: usize = 0;
    let mut cumulative_cost: f64 = 0.0;
    let mut all_tool_records: Vec<crate::shared::ToolCallRecord> = Vec::new();
    let mut final_error: Option<String> = None;
    let overall_started = std::time::Instant::now();

    while let Some(input) = next_prompt(&mut reader, &mut line_buf)? {
        turn_no += 1;
        if max_turns > 0 && turn_no > max_turns {
            tracing::info!(
                turn_no,
                max_turns,
                "reached --max-turns cap; stopping stdin read"
            );
            break;
        }

        let turn_started_at = std::time::Instant::now();
        let events = executor.run_turn(&input, &approval_tx, &cancelled).await?;
        // Per-turn wall-clock is recorded for tracing but not surfaced
        // in the JSON summary — the summary's `duration_ms` is the
        // overall wall-clock across all turns (set at end-of-loop).
        let _turn_duration_ms = turn_started_at.elapsed().as_millis() as u64;
        let turn_outcome = emit_turn_events(
            &events,
            output,
            &mut total_prompt_tokens,
            &mut total_completion_tokens,
            &mut cumulative_cost,
            &mut all_tool_records,
            &mut final_error,
        );
        // If a turn errored fatally (e.g. JSON parse error after
        // retry), the executor's event stream is the only signal —
        // we keep going for subsequent turns unless the user
        // passes `--max-turns 1`.
        let _ = turn_outcome;
    }

    // Post-loop: if we never ran a turn and no `--system` was
    // supplied, mirror the pre-M4 "No input provided" error. The
    // pre-M4 check lived inside the single read; here we check
    // after the loop because `next_prompt` filters blank lines
    // into `None` and EOF is also `None` — both cases end up
    // with `turn_no == 0`.
    if turn_no == 0 && system.is_none() {
        tracing::warn!("No input provided. Pipe a prompt or use --system.");
        return Ok(());
    }

    if output == crate::shared::OutputFormat::Text {
        println!();
    }

    if output == crate::shared::OutputFormat::Json {
        let total_duration_ms = overall_started.elapsed().as_millis() as u64;
        let recorded_messages: Vec<_> = executor.conversation_log().all().to_vec();
        let summary = crate::shared::SessionSummary {
            version: "1.0".into(),
            session: crate::shared::SessionInfo {
                id: "non-interactive".into(),
                model: model_name,
                duration_ms: total_duration_ms,
                started_at: chrono::Local::now().to_rfc3339(),
            },
            messages: recorded_messages,
            tool_calls: all_tool_records,
            usage: crate::shared::UsageSummary {
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

/// Serialize a JSON value and emit it as one stream-json line.
///
/// `serde_json::to_string` can fail only for non-finite floats; if that
/// somehow happens (e.g. a corrupted cost value), we log a warning and
/// skip the line rather than panicking in the headless output path.
fn print_json_line(value: &serde_json::Value) {
    match serde_json::to_string(value) {
        Ok(line) => println!("{}", line),
        Err(e) => tracing::warn!("failed to serialize stream-json event: {}", e),
    }
}

/// Per-turn event emission, extracted from the pre-M4 single-turn
/// loop so the multi-turn driver can call it once per turn without
/// duplicating the 165-line match. Mutates the running totals in
/// place; returns the `final_error` (if any) so the caller can
/// keep a "most recent error" pointer for the JSON summary.
#[allow(clippy::too_many_arguments)]
fn emit_turn_events(
    events: &[session::executor::TurnEvent],
    output: crate::shared::OutputFormat,
    total_prompt_tokens: &mut usize,
    total_completion_tokens: &mut usize,
    cumulative_cost: &mut f64,
    tool_records: &mut Vec<crate::shared::ToolCallRecord>,
    final_error: &mut Option<String>,
) -> Option<String> {
    // Per-tool timing + structured records for the JSON summary.
    // `ToolStart` arms the timer; the matching `ToolResult` reads
    // it and pushes a `ToolCallRecord` into `tool_records`. Tools
    // are dispatched sequentially by the executor, so a single
    // `Option` for the in-flight call is sufficient — we don't
    // need to key by id. The previous implementation emitted
    // `tool_calls: vec![]` regardless of reality (GPT 5.5 #13);
    // this fixes it.
    let mut in_flight: Option<(String, serde_json::Value, std::time::Instant)> = None;

    for event in events {
        match event {
            session::executor::TurnEvent::Token(t) => {
                if output == crate::shared::OutputFormat::Text {
                    print!("{}", t);
                    let _ = std::io::stdout().flush();
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "token", "content": t});
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::Thinking(t) => {
                if output == crate::shared::OutputFormat::Text {
                    eprintln!("\n[thinking] {}", t);
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "thinking", "content": t});
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::ToolStart { name, args } => {
                if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "tool_start", "name": name});
                    print_json_line(&line);
                }
                // Arm the in-flight timer for the matching ToolResult.
                // If we somehow see a second ToolStart without an
                // intervening ToolResult (shouldn't happen given the
                // executor's dispatch order, but defensive), the older
                // record is dropped — better than accumulating stale
                // timers.
                in_flight = Some((name.clone(), args.clone(), std::time::Instant::now()));
            }
            session::executor::TurnEvent::ToolResult {
                name,
                output: result,
                success,
            } => {
                if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "tool_result",
                        "name": name,
                        "content": result,
                    });
                    print_json_line(&line);
                } else if output == crate::shared::OutputFormat::Text {
                    eprintln!("\n[tool: {}] {} chars", name, result.len());
                }
                // If we have a matching in-flight record, fold it
                // into a ToolCallRecord and push. Name mismatch
                // (shouldn't happen but be defensive) falls back to
                // empty args + zero duration.
                if let Some((start_name, start_args, start_time)) = in_flight.take() {
                    let duration_ms = start_time.elapsed().as_millis() as u64;
                    let record = crate::shared::ToolCallRecord {
                        name: start_name,
                        arguments: start_args,
                        result: result.clone(),
                        success: *success,
                        duration_ms,
                    };
                    tool_records.push(record);
                    // If the name in the result doesn't match the
                    // start (paranoia), prefer the start name. We
                    // already used `start_name`; nothing to do.
                    let _ = name;
                }
            }
            session::executor::TurnEvent::Verification { message, success } => {
                if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "verification",
                        "message": message,
                        "success": success,
                    });
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::Error(e) => {
                *final_error = Some(e.clone());
                if output == crate::shared::OutputFormat::Text {
                    eprintln!("\n[error] {}", e);
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "error", "content": e});
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::CostStats {
                prompt_tokens,
                completion_tokens,
                turn_cost,
                cumulative_cost: cum_cost,
            } => {
                *total_prompt_tokens += prompt_tokens;
                *total_completion_tokens += completion_tokens;
                *cumulative_cost = *cum_cost;

                if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "cost",
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "turn_cost": turn_cost,
                        "cumulative_cost": *cum_cost,
                    });
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::PlanComplete => {
                // Non-interactive mode does not enter plan mode, so this
                // event should not arrive. If it does, ignore it.
            }
            session::executor::TurnEvent::CompactionReport {
                dropped_tool_results,
                condensed_assistant_turns,
                original_count,
                compacted_count,
                ..
            } => {
                if output == crate::shared::OutputFormat::Text {
                    eprintln!(
                        "\n[compaction] {} → {} messages, dropped {} tool result(s), condensed {} assistant turn(s).",
                        original_count,
                        compacted_count,
                        dropped_tool_results,
                        condensed_assistant_turns,
                    );
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "compaction",
                        "original_count": original_count,
                        "compacted_count": compacted_count,
                        "dropped_tool_results": dropped_tool_results,
                        "condensed_assistant_turns": condensed_assistant_turns,
                    });
                    print_json_line(&line);
                }
            }
        }
    }

    final_error.clone()
}

/// Resolve a `--continue-session` value to a log path.
///
/// Pure: takes the raw CLI string and returns either a `PathBuf`
/// (for path-style values) or an error. For id-prefix values, the
/// call to `session_index::resolve_session_id` is what actually
/// hits the filesystem — that side effect is documented at the
/// call site (`run_session`) so callers know what they're invoking.
fn resolve_continue_path(value: &str) -> anyhow::Result<std::path::PathBuf> {
    if value.contains('/') || value.ends_with(".conv.ndjson") {
        return Ok(std::path::PathBuf::from(value));
    }
    match session::session_index::resolve_session_id(value) {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Err(anyhow::anyhow!(
            "No saved session found matching '{}'. Run `kirkforge run --non-interactive` once to create one, or use `/sessions` in the TUI to list.",
            value
        )),
        Err(e) => Err(anyhow::anyhow!(
            "Error resolving session id '{}': {}",
            value,
            e
        )),
    }
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

/// Print a hint listing recent sessions when running non-interactively
/// without an explicit resume target.
fn print_recent_sessions_hint(sessions: &[crate::session::session_index::SessionEntry]) {
    eprintln!("Recent sessions (run with --auto-resume or --attach <id> to resume):");
    for (i, e) in sessions
        .iter()
        .enumerate()
        .take(crate::daemon::RECENT_SESSIONS_LIMIT)
    {
        eprintln!(
            "  {}. {} — {} messages — {}",
            i + 1,
            e.id,
            e.message_count,
            e.started_at
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Path-style values (containing a `/`) are returned as-is,
    /// without touching the session index. This is the "I have a
    /// specific log file path" case.
    #[test]
    fn resolve_continue_path_passthrough_for_path_style() {
        let p = resolve_continue_path("/home/kirk/sessions/foo.conv.ndjson").unwrap();
        assert_eq!(
            p,
            std::path::PathBuf::from("/home/kirk/sessions/foo.conv.ndjson")
        );
    }

    /// `.conv.ndjson` suffix is enough to be treated as a path,
    /// even without a separator. Belt-and-suspenders for users
    /// who pass a bare filename.
    #[test]
    fn resolve_continue_path_passthrough_for_conv_ndjson_suffix() {
        let p = resolve_continue_path("foo.conv.ndjson").unwrap();
        assert_eq!(p, std::path::PathBuf::from("foo.conv.ndjson"));
    }

    /// Empty input: contains neither a slash nor the suffix, so
    /// it would be treated as a session id prefix. An empty prefix
    /// is unlikely to resolve to anything; we just check the call
    /// goes through the id-resolution path (and errors out at the
    /// session-index layer for this test env, which is fine).
    #[test]
    fn resolve_continue_path_id_prefix_goes_to_index() {
        // We can't assert the exact error text (depends on the
        // real session directory) but we can assert it's an error
        // and that it doesn't fall through as a path.
        let r = resolve_continue_path("definitely-not-a-real-session-xyzzy");
        assert!(r.is_err(), "expected an error, got: {:?}", r);
        let err = r.unwrap_err().to_string();
        // Either "No saved session found" (empty sessions dir) or
        // a session-index error. Both indicate the id-resolution
        // path was taken, which is what we want to verify.
        assert!(
            err.contains("No saved session") || err.contains("Error resolving session id"),
            "unexpected error: {}",
            err
        );
    }

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
}
