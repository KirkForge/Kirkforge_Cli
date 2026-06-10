

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

    let ollama_host = &config.ollama_host;

    let data_dir = session::data_dir()?;
    std::fs::create_dir_all(&data_dir)?;

    let session_id = session::new_session_id();
    let log_path = if let Some(resume) = &resume {
        std::path::PathBuf::from(resume)
    } else {
        let sessions_dir = data_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        sessions_dir.join(format!("{}.conv.ndjson", session_id))
    };

    let conversation = session::conversation::ConversationLog::open(log_path)?;

    let adapter = adapters::adapter_for(&model, ollama_host, model_type.as_deref());

    let mut tools: Vec<Arc<dyn tools::Tool>> = tools::all_tools()
        .into_iter()
        .map(|t| t as Arc<dyn tools::Tool>)
        .collect();

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
        run_non_interactive(config, adapter, tools, conversation, system, output).await
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
) -> anyhow::Result<()> {

    let mut input = String::new();
    use std::io::Read;
    std::io::stdin().read_to_string(&mut input)?;
    let input = input.trim().to_string();

    if input.is_empty() && system.is_none() {
        eprintln!("No input provided. Pipe a prompt or use --system.");
        return Ok(());
    }

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

    // Wall-clock for the truthful `duration_ms` in the JSON summary
    // (was hardcoded `0` in the previous implementation — see GPT 5.5
    // review finding #13).
    let turn_started_at = std::time::Instant::now();
    let events = executor.run_turn(&input, &approval_tx, &cancelled).await?;
    let turn_duration_ms = turn_started_at.elapsed().as_millis() as u64;

    let mut total_prompt_tokens: usize = 0;
    let mut total_completion_tokens: usize = 0;
    let mut cumulative_cost: f64 = 0.0;
    let mut final_error: Option<String> = None;

    // Per-tool timing + structured records for the JSON summary.
    // `ToolStart` arms the timer; the matching `ToolResult` reads it
    // and pushes a `ToolCallRecord` into `tool_records`. Tools are
    // dispatched sequentially by the executor, so a single `Option`
    // for the in-flight call is sufficient — we don't need to key by
    // id. The previous implementation emitted `tool_calls: vec![]`
    // regardless of reality (GPT 5.5 #13); this fixes it.
    let mut in_flight: Option<(String, serde_json::Value, std::time::Instant)> = None;
    let mut tool_records: Vec<crate::shared::ToolCallRecord> = Vec::new();

    for event in events {
        match event {
            session::executor::TurnEvent::Token(t) => {
                if output == crate::shared::OutputFormat::Text {
                    print!("{}", t);
                    std::io::stdout().flush()?;
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
                in_flight = Some((name, args, std::time::Instant::now()));
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
                        success,
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
                final_error = Some(e.clone());
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
                total_prompt_tokens += prompt_tokens;
                total_completion_tokens += completion_tokens;
                cumulative_cost = cum_cost;

                if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "cost",
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "turn_cost": turn_cost,
                        "cumulative_cost": cumulative_cost,
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

    if output == crate::shared::OutputFormat::Text {
        println!();
    }

    if output == crate::shared::OutputFormat::Json {
        let recorded_messages: Vec<_> = executor.conversation_log().all().to_vec();
        let summary = crate::shared::SessionSummary {
            version: "1.0".into(),
            session: crate::shared::SessionInfo {
                id: "non-interactive".into(),
                model: model_name,
                duration_ms: turn_duration_ms,
                started_at: chrono::Local::now().to_rfc3339(),
            },
            messages: recorded_messages,
            tool_calls: tool_records,
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
