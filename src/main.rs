mod adapters;
mod daemon;
mod session;
mod shared;
mod tools;
mod tui;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use kirkforge_plugin::TrustTier;
use kirkforge_plugin_host::TrustPolicy;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing_subscriber::prelude::*;

/// Initialize tracing so logs go to a file instead of corrupting the TUI.
///
/// In interactive (TUI) mode stdout is the alternate screen, so any
/// tracing output written there would be drawn over the UI. We always
/// write logs to `<data_dir>/kirkforge.log` and additionally mirror them
/// to stderr when `KIRKFORGE_LOG_STDERR=1` is set (useful for daemon or
/// non-interactive debugging).
fn init_tracing(log_level: &str) {
    // Writer enum so that a failure to open the log file falls back to
    // a null sink instead of panicking on `/dev/null`.
    enum LogWriter {
        File(std::fs::File),
        Sink(std::io::Sink),
    }

    impl std::io::Write for LogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            match self {
                LogWriter::File(f) => f.write(buf),
                LogWriter::Sink(s) => s.write(buf),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            match self {
                LogWriter::File(f) => f.flush(),
                LogWriter::Sink(s) => s.flush(),
            }
        }
    }
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level));

    let log_file = session::data_dir()
        .map(|d| d.join("kirkforge.log"))
        .unwrap_or_else(|_| PathBuf::from("kirkforge.log"));
    let _ = std::fs::create_dir_all(
        log_file
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    );

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(move || {
            // Re-open on every write so rotation can be done by moving the
            // file aside while the process is running. The `tracing-appender`
            // crate would be cleaner, but we avoid the extra dependency.
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
            {
                Ok(file) => LogWriter::File(file),
                Err(e) => {
                    // Last-ditch fallback: write to stderr so logs aren't lost,
                    // and route tracing into a null sink so the subscriber
                    // still initializes even when `/dev/null` is unavailable
                    // (e.g. in a sandboxed or Windows environment).
                    eprintln!("failed to open log file {}: {}", log_file.display(), e);
                    LogWriter::Sink(std::io::sink())
                }
            }
        });

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer);

    if std::env::var("KIRKFORGE_LOG_STDERR").is_ok() {
        let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
        registry.with(stderr_layer).init();
    } else {
        registry.init();
    }
}

/// Map an anyhow error to a structured exit code.
/// 0 = success, 1 = general, 2 = bad args (clap), 3 = model unreachable,
/// 4 = permission/sandbox denied, 5 = config parse error.
fn exit_code(e: &anyhow::Error) -> i32 {
    let msg = format!("{e:#}").to_lowercase();
    if msg.contains("connection refused")
        || msg.contains("failed to connect")
        || msg.contains("dns error")
        || msg.contains("timed out")
        || msg.contains("model not found")
    {
        3
    } else if msg.contains("denied")
        || msg.contains("permission")
        || msg.contains("sandbox")
        || msg.contains("blocked")
    {
        4
    } else if msg.contains("config") && (msg.contains("parse") || msg.contains("invalid")) {
        5
    } else {
        1
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "kirkforge",
    version,
    about,
    after_help = "Exit codes:\n  0  success\n  1  general error\n  2  bad arguments\n  3  model unreachable\n  4  permission / sandbox denied\n  5  config parse error"
)]
struct Cli {
    /// Log verbosity. Overridden by RUST_LOG if set.
    #[arg(
        long,
        default_value = "warn",
        env = "KIRKFORGE_LOG_LEVEL",
        global = true
    )]
    log_level: String,

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

        /// Preview destructive operations without applying them.
        /// Read-only tools still run; write_file, edit_file, and bash
        /// report what they would do.
        #[arg(long)]
        dry_run: bool,

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

        /// Force line-mode (no TUI) even when stdout is a terminal.
        /// Useful when the alternate screen is broken or you want plain
        /// stdin/stdout interaction with explicit approval prompts.
        #[arg(long)]
        no_tui: bool,
    },
    /// Print shell completion script and exit.
    /// Example: kirkforge completions bash >> ~/.bashrc
    Completions { shell: Shell },
    /// List and export past sessions.
    /// Without arguments, lists recent sessions (newest first).
    /// With --export, writes the session to stdout or a file.
    Sessions {
        /// Session id or id prefix to export. Omit to list all sessions.
        id: Option<String>,

        /// Export format: markdown, json, or ndjson.
        #[arg(long, value_name = "FORMAT")]
        export: Option<String>,

        /// Write export to this file instead of stdout.
        #[arg(long, short)]
        output: Option<PathBuf>,
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
async fn main() {
    let cli = Cli::parse();
    init_tracing(&cli.log_level);

    let result = match cli.command {
        Command::Run {
            model,
            host,
            model_type,
            auto_approve,
            dry_run,
            system,
            resume,
            non_interactive,
            output,
            max_turns,
            continue_session,
            auto_resume,
            attach,
            no_tui,
        } => {
            run_session(RunArgs {
                model,
                host,
                model_type,
                auto_approve,
                dry_run,
                system,
                resume,
                non_interactive,
                output,
                max_turns,
                continue_session,
                auto_resume,
                attach,
                no_tui,
            })
            .await
        }
        Command::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "kirkforge",
                &mut std::io::stdout(),
            );
            Ok(())
        }
        Command::Sessions { id, export, output } => handle_sessions_command(id, export, output),
        Command::Daemon { foreground, stop } => daemon::server::run_daemon(foreground, stop).await,
    };

    if let Err(e) = result {
        eprintln!("kirkforge: {e:#}");
        std::process::exit(exit_code(&e));
    }
}

fn handle_sessions_command(
    id: Option<String>,
    export: Option<String>,
    out_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    use session::conversation::ConversationLog;
    use session::session_index::{list_sessions, resolve_session_id};

    // No id → list
    if id.is_none() && export.is_none() {
        let entries = list_sessions().unwrap_or_default();
        if entries.is_empty() {
            println!("No sessions found.");
            return Ok(());
        }
        println!("{:<30} {:>6} {:>10}  started", "ID", "msgs", "size");
        println!("{}", "-".repeat(60));
        for e in &entries {
            println!(
                "{:<30} {:>6} {:>10}  {}",
                e.id,
                e.message_count,
                format!("{:.1} KB", e.size_bytes as f64 / 1024.0),
                e.started_at
            );
        }
        return Ok(());
    }

    let id = id.ok_or_else(|| anyhow::anyhow!("--export requires a session id"))?;
    let fmt = export.as_deref().unwrap_or("markdown");

    let path =
        resolve_session_id(&id)?.ok_or_else(|| anyhow::anyhow!("session '{id}' not found"))?;

    let content = match fmt {
        "ndjson" => std::fs::read_to_string(&path)?,
        "json" => {
            let (log, _) = ConversationLog::open(path)?;
            serde_json::to_string_pretty(log.all())?
        }
        "markdown" | "md" => {
            let (log, _) = ConversationLog::open(path)?;
            // Build ConversationEntry list from Message list for transcript formatter
            let entries: Vec<tui::app::ConversationEntry> = log
                .all()
                .iter()
                .map(|m| {
                    let role = match m.role {
                        shared::Role::User => "user",
                        shared::Role::Assistant => "assistant",
                        shared::Role::Tool => "tool",
                        shared::Role::System => "system",
                    };
                    tui::app::ConversationEntry::new(role, m.content.clone())
                })
                .collect();
            tui::transcript::format_transcript(&id, &entries)
        }
        other => anyhow::bail!("unknown export format '{other}'; use markdown, json, or ndjson"),
    };

    if let Some(p) = out_path {
        std::fs::write(&p, &content)?;
        println!("Exported {} session to {}", fmt, p.display());
    } else {
        print!("{content}");
    }

    Ok(())
}

struct RunArgs {
    model: Option<String>,
    host: Option<String>,
    model_type: Option<String>,
    auto_approve: bool,
    dry_run: bool,
    system: Option<String>,
    resume: Option<String>,
    non_interactive: bool,
    output: crate::shared::OutputFormat,
    max_turns: usize,
    continue_session: Option<String>,
    auto_resume: bool,
    attach: Option<String>,
    no_tui: bool,
}

async fn run_session(args: RunArgs) -> anyhow::Result<()> {
    let RunArgs {
        model,
        host,
        model_type,
        auto_approve,
        dry_run,
        system,
        resume,
        non_interactive,
        output,
        max_turns,
        continue_session,
        auto_resume,
        attach,
        no_tui,
    } = args;

    let mut config = session::config::load_or_create_config();

    if let Some(host) = &host {
        config.ollama_host = host.clone();
    }
    let model = model.unwrap_or_else(|| config.default_model.clone());
    if auto_approve {
        config.auto_approve = true;
    }
    if dry_run {
        config.dry_run = true;
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
                    "daemon could not resolve session '{id}'. Run `/sessions` to see available ids."
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
                sessions_dir.join(format!("{session_id}.conv.ndjson"))
            }
        }
    } else {
        // Try the daemon for a startup picker in TUI mode, or a hint in
        // non-interactive / no-TUI mode.
        match daemon::client::try_list_recent().await? {
            Some(sessions) if !sessions.is_empty() && !non_interactive && !no_tui => {
                match tui::run_session_picker(sessions).await? {
                    Some(path) => {
                        tracing::info!(path = %path.display(), "resuming selected session");
                        path
                    }
                    None => {
                        tracing::info!("user chose new session");
                        let sessions_dir = data_dir.join("sessions");
                        std::fs::create_dir_all(&sessions_dir)?;
                        sessions_dir.join(format!("{session_id}.conv.ndjson"))
                    }
                }
            }
            Some(sessions) if !sessions.is_empty() => {
                // In machine-readable output modes the hint would pollute
                // stderr that callers may capture; only show it in plain
                // text mode where a human is reading the terminal.
                if output == crate::shared::OutputFormat::Text {
                    print_recent_sessions_hint(&sessions);
                }
                let sessions_dir = data_dir.join("sessions");
                std::fs::create_dir_all(&sessions_dir)?;
                sessions_dir.join(format!("{session_id}.conv.ndjson"))
            }
            _ => {
                let sessions_dir = data_dir.join("sessions");
                std::fs::create_dir_all(&sessions_dir)?;
                sessions_dir.join(format!("{session_id}.conv.ndjson"))
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

    let (conversation, open_outcome) = session::conversation::ConversationLog::open(log_path)?;
    if let session::conversation::OpenOutcome::Restored(messages) = open_outcome {
        eprintln!("⚠️  Session log was corrupt; restored {messages} message(s) from checkpoint.");
    }

    let adapter = adapters::caching::maybe_wrap_cached(
        adapters::adapter_for(&model, ollama_host, model_type.as_deref()),
        &config,
    );

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

    // ── Built-in tool access controls ──
    // PathGuard / DenyList are required by the bash, grep, and glob tools so
    // they can enforce sandbox containment and deny-list checks at the tool
    // layer (e.g. background bash must re-check the command, grep/glob must
    // re-check each discovered file). Build them once from the resolved
    // launch-time config.
    let (builtin_deny_list, builtin_path_guard, _builtin_read_gate) =
        session::access::access_from_config(&config);
    let bash_sandbox_workdir = config.bash_sandbox_workdir;

    // ── Toolset assembly (Phase 2.2) ──
    // Compose built-in, MCP, and plugin tools into a single source-aware
    // collection. The executor receives the flattened vector, but order and
    // duplicate-name resolution are controlled here: built-ins win over MCP,
    // and MCP wins over plugins.
    let mut toolset = session::toolset::CompositeToolset::empty();
    toolset.add(Box::new(session::toolset::VecToolset::new(
        "builtin",
        tools::all_tools(
            undo_stack.clone(),
            adapter.model_info().supports_images,
            builtin_deny_list,
            builtin_path_guard,
            bash_sandbox_workdir,
        ),
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

    let tools = toolset.into_tools()?;

    if let Some(sys) = &system {
        // Wired into the executor's PromptBuilder before the first turn
        // (see tui::run_tui and run_non_interactive). Kept as an info
        // log so operators can confirm the override took effect.
        tracing::info!("System prompt set from CLI: {}", sys);
    }

    // If stdout is not a real terminal (piped, redirected, detached pty),
    // the TUI cannot render. Fall back to the same line-mode loop that
    // --non-interactive uses, but read from stdin instead of a pre-baked
    // prompt list so the user can still chat.
    let no_color =
        std::env::var("NO_COLOR").is_ok() || std::env::var("TERM").is_ok_and(|t| t == "dumb");
    let use_tui = !no_tui && !non_interactive && !no_color && std::io::stdout().is_terminal();
    if use_tui {
        tui::run_tui(
            shared_config,
            adapter,
            tools,
            (conversation, open_outcome),
            system,
            undo_stack,
            &plugin_registry,
        )
        .await
    } else {
        run_line_mode(
            shared_config,
            adapter,
            tools,
            (conversation, open_outcome),
            system,
            output,
            max_turns,
            non_interactive,
            &plugin_registry,
        )
        .await
    }
}

/// Spawn the approval responder used by non-interactive runs.
///
/// Non-interactive mode has no human in the loop, so every request
/// that reaches this channel is denied. The executor already auto-allows
/// read-only discovery tools and benign bash; anything that still needs
/// approval (non-read-only bash, explicit Deny rules, etc.) must be
/// rejected rather than silently approved.
fn spawn_non_interactive_approval_handler(
    mut approval_rx: mpsc::UnboundedReceiver<session::executor::ApprovalRequest>,
) {
    tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            tracing::warn!(
                tool = %req.tool_name,
                args = %req.args,
                "non-interactive run denied approval for tool; use interactive mode or add a permission rule that explicitly allows this operation"
            );
            let _ = req.response.send(session::executor::ApprovalResponse::DeniedWithReason(
                "non-interactive mode cannot approve destructive tools; use interactive mode or add a permission rule".into(),
            ));
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_line_mode(
    config: crate::shared::SharedConfig,
    adapter: Box<dyn adapters::ModelAdapter>,
    tools: Vec<Arc<dyn tools::Tool>>,
    conversation: (
        session::conversation::ConversationLog,
        session::conversation::OpenOutcome,
    ),
    system: Option<String>,
    output: crate::shared::OutputFormat,
    max_turns: usize,
    non_interactive: bool,
    plugin_registry: &kirkforge_plugin_host::PluginRegistry,
) -> anyhow::Result<()> {
    // If running in non-interactive mode (scripted), deny all approvals.
    // If running in line-mode interactive (no TUI), prompt on stderr and
    // read from /dev/tty so the user can actually approve or deny.
    let model_name = adapter.model_info().name.clone();

    let (conversation, open_outcome) = conversation;
    let mut executor = session::executor::Executor::with_log_and_undo_and_plugins(
        adapter,
        tools,
        config.clone(),
        conversation,
        None,
        None,
        Some(plugin_registry),
    );
    if let session::conversation::OpenOutcome::Restored(messages) = open_outcome {
        executor.set_recovered_messages(messages);
    }
    executor.set_system_override(system.clone());

    let (approval_tx, approval_rx) =
        mpsc::unbounded_channel::<session::executor::ApprovalRequest>();

    if non_interactive {
        spawn_non_interactive_approval_handler(approval_rx);
    } else {
        spawn_line_mode_approval_handler(approval_rx);
    }

    if let Some(sys) = &system {
        tracing::info!("System prompt set from CLI: {}", sys);
    }

    let cancelled = std::sync::atomic::AtomicBool::new(false);

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

        // Built-in slash commands in line mode (where there is no TUI
        // key handler to intercept them). This makes `/exit` and
        // `/quit` behave consistently with the TUI.
        let trimmed = input.trim();
        if trimmed == "/exit" || trimmed == "/quit" {
            if output == crate::shared::OutputFormat::Text {
                println!("Exiting.");
            }
            break;
        }

        let turn_started_at = std::time::Instant::now();
        let events = executor
            .run_turn_collecting(&input, &approval_tx, &cancelled)
            .await?;
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
        let _ = turn_outcome;
    }

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

/// Spawn an approval responder for interactive line mode.
///
/// When the TUI is disabled, destructive tool calls still need a human
/// decision. This handler prints the request to stderr and reads a line
/// from `/dev/tty` (the controlling terminal) so it does not compete
/// with stdin prompt reading. `y`/`yes` approves; anything else denies.
///
/// The read is performed on a detached OS thread with a timeout so the
/// process can still exit cleanly when stdin reaches EOF or the user
/// types `/exit`/`Ctrl+D`: a tokio `spawn_blocking` thread would keep
/// the runtime alive while it waits forever on a quiet terminal.
fn spawn_line_mode_approval_handler(
    mut approval_rx: mpsc::UnboundedReceiver<session::executor::ApprovalRequest>,
) {
    tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            let args_preview = match serde_json::to_string_pretty(&req.args) {
                Ok(s) => s,
                Err(_) => req.args.to_string(),
            };
            eprintln!();
            eprintln!("⚠️  Approval required: {}", req.tool_name);
            eprintln!("{args_preview}");
            eprint!("Approve? [y/N]: ");
            let _ = std::io::stderr().flush();

            let tool_name = req.tool_name.clone();
            let (answer_tx, answer_rx) = tokio::sync::oneshot::channel::<bool>();

            // Detached thread: reads /dev/tty and sends the answer back.
            std::thread::spawn(move || {
                let mut answer = String::new();
                let approved = match std::fs::OpenOptions::new().read(true).open("/dev/tty") {
                    Ok(mut tty) => {
                        use std::io::BufRead;
                        let mut reader = std::io::BufReader::new(&mut tty);
                        let _ = reader.read_line(&mut answer);
                        let trimmed = answer.trim().to_ascii_lowercase();
                        trimmed == "y" || trimmed == "yes"
                    }
                    Err(e) => {
                        tracing::warn!(
                            tool = %tool_name,
                            error = %e,
                            "line-mode approval: no /dev/tty available; denying"
                        );
                        false
                    }
                };
                // If the tokio side already timed out, this send is
                // harmless; the leftover thread will exit once the user
                // finally provides input or the process terminates.
                let _ = answer_tx.send(approved);
            });

            let approved = tokio::time::timeout(std::time::Duration::from_secs(120), answer_rx)
                .await
                .map(|r| r.unwrap_or(false))
                .unwrap_or_else(|_| {
                    eprintln!("\nApproval prompt timed out after 120 s; denying.");
                    false
                });

            let resp = if approved {
                session::executor::ApprovalResponse::Approved
            } else {
                session::executor::ApprovalResponse::Denied
            };
            let _ = req.response.send(resp);
        }
    });
}

/// Serialize a JSON value and emit it as one stream-json line.
///
/// `serde_json::to_string` can fail only for non-finite floats; if that
/// somehow happens (e.g. a corrupted cost value), we log a warning and
/// skip the line rather than panicking in the headless output path.
fn print_json_line(value: &serde_json::Value) {
    match serde_json::to_string(value) {
        Ok(line) => println!("{line}"),
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
                    print!("{t}");
                    let _ = std::io::stdout().flush();
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "token", "content": t});
                    print_json_line(&line);
                }
            }
            session::executor::TurnEvent::Thinking(t) => {
                if output == crate::shared::OutputFormat::Text {
                    eprintln!("\n[thinking] {t}");
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
                    // Keep non-interactive output compact: one line per tool,
                    // and only the body if it failed. Successful tool churn is
                    // the main source of terminal spam.
                    let status = if *success { "ok" } else { "FAIL" };
                    eprintln!("[tool {name} -> {status}]");
                    if !success {
                        eprintln!("{result}");
                    }
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
                    eprintln!("\n[error] {e}");
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
            session::executor::TurnEvent::Recovered { messages } => {
                if output == crate::shared::OutputFormat::Text {
                    eprintln!("\n[recovered] restored {messages} message(s) from checkpoint");
                } else if output == crate::shared::OutputFormat::StreamJson {
                    let line = serde_json::json!({"type": "recovered", "messages": messages});
                    print_json_line(&line);
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
                        "\n[compaction] {original_count} → {compacted_count} messages, dropped {dropped_tool_results} tool result(s), condensed {condensed_assistant_turns} assistant turn(s).",
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
            session::executor::TurnEvent::PullProgress { .. } => {
                // Non-interactive mode has no place to show a live
                // progress bar; swallow the event silently.
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
            "No saved session found matching '{value}'. Run `kirkforge run --non-interactive` once to create one, or use `/sessions` in the TUI to list."
        )),
        Err(e) => Err(anyhow::anyhow!(
            "Error resolving session id '{value}': {e}"
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
        assert!(r.is_err(), "expected an error, got: {r:?}");
        let err = r.unwrap_err().to_string();
        // Either "No saved session found" (empty sessions dir) or
        // a session-index error. Both indicate the id-resolution
        // path was taken, which is what we want to verify.
        assert!(
            err.contains("No saved session") || err.contains("Error resolving session id"),
            "unexpected error: {err}"
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

    /// The non-interactive approval handler must deny every request,
    /// even when global auto_approve is true. Otherwise it would bypass
    /// the executor's safety downgrade for non-read-only bash.
    #[tokio::test]
    async fn non_interactive_approval_handler_denies_all_requests() {
        let (tx, rx) = mpsc::unbounded_channel();
        spawn_non_interactive_approval_handler(rx);

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
            "expected a reasoned denial, got {resp:?}"
        );
    }
}
