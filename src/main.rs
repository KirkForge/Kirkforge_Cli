// Many modules expose stubs and helper APIs not yet wired into the main
// execution path — suppress dead-code warnings so we keep the public API
// surface clean for future callers.
#![allow(dead_code)]

mod adapters;
mod session;
mod shared;
mod tools;
mod tui;

use clap::Parser;
use std::io::Write;
use std::sync::Arc;

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
    let mut config = session::load_or_create_config();

    // Apply CLI overrides
    if let Some(host) = &cli.host {
        config.ollama_host = host.clone();
    }
    let model = cli.model.clone().unwrap_or_else(|| config.default_model.clone());
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
    _config: crate::shared::Config,
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

    // Build messages
    let model_info = adapter.model_info();
    let tool_names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
    let mut prompt_builder = session::prompt::PromptBuilder::new();

    let mut messages = Vec::new();

    // System prompt
    let system = prompt_builder.build(&model_info.name, model_info.supports_thinking, &tool_names);
    messages.push(system);

    // Existing conversation
    for msg in conversation.all() {
        messages.push(msg.clone());
    }

    let output = cli.output;
    let model_name = model_info.name.clone();

    // User input
    let user_msg = crate::shared::Message {
        role: crate::shared::Role::User,
        content: input.clone(),
        thinking: None,
        tool_calls: None,
        tool_call_id: None,
        tool_name: None,
        token_count: None,
    };

    // Accumulators for JSON summary
    let mut recorded_messages: Vec<crate::shared::Message> = Vec::new();
    let mut recorded_tool_calls: Vec<crate::shared::ToolCallRecord> = Vec::new();
    let mut assistant_content = String::new();
    let mut total_prompt_tokens: usize = 0;
    let mut total_completion_tokens: usize = 0;
    let mut cumulative_cost: f64 = 0.0;
    let mut final_error: Option<String> = None;

    if !input.is_empty() {
        recorded_messages.push(user_msg.clone());
        messages.push(user_msg);
    }

    let tool_defs: Vec<crate::shared::ToolDef> = tools.iter().map(|t| t.def()).collect();

    // Stream the response
    let mut rx = adapter.stream(&messages, &tool_defs).await?;

    while let Some(event) = rx.recv().await {
        match event {
            crate::shared::StreamEvent::Text(t) => {
                if output == crate::shared::OutputFormat::Text {
                    print!("{}", t);
                    std::io::stdout().flush()?;
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "token", "content": t});
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
                assistant_content.push_str(&t);
            }
            crate::shared::StreamEvent::Thinking(t) => {
                if output == crate::shared::OutputFormat::Text {
                    eprintln!("\n[thinking] {}", t);
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "thinking", "content": t});
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
            }
            crate::shared::StreamEvent::ToolCall(tc) => {
                let start = std::time::Instant::now();
                let (output_content, success) = if let Some(tool) = tools.iter().find(|t| t.def().name == tc.name) {
                    let result = tool.run(tc.arguments.clone()).await;
                    match &result {
                        crate::shared::ToolOutcome::Success { content }
                        | crate::shared::ToolOutcome::FileContent { content, .. }
                        | crate::shared::ToolOutcome::FileEdit { diff: content, .. } => {
                            (content.clone(), true)
                        }
                        crate::shared::ToolOutcome::GrepMatches { matches, .. } => {
                            (format!("{} matches", matches.len()), true)
                        }
                        crate::shared::ToolOutcome::Error { message } => {
                            (message.clone(), false)
                        }
                    }
                } else {
                    (format!("Unknown tool: {}", tc.name), false)
                };

                let duration_ms = start.elapsed().as_millis() as u64;

                if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "tool_call",
                        "name": tc.name,
                        "arguments": tc.arguments,
                        "duration_ms": duration_ms,
                        "success": success,
                    });
                    println!("{}", serde_json::to_string(&line).unwrap());
                } else {
                    eprintln!("\n[tool: {}] {} ({})", tc.name, output_content.len(), if success { "ok" } else { "error" });
                }

                recorded_tool_calls.push(crate::shared::ToolCallRecord {
                    name: tc.name.clone(),
                    arguments: tc.arguments,
                    result: output_content,
                    success,
                    duration_ms,
                });
            }
            crate::shared::StreamEvent::Error(e) => {
                final_error = Some(e.clone());
                if output == crate::shared::OutputFormat::Text {
                    eprintln!("\n[error] {}", e);
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "error", "content": e});
                    println!("{}", serde_json::to_string(&line).unwrap());
                }
            }
            crate::shared::StreamEvent::Done { finish_reason: _, usage } => {
                // Record assistant message
                recorded_messages.push(crate::shared::Message {
                    role: crate::shared::Role::Assistant,
                    content: assistant_content.clone(),
                    thinking: None,
                    tool_calls: None,
                    tool_call_id: None,
                    tool_name: None,
                    token_count: None,
                });

                // Cost tracking
                if let Some(ref u) = usage {
                    let pt = u.prompt_tokens.unwrap_or(0);
                    let ct = u.completion_tokens.unwrap_or(0);
                    total_prompt_tokens += pt;
                    total_completion_tokens += ct;
                    let cost = crate::shared::calculate_cost(&model_name, pt, ct);
                    cumulative_cost += cost;

                    if output == crate::shared::OutputFormat::StreamJson {
                        let line = serde_json::json!({
                            "type": "cost",
                            "prompt_tokens": pt,
                            "completion_tokens": ct,
                            "turn_cost": cost,
                            "cumulative_cost": cumulative_cost,
                        });
                        println!("{}", serde_json::to_string(&line).unwrap());
                    }
                }

                if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({
                        "type": "done",
                        "usage": {
                            "prompt_tokens": total_prompt_tokens,
                            "completion_tokens": total_completion_tokens,
                            "total_tokens": total_prompt_tokens + total_completion_tokens,
                            "cost_usd": cumulative_cost,
                        }
                    });
                    println!("{}", serde_json::to_string(&line).unwrap());
                } else {
                    println!();
                }
                break;
            }
        }
    }

    // JSON mode: full summary at end
    if output == crate::shared::OutputFormat::Json {
        let summary = crate::shared::SessionSummary {
            version: "1.0".into(),
            session: crate::shared::SessionInfo {
                id: "non-interactive".into(),
                model: model_name,
                duration_ms: 0,
                started_at: chrono::Local::now().to_rfc3339(),
            },
            messages: recorded_messages,
            tool_calls: recorded_tool_calls,
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