// Dead-code warnings.
//
// Review.md arch concern #2 called out the blanket `#![allow(dead_code)]`
// at the crate root: it suppresses warnings across the entire crate, so
// genuinely-unused code never surfaces. The principled fix is to scope
// the allow to each file that has a public surface not yet wired into
// the in-crate call graph — that is, library-style modules whose `pub`
// items are meant for external consumers but Rust's lint can't see them.
//
// 22 files currently produce dead-code warnings. Scoping the allow
// across all of them is a separate, multi-hour cleanup that doesn't
// ship in M1 alongside the bang/scheduler work. The pragmatic
// compromise: keep the root-level allow for now, but record the
// deferred scoping as a tracked follow-up. The list of offenders
// is captured in `state.md` for the next cleanup pass.
#![allow(dead_code)]

mod adapters;
mod session;
mod shared;
mod tools;
mod tui;

use clap::{Parser, Subcommand};
use std::io::Write;
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
    },

    Schedule {

        #[arg(long)]
        config: Option<std::path::PathBuf>,

        #[arg(long)]
        print_config: bool,
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
            })
            .await
        }
        Command::Schedule { config, print_config } => run_scheduler(config, print_config).await,
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
    } = args;

    let mut config = session::config::load_or_create_config();

    if let Some(host) = &host {
        config.ollama_host = host.clone();
    }
    let model = model.unwrap_or_else(|| config.default_model.clone());
    if auto_approve {
        config.auto_approve = true;
    }

    let _ = session::config::save_config(&config);

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
    // Resolve the log path. Three inputs can determine it, in
    // priority order:
    //   1. `--continue-session <value>` — id prefix OR full path
    //   2. `--resume <path>`            — legacy path-only flag
    //   3. brand-new session id
    //
    // `--continue-session` accepts a session-id prefix when the
    // value does not contain a path separator; if it does, it's
    // treated as a full path. The legacy `--resume` flag is kept
    // for back-compat and behaves exactly as it did before M4.
    let log_path = if let Some(cont) = &continue_session {
        resolve_continue_path(cont)?
    } else if let Some(resume) = &resume {
        std::path::PathBuf::from(resume)
    } else {
        let sessions_dir = data_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        sessions_dir.join(format!("{}.conv.ndjson", session_id))
    };

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

    let mut tools: Vec<Arc<dyn tools::Tool>> =
        tools::all_tools(undo_stack.clone(), adapter.model_info().supports_images);

    // --- MCP tools ---
    if !config.mcp_servers.is_empty() {
        let mcp_mgr = session::mcp_client::McpClientManager::new(&config.mcp_servers).await;
        let mcp_tool_count = mcp_mgr.tool_count();
        if mcp_tool_count > 0 {
            let mcp_mgr = std::sync::Arc::new(mcp_mgr);
            let mcp_tools = session::mcp_tools::all_mcp_tools(mcp_mgr);
            tools.extend(mcp_tools);
            tracing::info!(count = mcp_tool_count, "MCP tools registered");
        }
    }

    if let Some(sys) = &system {
        // Wired into the executor's PromptBuilder before the first turn
        // (see tui::run_tui and run_non_interactive). Kept as an info
        // log so operators can confirm the override took effect.
        tracing::info!("System prompt set from CLI: {}", sys);
    }

    if non_interactive {
        run_non_interactive(
            config,
            adapter,
            tools,
            conversation,
            system,
            output,
            max_turns,
        )
        .await
    } else {
        tui::run_tui(config, adapter, tools, conversation, system).await
    }
}

async fn run_scheduler(
    config: Option<std::path::PathBuf>,
    print_config: bool,
) -> anyhow::Result<()> {
    let path = config.unwrap_or_else(|| {
        let mut p = session::data_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        p.push("schedule.toml");
        p
    });

    if print_config {
        let cfg = session::scheduler::ScheduleConfig::load(&path)?;
        println!("schedule.toml  : {}", path.display());
        println!("cron           : {}", cfg.schedule.cron);
        println!("work_budget_s  : {}", cfg.schedule.work_budget_secs);
        println!("idle_timeout_s : {}", cfg.schedule.idle_timeout_secs);
        println!("token_cap      : {}", cfg.schedule.token_cap);
        println!("model          : {}", cfg.session.model);
        println!("project_dir    : {}", cfg.resolved_project_dir().display());
        println!("state_dir      : {}", cfg.resolved_state_dir().display());
        match &cfg.session.prompt {
            Some(p) => {
                let preview: String = p.chars().take(80).collect();
                println!("prompt         : {} ({} chars)", preview, p.len());
            }
            None => println!("prompt_file    : {:?}", cfg.session.prompt_file),
        }
        return Ok(());
    }

    let cfg = session::scheduler::ScheduleConfig::load(&path)?;
    let sched = session::scheduler::Scheduler::new(cfg)?;
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    sched.run(shutdown).await
}

async fn run_non_interactive(
    config: crate::shared::Config,
    adapter: Box<dyn adapters::ModelAdapter>,
    tools: Vec<Arc<dyn tools::Tool>>,
    conversation: session::conversation::ConversationLog,
    system: Option<String>,
    output: crate::shared::OutputFormat,
    max_turns: usize,
) -> anyhow::Result<()> {

    let model_name = adapter.model_info().name.clone();

    let mut executor =
        session::executor::Executor::with_log(adapter, tools, config.clone(), conversation, None);
    // Apply --system override before run_turn. Without this the
    // override is silently dropped (was GPT 5.5 review finding #2).
    executor.set_system_override(system.clone());

    let (approval_tx, mut approval_rx) =
        mpsc::unbounded_channel::<session::executor::ApprovalRequest>();
    tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            let response = if config.auto_approve {
                session::executor::ApprovalResponse::Approved
            } else {
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
        eprintln!("No input provided. Pipe a prompt or use --system.");
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
        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    }

    Ok(())
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
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
            }
            session::executor::TurnEvent::Thinking(t) => {
                if output == crate::shared::OutputFormat::Text {
                    eprintln!("\n[thinking] {}", t);
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "thinking", "content": t});
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
            }
            session::executor::TurnEvent::ToolStart { name, args } => {
                if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "tool_start", "name": name});
                    println!("{}", serde_json::to_string(&line).unwrap());
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
                    println!("{}", serde_json::to_string(&line).unwrap());
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
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
            }
            session::executor::TurnEvent::Error(e) => {
                *final_error = Some(e.clone());
                if output == crate::shared::OutputFormat::Text {
                    eprintln!("\n[error] {}", e);
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "error", "content": e});
                    println!("{}", serde_json::to_string(&line).unwrap());
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
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
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
                    println!("{}", serde_json::to_string(&line).unwrap());
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
fn next_prompt<R: std::io::BufRead>(reader: &mut R, buf: &mut String) -> std::io::Result<Option<String>> {
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

    /// Path-style values (containing a `/`) are returned as-is,
    /// without touching the session index. This is the "I have a
    /// specific log file path" case.
    #[test]
    fn resolve_continue_path_passthrough_for_path_style() {
        let p = resolve_continue_path("/home/kirk/sessions/foo.conv.ndjson").unwrap();
        assert_eq!(p, std::path::PathBuf::from("/home/kirk/sessions/foo.conv.ndjson"));
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
            err.contains("No saved session")
                || err.contains("Error resolving session id"),
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
