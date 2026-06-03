mod adapters;
mod session;
mod shared;
mod tools;
mod tui;

use clap::Parser;
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

    // User input
    if !input.is_empty() {
        messages.push(crate::shared::Message {
            role: crate::shared::Role::User,
            content: input.clone(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        });
    }

    let tool_defs: Vec<crate::shared::ToolDef> = tools.iter().map(|t| t.def()).collect();

    // Stream the response
    let mut rx = adapter.stream(&messages, &tool_defs).await?;

    while let Some(event) = rx.recv().await {
        match event {
            crate::shared::StreamEvent::Text(t) => {
                print!("{}", t);
                use std::io::Write;
                std::io::stdout().flush()?;
            }
            crate::shared::StreamEvent::Thinking(t) => {
                eprintln!("\n[thinking] {}", t);
            }
            crate::shared::StreamEvent::ToolCall(tc) => {
                // In non-interactive mode, execute tool calls automatically
                if let Some(tool) = tools.iter().find(|t| t.def().name == tc.name) {
                    let result = tool.run(tc.arguments.clone()).await;
                    match result {
                        crate::shared::ToolOutcome::Success { content }
                        | crate::shared::ToolOutcome::FileContent { content, .. }
                        | crate::shared::ToolOutcome::FileEdit { diff: content, .. } => {
                            eprintln!("\n[tool: {}] {} bytes returned", tc.name, content.len());
                        }
                        crate::shared::ToolOutcome::Error { message } => {
                            eprintln!("\n[tool: {}] Error: {}", tc.name, message);
                        }
                        _ => {}
                    }
                }
            }
            crate::shared::StreamEvent::Error(e) => {
                eprintln!("\n[error] {}", e);
            }
            crate::shared::StreamEvent::Done { .. } => {
                println!();
                break;
            }
        }
    }

    Ok(())
}