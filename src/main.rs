// Dead-code warnings suppressed for:
//   - Public API fields/variants defined for completeness (not all consumed yet)
//   - Handler methods registered at runtime via trait objects
//   - Test helpers behind #[cfg(test)]
// The one truly dead subsystem (workflow engine, 889 lines) has been removed.
#![allow(dead_code)]

mod adapters;
mod session;
mod shared;
mod tools;
mod tui;

use clap::Parser;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::mpsc;

/// KirkForge — Native Ollama CLI coding agent.
///
/// A static-binary TUI agent that talks directly to Ollama.
/// Runs on potato hardware. No Node.js, no Anthropic layer, no proxy.
#[derive(Parser, Debug)]
#[command(name = "kirkforge", version, about)]
struct Cli {
    /// Model to use (default: from config)
    #[arg(short, long)]
    model: Option<String>,

    /// Ollama host (default: http://localhost:11434)
    #[arg(short, long)]
    host: Option<String>,

    /// Model type override (glm, deepseek, gemini, openai)
    #[arg(long)]
    model_type: Option<String>,

    /// Auto-approve destructive tool calls (bash, write, edit)
    #[arg(long)]
    auto_approve: bool,

    /// Start with a system message
    #[arg(short, long)]
    system: Option<String>,

    /// Path to a conversation log to resume
    #[arg(short, long)]
    resume: Option<String>,

    /// Never prompt — just log the response and exit
    #[arg(long)]
    non_interactive: bool,

    /// Output format for non-interactive mode
    #[arg(long, default_value = "text")]
    output: crate::shared::OutputFormat,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    // Load config
    let mut config = session::config::load_or_create_config();

    // Apply CLI overrides
    if let Some(host) = &cli.host {
        config.ollama_host = host.clone();
    }
    let model = cli
        .model
        .clone()
        .unwrap_or_else(|| config.default_model.clone());
    if cli.auto_approve {
        config.auto_approve = true;
    }

    // Persist config so CLI overrides (model, host, auto-approve) survive
    let _ = session::config::save_config(&config);

    let ollama_host = &config.ollama_host;

    // Set up session data directory
    let data_dir = session::data_dir()?;
    std::fs::create_dir_all(&data_dir)?;

    // Open or create conversation log
    let session_id = session::new_session_id();
    let log_path = if let Some(resume) = &cli.resume {
        std::path::PathBuf::from(resume)
    } else {
        let sessions_dir = data_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        sessions_dir.join(format!("{}.conv.ndjson", session_id))
    };

    let conversation = session::conversation::ConversationLog::open(log_path)?;

    // Build adapter
    let adapter = adapters::adapter_for(&model, ollama_host, cli.model_type.as_deref());

    // Load tools
    let tools: Vec<Arc<dyn tools::Tool>> = tools::all_tools()
        .into_iter()
        .map(|t| t as Arc<dyn tools::Tool>)
        .collect();

    // Add system prompt if provided
    if let Some(sys) = &cli.system {
        // Will be handled by the prompt builder
        tracing::info!("System prompt set from CLI: {}", sys);
    }

    // Run
    if cli.non_interactive {
        run_non_interactive(config, adapter, tools, conversation, cli).await
    } else {
        tui::run_tui(config, adapter, tools, conversation).await
    }
}

async fn run_non_interactive(
    config: crate::shared::Config,
    adapter: Box<dyn adapters::ModelAdapter>,
    tools: Vec<Arc<dyn tools::Tool>>,
    conversation: session::conversation::ConversationLog,
    cli: Cli,
) -> anyhow::Result<()> {
    // Read prompt from stdin
    let mut input = String::new();
    use std::io::Read;
    std::io::stdin().read_to_string(&mut input)?;
    let input = input.trim().to_string();

    if input.is_empty() && cli.system.is_none() {
        eprintln!("No input provided. Pipe a prompt or use --system.");
        return Ok(());
    }

    let output = cli.output;
    let model_name = adapter.model_info().name.clone();

    // Create an executor with the full safety pipeline:
    // deny list, path guard, read-before-edit gate, verifiers,
    // correction loop, approval flow, conversation logging.
    let mut executor = session::executor::Executor::with_log(
        adapter,
        tools,
        config.clone(),
        conversation,
        None,
    );

    // Approval channel: auto-deny destructive calls unless --auto-approve
    let (approval_tx, mut approval_rx) =
        mpsc::unbounded_channel::<session::executor::ApprovalRequest>();
    tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            let response = if config.auto_approve {
                session::executor::ApprovalResponse::Approved
            } else {
                session::executor::ApprovalResponse::Denied
            };
            let _ = req.response.send(response);
        }
    });

    // Add system prompt as user message if provided
    if let Some(sys) = &cli.system {
        // System prompt handling deferred to prompt builder
        let _ = sys;
    }

    // Run through the executor's full safety pipeline (run_turn)
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let events = executor.run_turn(&input, &approval_tx, &cancelled).await?;

    // Process events for output
    let mut total_prompt_tokens: usize = 0;
    let mut total_completion_tokens: usize = 0;
    let mut cumulative_cost: f64 = 0.0;
    let mut final_error: Option<String> = None;

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
            session::executor::TurnEvent::ToolStart { name, args: _ } => {
                if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "tool_start", "name": name});
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
            }
            session::executor::TurnEvent::ToolResult { name, output: result } => {
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
        }
    }

    // Final newline for text mode
    if output == crate::shared::OutputFormat::Text {
        println!();
    }

    // JSON mode: full summary at end
    if output == crate::shared::OutputFormat::Json {
        let recorded_messages: Vec<_> = executor.conversation_log().all().to_vec();
        let summary = crate::shared::SessionSummary {
            version: "1.0".into(),
            session: crate::shared::SessionInfo {
                id: "non-interactive".into(),
                model: model_name,
                duration_ms: 0,
                started_at: chrono::Local::now().to_rfc3339(),
            },
            messages: recorded_messages,
            tool_calls: vec![],
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
