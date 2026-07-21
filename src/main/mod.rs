// Stabilization lint: holding a std::sync lock guard across an .await point
// is a classic async Rust foot-gun that can deadlock the executor. The
// codebase currently passes this check; deny it to keep it that way.
#![deny(clippy::await_holding_lock)]
// Stabilization lint: unwrap() in production code can crash the TUI. Tests
// are allowed to unwrap for brevity; production code must use proper error
// handling or explicit expect() with a justification.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod chrome_launcher;
mod turn_events;

use clap::{CommandFactory, Parser};
use kirkforge::cli::{Cli, Command};
use kirkforge::{adapters, daemon, line_mode, session, shared, tools, tui};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing_subscriber::prelude::*;
use turn_events::{emit_turn_events, resolve_continue_path};

/// Initialize tracing so logs go to a file instead of corrupting the TUI.
///
/// In interactive (TUI) mode stdout is the alternate screen, so any
/// tracing output written there would be drawn over the UI. We always
/// write logs to `<data_dir>/kirkforge.log` and additionally mirror them
/// to stderr when `KIRKFORGE_LOG_STDERR=1` is set (useful for daemon or
/// non-interactive debugging).
fn init_tracing(log_level: &str) -> anyhow::Result<()> {
    // Writer enum so that a failure to open the log file falls back to
    // a null sink instead of panicking on `/dev/null`. The file is opened
    // once and shared behind a mutex; the old per-record `OpenOptions::open`
    // caused thousands of syscalls per turn under `RUST_LOG=debug`.
    enum LogWriter {
        File(std::sync::Arc<std::sync::Mutex<std::fs::File>>),
        Sink(std::io::Sink),
    }

    impl std::io::Write for LogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            match self {
                LogWriter::File(arc) => arc.lock().expect("log file mutex poisoned").write(buf),
                LogWriter::Sink(s) => s.write(buf),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            match self {
                LogWriter::File(arc) => arc.lock().expect("log file mutex poisoned").flush(),
                LogWriter::Sink(s) => s.flush(),
            }
        }
    }
    let env_filter = match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(f) => f,
        Err(_) => tracing_subscriber::EnvFilter::try_new(log_level)
            .map_err(|e| anyhow::anyhow!("invalid log level '{log_level}': {e}"))?,
    };

    let log_file = session::data_dir()
        .map(|d| d.join("kirkforge.log"))
        .unwrap_or_else(|_| PathBuf::from("kirkforge.log"));
    let log_dir = log_file
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    if let Err(e) = std::fs::create_dir_all(log_dir) {
        eprintln!(
            "failed to create log directory {}: {}",
            log_dir.display(),
            e
        );
    }

    // Open the log file once. Rotation-by-moving-aside is sacrificed for
    // performance; callers can copy/truncate the file in place instead.
    let file_handle: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>> =
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
        {
            Ok(file) => Some(std::sync::Arc::new(std::sync::Mutex::new(file))),
            Err(e) => {
                // Last-ditch fallback: write to stderr so logs aren't lost,
                // and route tracing into a null sink so the subscriber
                // still initializes even when `/dev/null` is unavailable
                // (e.g. in a sandboxed or Windows environment).
                eprintln!("failed to open log file {}: {}", log_file.display(), e);
                None
            }
        };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(move || match &file_handle {
            Some(arc) => LogWriter::File(std::sync::Arc::clone(arc)),
            None => LogWriter::Sink(std::io::sink()),
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
    Ok(())
}

/// Typed error categories used to pick a stable process exit code.
///
/// The previous `exit_code` implementation lowercased the error message and
/// matched substrings, which missed real sandbox denials that used phrases
/// such as "path is outside the allowed area" or "operation not permitted".
/// Centralising the classification in an enum makes the exit-code contract
/// explicit and easier to extend as more error sources become typed.
#[derive(Debug)]
enum KirkForgeError {
    /// Model/host unreachable or DNS/connection failure.
    ModelUnreachable(anyhow::Error),
    /// Permission denied, sandbox violation, or path blocked by policy.
    AccessDenied(anyhow::Error),
    /// Configuration file parsing or validation failure.
    ConfigParse(anyhow::Error),
    /// Any other failure.
    General(anyhow::Error),
}

impl std::fmt::Display for KirkForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KirkForgeError::ModelUnreachable(e)
            | KirkForgeError::AccessDenied(e)
            | KirkForgeError::ConfigParse(e)
            | KirkForgeError::General(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for KirkForgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KirkForgeError::ModelUnreachable(e)
            | KirkForgeError::AccessDenied(e)
            | KirkForgeError::ConfigParse(e)
            | KirkForgeError::General(e) => Some(e.as_ref()),
        }
    }
}

impl From<anyhow::Error> for KirkForgeError {
    fn from(e: anyhow::Error) -> Self {
        // TODO: as more library calls return typed errors, replace these
        // string probes with `downcast_ref` checks against concrete error types.
        let msg = format!("{e:#}").to_lowercase();
        if msg.contains("connection refused")
            || msg.contains("failed to connect")
            || msg.contains("dns error")
            || msg.contains("timed out")
            || msg.contains("model not found")
            || msg.contains("model unreachable")
        {
            KirkForgeError::ModelUnreachable(e)
        } else if msg.contains("denied")
            || msg.contains("permission")
            || msg.contains("sandbox")
            || msg.contains("blocked")
            || msg.contains("outside the allowed area")
            || msg.contains("not permitted")
        {
            KirkForgeError::AccessDenied(e)
        } else if msg.contains("config") && (msg.contains("parse") || msg.contains("invalid")) {
            KirkForgeError::ConfigParse(e)
        } else {
            KirkForgeError::General(e)
        }
    }
}

impl KirkForgeError {
    /// Structured exit code: 0 = success, 1 = general, 2 = bad args (clap),
    /// 3 = model unreachable, 4 = permission/sandbox denied, 5 = config parse error.
    fn exit_code(&self) -> i32 {
        match self {
            KirkForgeError::ModelUnreachable(_) => 3,
            KirkForgeError::AccessDenied(_) => 4,
            KirkForgeError::ConfigParse(_) => 5,
            KirkForgeError::General(_) => 1,
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = init_tracing(&cli.log_level) {
        eprintln!("{e:#}");
        std::process::exit(2);
    }

    if let Some(endpoint) = kirkforge::shared::metrics::init_telemetry() {
        tracing::info!(otel_endpoint = %endpoint, "OpenTelemetry export enabled");
    }

    let result: Result<(), KirkForgeError> = match cli.command {
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
            seed,
            worktree,
            docker,
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
                seed,
                worktree,
                docker,
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
        Command::Metrics => {
            let summary = kirkforge::shared::metrics::summarize();
            println!("{}", kirkforge::shared::metrics::format_summary(&summary));
            Ok(())
        }
        Command::Sessions {
            id,
            export,
            output,
            search,
        } => handle_sessions_command(id, export, output, search),
        Command::Daemon { foreground, stop } => {
            #[cfg(unix)]
            {
                daemon::server::run_daemon(foreground, stop).await
            }
            #[cfg(windows)]
            {
                let _ = (foreground, stop);
                Err(anyhow::anyhow!(
                    "session daemon is not supported on Windows"
                ))
            }
        }
        Command::Jobd { foreground, stop } => {
            #[cfg(unix)]
            {
                kirkforge::jobs::run_job_daemon(foreground, stop).await
            }
            #[cfg(windows)]
            {
                let _ = (foreground, stop);
                Err(anyhow::anyhow!(
                    "scheduled-job daemon is not supported on Windows"
                ))
            }
        }
        Command::Bench {
            tasks,
            model,
            output,
            summary,
            timeout,
        } => handle_bench_command(tasks, model, output, summary, timeout).await,
    }
    .map_err(KirkForgeError::from);

    kirkforge::shared::metrics::shutdown_telemetry();

    if let Err(e) = result {
        eprintln!("kirkforge: {e}");
        std::process::exit(e.exit_code());
    }
}

async fn handle_bench_command(
    tasks: std::path::PathBuf,
    model: Option<String>,
    output: Option<std::path::PathBuf>,
    summary: Option<std::path::PathBuf>,
    timeout: u64,
) -> anyhow::Result<()> {
    let config = kirkforge::session::config::load_or_create_config();
    let model_name = model.unwrap_or_else(|| config.default_model.clone());
    let bench_tasks = kirkforge_bench::load_tasks(&tasks)?;
    if bench_tasks.is_empty() {
        anyhow::bail!("no task files found in {}", tasks.display());
    }
    eprintln!(
        "running {} benchmark tasks with model {}",
        bench_tasks.len(),
        model_name
    );
    let report =
        kirkforge::session::bench::run_all(&bench_tasks, &model_name, &config, timeout).await;
    eprintln!(
        "{}/{} tasks passed ({:.0}%)",
        report.summary.tasks_passed,
        report.summary.tasks_run,
        report.summary.success_rate * 100.0
    );
    let json_path = output.unwrap_or_else(|| {
        std::path::PathBuf::from(format!(
            "bench-report-{}.json",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
    });
    kirkforge_bench::write_report(&report, &json_path)?;
    eprintln!("report written to {}", json_path.display());
    if let Some(md_path) = summary {
        kirkforge_bench::write_markdown_summary(&report, &md_path)?;
        eprintln!("summary written to {}", md_path.display());
    }
    Ok(())
}

fn handle_sessions_command(
    id: Option<String>,
    export: Option<String>,
    out_path: Option<PathBuf>,
    search: Option<String>,
) -> anyhow::Result<()> {
    use session::conversation::ConversationLog;
    use session::session_index::{list_sessions, resolve_session_id, search_sessions};

    // Search takes priority over list when no id/export is given.
    if let Some(query) = search {
        let entries = search_sessions(&query).unwrap_or_default();
        if entries.is_empty() {
            println!("No sessions matching '{query}'.");
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
    output: kirkforge::shared::OutputFormat,
    max_turns: usize,
    continue_session: Option<String>,
    auto_resume: bool,
    attach: Option<String>,
    no_tui: bool,
    seed: Option<u64>,
    worktree: bool,
    docker: bool,
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
        seed,
        worktree,
        docker,
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
    if let Some(seed) = seed {
        config.seed = Some(seed);
    }
    if worktree {
        config.worktree_enabled = true;
    }
    if docker {
        config.docker.enabled = true;
    }

    // CLI flags are transient runtime overrides; do not persist them to
    // config.toml. `load_or_create_config` already wrote a default file on
    // first run, and explicit in-session config changes are saved by their
    // respective handlers (e.g. /reload). Persisting here made a single
    // scripted invocation permanently flip `auto_approve` or `dry_run`.
    //
    // We keep the loaded/merged config object for the rest of the session.
    //
    // Previously: `session::config::save_config(&config)` was called here.

    // Honor `NO_COLOR` / `TERM=dumb` consistently across all user-facing
    // output, including the session-restoration message printed before the
    // TUI/line-mode branch is chosen.
    let no_color =
        std::env::var("NO_COLOR").is_ok() || std::env::var("TERM").is_ok_and(|t| t == "dumb");

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

    // ── Git worktree (--worktree flag) ──
    // When enabled, create an isolated git worktree for the session.
    // Edits land in the worktree, not the user's working tree.
    // The worktree is removed when `_worktree` is dropped.
    let _worktree: Option<session::worktree::WorktreeSession> = if config.worktree_enabled {
        let repo_root = std::env::current_dir()?;
        let wt = session::worktree::WorktreeSession::create(&session_id.to_string(), &repo_root)?;
        // Redirect sandbox to the worktree path
        config.sandbox_dir = Some(wt.path().to_string_lossy().to_string());
        // Also redirect the log path into the worktree
        Some(wt)
    } else {
        None
    };

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
                if output == kirkforge::shared::OutputFormat::Text {
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
    kirkforge::session::session_index::touch_session(&touch_id, &log_path);

    let (mut conversation, open_outcome) = session::conversation::ConversationLog::open(log_path)?;
    conversation = conversation.with_checkpoint_interval(config.checkpoint_interval_messages);
    if let session::conversation::OpenOutcome::Restored(messages) = open_outcome {
        let warn_icon = line_mode::symbol(no_color, "⚠️");
        let warn_sep = if warn_icon.is_empty() { "" } else { " " };
        eprintln!("{warn_icon}{warn_sep}Session log was corrupt; restored {messages} message(s) from checkpoint.");
    }

    let adapter = adapters::caching::maybe_wrap_cached(
        adapters::adapter_for_with_provider(
            &model,
            ollama_host,
            model_type.as_deref(),
            &config.anthropic_provider,
            config.request_timeout_secs,
        ),
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
    let minify_write_side = config.minify_write_side;
    let computer_use_cfg = config.computer_use.clone();
    let computer_use_enabled = computer_use_cfg.enabled;

    // ── LSP pool (lazy-started, fail-cooled) ──
    // Build the pool from `[[lsp_servers]]` config. Servers are spawned
    // lazily on the first `lsp_query` call for that language, so this is
    // cheap when no LSP-aware tool runs. The pool is wrapped in `Arc` and
    // shared with the `lsp_query` tool below.
    let lsp_pool: Option<std::sync::Arc<kirkforge_lsp::LspPool>> = if config.lsp_servers.is_empty()
    {
        None
    } else {
        let language_configs: Vec<kirkforge_lsp::LanguageConfig> = config
            .lsp_servers
            .iter()
            .map(|e| kirkforge_lsp::LanguageConfig {
                name: e.language.clone(),
                extensions: e.extensions.clone(),
                lsp: Some(kirkforge_lsp::LspServerConfig {
                    command: e.command.clone(),
                    args: e.args.clone(),
                }),
            })
            .collect();
        Some(std::sync::Arc::new(kirkforge_lsp::LspPool::new(
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            language_configs,
        )))
    };

    // ── Chrome tab for computer_use ──
    // Try to launch Chrome only when the tool is enabled. If the launch fails,
    // fall back to a placeholder tab that fails gracefully at runtime. This
    // keeps the toolset construction cheap and avoids hard-failing startup
    // when Chrome is not installed.
    let chrome_tab: std::sync::Arc<dyn crate::tools::computer_use::ChromeTab> =
        if computer_use_enabled {
            match chrome_launcher::launch_chrome_tab(&config.computer_use).await {
                Ok(tab) => tab,
                Err(e) => {
                    tracing::warn!(error = %e, "computer_use enabled but Chrome launch failed; tool will fail gracefully");
                    std::sync::Arc::new(crate::tools::computer_use::PlaceholderTab)
                }
            }
        } else {
            std::sync::Arc::new(crate::tools::computer_use::PlaceholderTab)
        };

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
            minify_write_side,
            lsp_pool.clone(),
            Some((computer_use_enabled, computer_use_cfg.clone())),
            Some(chrome_tab),
            Some(config.docker.clone()),
        ),
    )));

    // ── Shared config (hot-reload foundation) ──
    // Wrap the launch-time Config in an Arc<RwLock> so both TUI and
    // executor can observe live updates from SIGHUP or `/reload`.
    let shared_config = std::sync::Arc::new(std::sync::RwLock::new(config));

    // ── Repo-graph context index (P1-long-1) ──
    // Build a tree-sitter-backed symbol index from the sandbox directory.
    // The index is passed to the executor's PromptBuilder so relevant
    // symbols are injected into the system prompt before every turn.
    let context_index = {
        let cfg = kirkforge::shared::read_shared_config(&shared_config);
        cfg.sandbox_dir.as_ref().and_then(|dir| {
            let path = std::path::Path::new(dir);
            if path.is_dir() {
                let mut idx = kirkforge_context_index::ContextIndex::new();
                match idx.index_dir(path) {
                    Ok(()) => {
                        let count = idx.symbols().len();
                        tracing::info!(symbol_count = count, sandbox_dir = %dir, "built repo-graph context index");
                        Some(idx)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, sandbox_dir = %dir, "failed to build context index");
                        None
                    }
                }
            } else {
                None
            }
        })
    };

    // --- MCP tools ---
    let cfg_for_mcp = kirkforge::shared::read_shared_config(&shared_config).clone();
    if !cfg_for_mcp.mcp_servers.is_empty() {
        let mcp_mgr = session::mcp_client::McpClientManager::new(&cfg_for_mcp.mcp_servers).await;
        for warning in mcp_mgr.warnings() {
            eprintln!("MCP warning: {warning}");
            tracing::warn!(warning = %warning, "MCP startup warning");
        }
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
    let cfg_for_plugins = kirkforge::shared::read_shared_config(&shared_config).clone();
    let (plugin_registry, plugin_warnings) =
        match session::plugin_tools::load_plugin_registry(&cfg_for_plugins) {
            Ok(rw) => rw,
            Err(e) => {
                eprintln!("Warning: failed to load plugin registry: {e:#}");
                (kirkforge_plugin_host::PluginRegistry::new(), vec![])
            }
        };
    let plugin_tools =
        session::plugin_tools::all_plugin_tools(&plugin_registry, shared_config.clone());
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
        eprintln!("Plugin warning: {w}");
        tracing::warn!(warning = %w, "plugin load warning");
    }

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
    let use_tui = !no_tui && !non_interactive && !no_color && std::io::stdout().is_terminal();
    if use_tui {
        tui::run_tui(
            shared_config,
            adapter,
            toolset,
            (conversation, open_outcome),
            system,
            undo_stack,
            &plugin_registry,
            context_index,
        )
        .await
    } else {
        run_line_mode(
            shared_config,
            adapter,
            toolset,
            (conversation, open_outcome),
            system,
            output,
            max_turns,
            non_interactive,
            no_color,
            &plugin_registry,
            session_id.to_string(),
            context_index,
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
            kirkforge::send_or_warn!(req.response.send(session::executor::ApprovalResponse::DeniedWithReason(
                "non-interactive mode cannot approve destructive tools; use interactive mode or add a permission rule".into(),
            )), "approval response receiver dropped; response discarded");
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_line_mode(
    config: kirkforge::shared::SharedConfig,
    adapter: Box<dyn adapters::ModelAdapter>,
    tools: kirkforge::session::toolset::CompositeToolset,
    conversation: (
        session::conversation::ConversationLog,
        session::conversation::OpenOutcome,
    ),
    system: Option<String>,
    output: kirkforge::shared::OutputFormat,
    max_turns: usize,
    non_interactive: bool,
    no_color: bool,
    plugin_registry: &kirkforge_plugin_host::PluginRegistry,
    session_id: String,
    context_index: Option<kirkforge_context_index::ContextIndex>,
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
    executor.set_session_id(session_id);
    if let session::conversation::OpenOutcome::Restored(messages) = open_outcome {
        executor.set_recovered_messages(messages);
    }
    executor.set_system_override(system.clone());

    // Attach the repo-graph context index if one was built.
    if let Some(idx) = context_index {
        executor.set_context_index(idx);
    }

    let (approval_tx, approval_rx) =
        mpsc::unbounded_channel::<session::executor::ApprovalRequest>();

    if non_interactive {
        spawn_non_interactive_approval_handler(approval_rx);
    } else {
        spawn_line_mode_approval_handler(approval_rx, no_color);
    }

    if let Some(sys) = &system {
        tracing::info!("System prompt set from CLI: {}", sys);
    }

    let cancelled = std::sync::atomic::AtomicBool::new(false);

    let mut line_reader = line_mode::LineReader::new(!non_interactive)?;
    let mut turn_no: usize = 0;
    let mut total_prompt_tokens: usize = 0;
    let mut total_completion_tokens: usize = 0;
    let mut cumulative_cost: f64 = 0.0;
    let mut all_tool_records: Vec<kirkforge::shared::ToolCallRecord> = Vec::new();
    let mut final_error: Option<String> = None;
    let overall_started = std::time::Instant::now();

    while let Some(input) = line_reader.next_line().await? {
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
            if output == kirkforge::shared::OutputFormat::Text {
                println!("Exiting.");
            }
            break;
        }

        if trimmed == "/reload plugins" {
            let cfg = kirkforge::shared::read_shared_config(&config).clone();
            match session::plugin_tools::load_plugin_registry(&cfg) {
                Ok((registry, warnings)) => {
                    let summary = executor.reload_plugins(&registry);
                    if output == kirkforge::shared::OutputFormat::Text {
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
                        if output == kirkforge::shared::OutputFormat::Text {
                            println!("Usage: /workflow run <name>");
                        }
                    } else {
                        let path = match kirkforge_workflow::find_workflow_file(rest) {
                            Some(p) => p,
                            None => {
                                if output == kirkforge::shared::OutputFormat::Text {
                                    println!("Workflow '{rest}' not found.");
                                }
                                continue;
                            }
                        };
                        match kirkforge_workflow::Workflow::from_file(&path) {
                            Ok(workflow) => {
                                let cfg = kirkforge::shared::read_shared_config(&config).clone();
                                let ollama_host = cfg.ollama_host.clone();
                                let supports_images = cfg.ollama_host.contains("localhost")
                                    || cfg.ollama_host.contains("127.0.0.1")
                                    || cfg.ollama_host.contains("[::1]");
                                let cancel =
                                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                                let workflow_name = workflow.name.clone();
                                let step_count = workflow.steps.len();
                                if output == kirkforge::shared::OutputFormat::Text {
                                    println!("🚀 Started workflow '{workflow_name}' ({step_count} steps).");
                                }
                                let runner = kirkforge::tui::commands::workflow::LineStepRunner {
                                    model_name: model_name.clone(),
                                    ollama_host,
                                    config: cfg,
                                    supports_images,
                                    undo_stack: None,
                                };
                                let result = kirkforge_workflow::WorkflowExecutor::new(workflow)
                                    .run(&runner, Some(&cancel))
                                    .await;
                                match result {
                                    Ok(summary) => {
                                        if output == kirkforge::shared::OutputFormat::Text {
                                            let s =
                                                kirkforge::tui::commands::workflow::format_summary(
                                                    &workflow_name,
                                                    &summary,
                                                );
                                            println!("{s}");
                                        }
                                    }
                                    Err(e) => {
                                        if output == kirkforge::shared::OutputFormat::Text {
                                            println!("Workflow failed: {e}");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                if output == kirkforge::shared::OutputFormat::Text {
                                    println!("Failed to load workflow '{rest}': {e}");
                                }
                            }
                        }
                    }
                }
                "status" => {
                    if output == kirkforge::shared::OutputFormat::Text {
                        println!("No workflow is currently running. Use /workflow run <name>.");
                    }
                }
                "cancel" => {
                    if output == kirkforge::shared::OutputFormat::Text {
                        println!("⛔ Workflow cancelled.");
                    }
                }
                _ => {
                    if output == kirkforge::shared::OutputFormat::Text {
                        println!("Usage: /workflow run <name> | status | cancel");
                    }
                }
            }
            continue;
        }

        if trimmed == "/reload skills" {
            // Line mode has no AppState skill registry; just report that the
            // interactive skill reload is a TUI-only feature.
            if output == kirkforge::shared::OutputFormat::Text {
                let icon = line_mode::symbol(no_color, "🧠");
                let sep = if icon.is_empty() { "" } else { " " };
                println!("{icon}{sep}Skill reload is only available in the TUI. Use /help to see available line-mode commands.");
            }
            continue;
        }

        if trimmed == "/carryover show" || trimmed == "/carryover" {
            let profile = session::carryover::load_carryover();
            if output == kirkforge::shared::OutputFormat::Text {
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
            if output == kirkforge::shared::OutputFormat::Text {
                println!("Carryover profile cleared.");
            }
            continue;
        }

        if trimmed == "/help" || trimmed == "/h" || trimmed == "/?" {
            if output == kirkforge::shared::OutputFormat::Text {
                println!("Built-in line-mode commands:");
                println!("  /exit, /quit          Exit the session");
                println!("  /reload               Reload config.toml");
                println!("  /reload plugins       Re-scan plugin directory");
                println!("  /carryover            Show or clear cross-session carryover");
                println!("  /help                 Show this help");
            }
            continue;
        }

        let turn_started_at = std::time::Instant::now();
        let events = executor
            .run_turn_collecting(&input, &approval_tx, &cancelled)
            .await?;
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
    }

    if turn_no == 0 && system.is_none() {
        tracing::warn!("No input provided. Pipe a prompt or use --system.");
        return Ok(());
    }

    if output == kirkforge::shared::OutputFormat::Text {
        println!();
    }

    if output == kirkforge::shared::OutputFormat::Json {
        let total_duration_ms = overall_started.elapsed().as_millis() as u64;
        let recorded_messages: Vec<_> = executor.conversation_log().all().to_vec();
        let summary = kirkforge::shared::SessionSummary {
            version: "1.0".into(),
            session: kirkforge::shared::SessionInfo {
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
            usage: kirkforge::shared::UsageSummary {
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
            // On Windows the same pollable abstraction races a blocking stdin
            // reader against the shutdown flag and returns `None` when
            // interrupted, so the same timeout/gate logic applies.
            let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let shutdown_reader = shutdown.clone();
            let reader_handle: std::thread::JoinHandle<()> = std::thread::spawn(move || {
                let approved =
                    read_approval_answer_pollable(&tool_name, &shutdown_reader).unwrap_or(false);
                // If the tokio side already timed out, `answer_rx` was dropped
                // and this send is harmless.
                kirkforge::send_or_warn!(
                    answer_tx.send(approved),
                    "line-mode answer channel receiver dropped"
                );
            });

            let result = tokio::time::timeout(std::time::Duration::from_secs(120), answer_rx).await;
            if result.is_err() {
                // Signal the poll loop to exit, then join so the thread is
                // reclaimed rather than lingering until the next input.
                shutdown.store(true, std::sync::atomic::Ordering::Release);
                eprintln!("\nApproval prompt timed out after 120 s; denying.");
            }
            // Always join: on the answer path the thread has already exited
            // (instant); on the timeout path it exits within one poll interval.
            let _ = reader_handle.join();

            let approved = result.map(|r| r.unwrap_or(false)).unwrap_or(false);

            let resp = if approved {
                session::executor::ApprovalResponse::Approved
            } else {
                session::executor::ApprovalResponse::Denied
            };
            kirkforge::send_or_warn!(
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

/// Print a hint listing recent sessions when running non-interactively
/// without an explicit resume target.
fn print_recent_sessions_hint(sessions: &[kirkforge::session::session_index::SessionEntry]) {
    eprintln!("Recent sessions (run with --auto-resume or --attach <id> to resume):");
    for (i, e) in sessions
        .iter()
        .enumerate()
        .take(kirkforge::daemon::RECENT_SESSIONS_LIMIT)
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

        // Let the thread enter its poll loop.
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
}
