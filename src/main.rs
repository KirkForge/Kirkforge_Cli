// Crate-level `#[allow(dead_code)]` is in place while a per-module
// triage is in progress. Triage status (Phase 36+):
//
//   - **Removed in this branch (was queued for v1.3):**
//     - `shared::permission::describe_rule` (no caller — the TUI
//       builds its own description string from rule fields directly)
//     - `shared::Approval` enum (unrelated to `executor::ApprovalDecision`
//       and `executor::ApprovalResponse`; leftover from an earlier API)
//     - `tui::commands::mentions::MentionParse` and its `is_ok` /
//       `display_path` methods (no caller in the new TUI dispatch path)
//
//   - **Likely unused, kept pending verification of the v1.3 work list:**
//     - `session::access::DenyList::is_url_denied` — only its own
//       test exercises it; there's no URL fetch tool yet, so this is
//       speculative API surface
//     - `tui::rendering::{syntax_set, theme_set, highlight_code,
//       render_markdown, truncate, format_size}` — not called from
//       the current TUI code; looks like an earlier "phase 8" plan
//       that never landed
//
//   - **Match-arm variants that look unused but aren't:**
//     - `tui::app::ConnectionState::{Connecting, Connected, Error}` —
//       exhaustive-match sites in `tui/widgets/{chat,status}.rs` cover
//       all four variants even though only `Disconnected` is set in
//       `AppState::new()`. Keep until the TUI actually drives a real
//       connection state machine.
//
//   - **The remaining ~50 warnings are field/event variants that are
//     intentionally public for the event bus / verifier / truncation
//     inter-module contract:** `BusEvent` payload fields, `CorrectionResult`
//     fields, `TruncationStrategy` variants, `ToolInvocation`/`Message`
//     fields populated by the parser. These are pub(crate) API even
//     when the consuming side isn't built yet. Triage is item-by-item;
//     the work list lives in the v1.3 milestone notes.
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
    #[arg(long)]
    host: Option<String>,

    /// Model type override (glm, deepseek, gemini, openai)
    #[arg(long)]
    model_type: Option<String>,

    /// Auto-approve destructive tool calls (bash, write, edit).
    ///
    /// **What `--auto-approve` does:** sets the *default* permission
    /// action to `Allow` instead of `Ask`. The first time the model
    /// wants to run `bash`, `write_file`, or `edit_file`, the call
    /// runs without a confirmation prompt.
    ///
    /// **What `--auto-approve` does NOT do:**
    /// - It does NOT skip user-written `deny` rules — a rule like
    ///   `[[permission_rules]] tool = "bash" key = "command"
    ///   pattern = "rm -rf **" action = "deny"` still refuses, no
    ///   matter what `--auto-approve` is.
    /// - It does NOT bypass the built-in danger list
    ///   (`rm -rf /`, fork bombs, `dd if=/dev/zero of=...`,
    ///   metadata-endpoint references, etc.) — those are blocked
    ///   unconditionally.
    /// - It does NOT skip the path deny-list, denied extensions, or
    ///   the path guard (sandbox / `allowed_write_dirs` / dotfile /
    ///   symlink rules) for file tools.
    /// - It does NOT relax the read-only classification for bash:
    ///   `auto_approve` still requires the command to be in the
    ///   read-only allowlist AND to have no dangerous chains
    ///   (redirects, pipe-to-shell, `;`, `&&`, `||`, `$(...)`).
    ///   Non-read-only bash (e.g. `cargo build`, `npm install`,
    ///   `python script.py`) still goes through the approval flow
    ///   even with `--auto-approve`.
    ///
    /// **Bottom line:** `--auto-approve` removes the interactive
    /// `Ask` prompt for destructive calls whose command/path
    /// matches no `deny` rule, is not on the built-in danger list,
    /// and (for bash) is read-only. It is a usability shortcut for
    /// scripted / non-interactive use, not a sandbox bypass.
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
    let mut executor =
        session::executor::Executor::with_log(adapter, tools, config.clone(), conversation, None);

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
            // Surfacing a dropped-responder here is meaningful: it
            // means the executor stopped waiting on this approval
            // (cancellation, panic, or shutdown) before we got our
            // decision. The old `let _ =` hid this from logs.
            // `?e` (Debug) because `oneshot::error::SendError<T>`
            // only impls Debug.
            if let Err(e) = req.response.send(response) {
                tracing::warn!(
                    tool = %req.tool_name,
                    error = ?e,
                    "approval responder dropped before send (executor may have cancelled or shut down)"
                );
            }
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
            session::executor::TurnEvent::ToolResult {
                name,
                output: result,
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
                // Headless mode: the executor has already atomically
                // rewritten the NDJSON log and updated its in-memory
                // conversation. We just need to surface the outcome
                // to the user / machine consumer.
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
