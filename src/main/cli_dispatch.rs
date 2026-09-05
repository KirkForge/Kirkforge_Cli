// CLI entry point: tracing init + clap arg parse + subcommand dispatch.
// Extracted from the binary root — pure move, no behaviour change.

use clap::{CommandFactory, Parser};
use kf_code::cli::Command;
use std::path::PathBuf;
use tracing_subscriber::prelude::*;

use super::error::KirkForgeError;
// devtools-gated handlers (WO 47.5).
#[cfg(feature = "devtools")]
use super::handle_bench::handle_bench_command;
#[cfg(feature = "devtools")]
use super::handle_doctor::handle_doctor_command;
use super::handle_plugin::handle_plugin_command;
use super::handle_replay::handle_replay_command;
use super::handle_sessions::handle_sessions_command;
use super::handle_update::handle_update_command;
use super::run_session::{run_session, RunArgs};

/// Initialize tracing so logs go to a file instead of corrupting the TUI.
///
/// In interactive (TUI) mode stdout is the alternate screen, so any
/// tracing output written there would be drawn over the UI. We always
/// write logs to `<data_dir>/kf-code.log` and additionally mirror them
/// to stderr when `KF_CODE_LOG_STDERR=1` is set (useful for daemon or
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
                // WO 47.36: recover from poison — logging must never abort.
                LogWriter::File(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).write(buf),
                LogWriter::Sink(s) => s.write(buf),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            match self {
                LogWriter::File(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).flush(),
                LogWriter::Sink(s) => s.flush(),
            }
        }
    }
    let env_filter = match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(f) => f,
        Err(_) => tracing_subscriber::EnvFilter::try_new(log_level)
            .map_err(|e| anyhow::anyhow!("invalid log level '{log_level}': {e}"))?,
    };

    let log_file = kf_code::session::data_dir()
        .map(|d| d.join("kf-code.log"))
        .unwrap_or_else(|_| PathBuf::from("kf-code.log"));
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
    let file_handle: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>> = {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        // ponytail: Windows append path is unprotected against a
        // pre-created symlink — O_NOFOLLOW is Posix-only; upgrade path
        // is a per-component openat2(RESOLVE_NO_SYMLINKS) walk.
        match opts.open(&log_file) {
            Ok(file) => Some(std::sync::Arc::new(std::sync::Mutex::new(file))),
            Err(e) => {
                // Last-ditch fallback: write to stderr so logs aren't lost,
                // and route tracing into a null sink so the subscriber
                // still initializes even when `/dev/null` is unavailable
                // (e.g. in a sandboxed or Windows environment).
                eprintln!("failed to open log file {}: {}", log_file.display(), e);
                None
            }
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

    if std::env::var("KF_CODE_LOG_STDERR").is_ok() {
        let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
        registry.with(stderr_layer).init();
    } else {
        registry.init();
    }
    Ok(())
}

#[tokio::main]
pub async fn main() {
    let cli = kf_code::cli::Cli::parse();
    if let Err(e) = init_tracing(&cli.log_level) {
        eprintln!("{e:#}");
        std::process::exit(2);
    }

    if let Some(endpoint) = kf_code::shared::metrics::init_telemetry() {
        tracing::info!(otel_endpoint = %endpoint, "OpenTelemetry export enabled");
    }

    // WO 38.10 P2: capture the `--output` format for the error path so a
    // hard error in `--output json` mode emits `{"error": ...}` to stdout
    // before exiting (previously it printed nothing to stdout). Only the
    // `run` subcommand carries an output format; other subcommands print
    // human-readable text and exit via the same error path.
    let error_output_format = match &cli.command {
        Command::Run { output, .. } => Some(*output),
        _ => None,
    };

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
            harden,
            no_network,
            block_edits,
            i_accept_unsandboxed,
            no_trace,
            prompt,
            read_stdin_full,
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
                harden,
                no_network,
                block_edits,
                i_accept_unsandboxed,
                no_trace,
                prompt,
                read_stdin_full,
            })
            .await
        }
        Command::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut kf_code::cli::Cli::command(),
                "kf-code",
                &mut std::io::stdout(),
            );
            Ok(())
        }
        Command::Metrics => {
            let summary = kf_code::shared::metrics::summarize();
            println!("{}", kf_code::shared::metrics::format_summary(&summary));
            Ok(())
        }
        Command::Verify => {
            // WO 11.7: print recent verifier verdicts from the metrics log.
            println!("{}", kf_code::shared::metrics::format_verifier_report(20));
            Ok(())
        }
        Command::Sessions {
            id,
            export,
            output,
            search,
        } => handle_sessions_command(id, export, output, search),
        // daemon-gated (WO 47.12): the Daemon/Jobd variants only exist
        // with --features daemon; default builds never match these arms.
        #[cfg(feature = "daemon")]
        Command::Daemon { foreground, stop } => {
            #[cfg(unix)]
            {
                kf_code::daemon::server::run_daemon(foreground, stop).await
            }
            #[cfg(windows)]
            {
                let _ = (foreground, stop);
                Err(anyhow::anyhow!(
                    "session daemon is not supported on Windows"
                ))
            }
        }
        #[cfg(feature = "daemon")]
        Command::Jobd { foreground, stop } => {
            #[cfg(unix)]
            {
                kf_code::jobs::run_job_daemon(foreground, stop).await
            }
            #[cfg(windows)]
            {
                let _ = (foreground, stop);
                Err(anyhow::anyhow!(
                    "scheduled-job daemon is not supported on Windows"
                ))
            }
        }
        Command::Replay {
            id,
            data_dir,
            turn,
            from,
            to,
            interactive,
        } => handle_replay_command(id, data_dir, turn, from, to, interactive),
        // devtools-gated subcommands (WO 47.5) — variants only exist when
        // the binary is built with --features devtools.
        #[cfg(feature = "devtools")]
        Command::Bench { command } => handle_bench_command(command).await,
        Command::Plugin { command } => handle_plugin_command(command),
        #[cfg(feature = "devtools")]
        Command::Doctor { command } => handle_doctor_command(command),
        Command::Update { check } => handle_update_command(check).await,
        Command::Config => handle_config_command(),
        Command::Models => handle_models_command().await,
    }
    .map_err(KirkForgeError::from);

    kf_code::shared::metrics::shutdown_telemetry();

    if let Err(e) = result {
        // WO 38.10 P2: in `--output json` mode, emit a machine-readable
        // error object to stdout before the human-readable stderr line so
        // scripted consumers can capture the failure reason. The exit
        // code is unchanged (the classifier picks it).
        if let Some(kf_code::cli::OutputFormat::Json) = error_output_format {
            let exit_code = e.exit_code();
            let err_obj = serde_json::json!({
                "error": format!("{e:#}"),
                "exit_code": exit_code,
            });
            println!("{}", serde_json::to_string(&err_obj).unwrap_or_else(|_| "{}".into()));
        }
        eprintln!("kf-code: {e}");
        if let Some(h) = e.hint() {
            eprintln!("hint: {h}");
        }
        std::process::exit(e.exit_code());
    }
}

/// `kf-code config` (WO 38.10 P2): print the resolved config as TOML.
/// The config is loaded via the strict loader so a parse failure exits 5
/// (same as `run`) instead of silently showing defaults.
fn handle_config_command() -> anyhow::Result<()> {
    let config = kf_code::session::config::load_or_create_config_strict()?;
    let toml = toml::to_string_pretty(&config)
        .map_err(|e| anyhow::anyhow!("failed to serialize config as TOML: {e}"))?;
    println!("{toml}");
    Ok(())
}

/// `kf-code models` (WO 38.10 P2): list available models. For the
/// configured `default_model`'s adapter kind, Ollama queries
/// `{ollama_host}/api/tags`; cloud providers print the built-in pricing
/// table. Keeps it simple — no per-provider API calls for cloud.
async fn handle_models_command() -> anyhow::Result<()> {
    use kf_code::adapters::adapter_kind_for;

    let config = kf_code::session::config::load_or_create_config_strict()?;
    let model_name = &config.model.default_model;
    let ollama_host = &config.model.ollama_host;
    let kind = adapter_kind_for(model_name, None, &config.model.anthropic_provider);

    match kind {
        kf_code::adapters::AdapterKind::Ollama => {
            let client = kf_code::shared::build_reqwest_client(Some(std::time::Duration::from_secs(
                5,
            )));
            let models = kf_code::tui::commands::fetch_model_list(&client, ollama_host)
                .await
                .map_err(|e| anyhow::anyhow!("failed to query {ollama_host}/api/tags: {e}"))?;
            if models.is_empty() {
                println!("No models found at {ollama_host}/api/tags.");
            } else {
                for m in models {
                    println!("{m}");
                }
            }
        }
        _ => {
            // Cloud providers: list the built-in pricing table model
            // prefixes (non-empty rows only). This is the static catalogue
            // — a live cloud list call would need provider auth and is out
            // of scope for the simple `models` subcommand.
            println!("Configured provider: {kind} (model: {model_name})");
            println!();
            println!("Built-in pricing table models:");
            for p in kf_code::shared::PRICING_TABLE
                .iter()
                .filter(|p| !p.model_prefix.is_empty())
            {
                println!(
                    "  {:<24} in ${:.2}/Mtok  out ${:.2}/Mtok",
                    p.model_prefix, p.input_per_mtok, p.output_per_mtok
                );
            }
        }
    }
    Ok(())
}
