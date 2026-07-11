#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

//! `stratum` binary: command-line interface to the Stratum compression and
//! rules pipeline.

mod cli;
mod config_source;
mod init;
mod input;
mod mode;
mod output;
mod report;
mod stdout;

use anyhow::Context;
use clap::{CommandFactory, Parser};
use kirkstratum_core::content::{detect_content_type, ContentType};
use kirkstratum_core::mode::Mode;
use kirkstratum_core::pipeline::{CompressionContext, CompressionPipeline};
use kirkstratum_core::store::InMemoryOffloadStore;
use kirkstratum_hosts::build_rules;
use std::path::PathBuf;
use tracing::{debug, info, instrument};

use cli::{load_config, Cli, Command, ProcessEnv};
use config_source::ConfigSource;
use init::initialise_config;
use input::{max_input_size, read_input};
use mode::resolve_mode_with_override;
use output::emit_json_or_human;
use report::DryRunReport;
use stdout::write_stdout;

/// Exit codes following BSD sysexits(3) where applicable.
mod exit {
    /// Successful completion.
    pub const EX_OK: i32 = 0;
    /// Command-line usage error.
    pub const EX_USAGE: i32 = 64;
    /// Input data error (e.g. input too large).
    pub const EX_DATAERR: i32 = 65;
    /// Input file could not be opened or read.
    pub const EX_NOINPUT: i32 = 66;
    /// Internal software error.
    pub const EX_SOFTWARE: i32 = 70;
    /// Configuration file error.
    pub const EX_CONFIG: i32 = 78;
}

fn init_tracing(verbose: u8, quiet: u8) {
    let base = match (verbose, quiet) {
        (_, q) if q >= 2 => tracing::Level::ERROR,
        (_, 1) => tracing::Level::WARN,
        (0, 0) => tracing::Level::INFO,
        (1, _) => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(base.into())
                .from_env_lossy(),
        )
        .init();
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");
        tracing::error!(%payload, location = ?info.location(), "panic");
        print_panic_message(payload);
    }));
}

/// Print a panic message to stderr, ignoring `BrokenPipe` so that a panic in a
/// pipeline context does not abort the process just because stderr was closed.
fn print_panic_message(payload: &str) {
    use std::io::{self, Write};
    let mut stderr = io::stderr().lock();
    if let Err(err) = writeln!(stderr, "stratum: internal error: {payload}") {
        if err.kind() != io::ErrorKind::BrokenPipe {
            let _ = stderr.write_all(b"stratum: failed to print panic message\n");
        }
    }
}

fn main() {
    install_panic_hook();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let code = match e.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                    exit::EX_OK
                }
                _ => exit::EX_USAGE,
            };
            print_clap_error(&e);
            std::process::exit(code);
        }
    };

    init_tracing(cli.verbose, cli.quiet);

    info!(command = ?cli.command, "starting stratum");

    match run(&cli) {
        Ok(()) => {
            info!("stratum completed");
            std::process::exit(exit::EX_OK);
        }
        Err(e) => {
            let code = exit_code(&e);
            tracing::error!(error = ?e, code, "stratum failed");
            print_error_message(&e);
            std::process::exit(code);
        }
    }
}

/// Emit a clap error to its intended stream, ignoring `BrokenPipe` so that
/// shell pipelines like `stratum --help | head` exit cleanly instead of panicking.
fn print_clap_error(e: &clap::Error) {
    use std::io::{self, Write};
    if let Err(err) = e.print() {
        if err.kind() != io::ErrorKind::BrokenPipe {
            let mut stderr = io::stderr().lock();
            let _ = stderr.write_all(b"stratum: failed to print usage information\n");
        }
    }
}

/// Print the final error message to stderr, ignoring `BrokenPipe` so that
/// pipelines like `stratum run 2>&1 | head` do not panic when the downstream
/// reader closes early.
fn print_error_message(e: &anyhow::Error) {
    use std::io::{self, Write};
    let mut stderr = io::stderr().lock();
    if let Err(err) = writeln!(stderr, "stratum: {e:#}") {
        if err.kind() != io::ErrorKind::BrokenPipe {
            let _ = stderr.write_all(b"stratum: failed to print error message\n");
        }
    }
}

fn exit_code(e: &anyhow::Error) -> i32 {
    if e.downcast_ref::<kirkstratum_core::config::ConfigError>()
        .is_some()
    {
        return exit::EX_CONFIG;
    }
    if e.root_cause()
        .downcast_ref::<input::InputTooLarge>()
        .is_some()
    {
        return exit::EX_DATAERR;
    }
    if let Some(io) = e.root_cause().downcast_ref::<std::io::Error>() {
        return match io.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => exit::EX_NOINPUT,
            _ => exit::EX_SOFTWARE,
        };
    }
    if e.is::<clap::Error>() {
        return exit::EX_USAGE;
    }
    exit::EX_SOFTWARE
}

fn compression_context(cli: &Cli) -> CompressionContext {
    let mut ctx = CompressionContext::default();
    if let Some(budget) = cli.token_budget {
        ctx = ctx.with_token_budget(budget);
    }
    ctx
}

#[instrument(skip(cli), fields(command = ?cli.command))]
fn run(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Run => {
            let mode = resolve_mode_with_override(cli, None);
            execute_pipeline(cli, None, None, mode)?;
        }
        Command::Apply {
            ref file,
            ref content_type,
            ref mode,
        } => {
            let mode = resolve_mode_with_override(cli, *mode);
            execute_pipeline(cli, file.clone(), *content_type, mode)?;
        }
        Command::Mode { value } => {
            let mode = resolve_mode_with_override(cli, *value);
            let payload = serde_json::json!({ "mode": mode.as_str() });
            emit_json_or_human(cli.json, &format!("{mode}\n"), &payload)?;
        }
        Command::Rules { mode } => {
            let mode = resolve_mode_with_override(cli, *mode);
            let rules = build_rules(mode);
            let payload = serde_json::json!({ "mode": mode.as_str(), "rules": rules });
            emit_json_or_human(cli.json, &format!("{rules}\n"), &payload)?;
        }
        Command::Version => {
            let version = env!("CARGO_PKG_VERSION");
            let payload = serde_json::json!({ "version": version });
            emit_json_or_human(cli.json, &format!("stratum {version}\n"), &payload)?;
        }
        Command::Config { validate, sources } => {
            let cfg_and_sources = load_config(cli, &ProcessEnv);
            if *validate {
                match &cfg_and_sources {
                    Ok(_) => {
                        let payload = serde_json::json!({ "valid": true });
                        emit_json_or_human(cli.json, "config is valid\n", &payload)?;
                    }
                    Err(e) => {
                        let payload = serde_json::json!({
                            "valid": false,
                            "error": format!("{e:#}"),
                        });
                        emit_json_or_human(cli.json, "config is invalid\n", &payload)?;
                    }
                }
                cfg_and_sources?;
            } else if *sources {
                let (_, config_sources) = load_config(cli, &ProcessEnv)?;
                let human = config_sources
                    .iter()
                    .map(ConfigSource::to_human)
                    .collect::<Vec<_>>()
                    .join("\n");
                let payload = serde_json::json!({
                    "sources": config_sources
                        .iter()
                        .map(|s| serde_json::json!({
                            "kind": s.kind(),
                            "description": s.to_human(),
                        }))
                        .collect::<Vec<_>>()
                });
                emit_json_or_human(cli.json, &format!("{human}\n"), &payload)?;
            } else {
                let (cfg, _) = cfg_and_sources?;
                let cfg_value = serde_json::to_value(&cfg)?;
                emit_json_or_human(cli.json, &toml::to_string_pretty(&cfg)?, &cfg_value)?;
            }
        }
        Command::Completion { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            let mut buf = Vec::new();
            clap_complete::generate(*shell, &mut cmd, name, &mut buf);
            write_stdout(&String::from_utf8_lossy(&buf))
                .with_context(|| "failed to write completion script")?;
        }
        Command::Init { force } => {
            let path = initialise_config(&ProcessEnv, cli.config_dir.as_deref(), *force)?;
            info!(path = %path.display(), "initialised config");
            let payload = serde_json::json!({ "path": path });
            emit_json_or_human(
                cli.json,
                &format!("initialised config at {}\n", path.display()),
                &payload,
            )?;
        }
    }

    Ok(())
}

#[instrument(skip(cli), fields(mode = ?mode, content_type = ?forced_type, has_file = file.is_some()))]
fn execute_pipeline(
    cli: &Cli,
    file: Option<PathBuf>,
    forced_type: Option<ContentType>,
    mode: Mode,
) -> anyhow::Result<()> {
    let (cfg, _sources) = load_config(cli, &ProcessEnv)?;
    debug!(?cfg, "loaded effective config");
    let ctx = compression_context(cli);
    let max_size = max_input_size(cli.max_input_size);
    let input = read_input(file, max_size)?;
    let content_type = forced_type.unwrap_or_else(|| detect_content_type(&input));
    debug!(?content_type, ?mode, "resolved content type and mode");

    if cli.dry_run {
        let report = DryRunReport::new(&input, content_type, &ctx, &cfg, mode, max_size);
        emit_json_or_human(cli.json, &report.human(), &report.to_json())?;
    } else {
        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let output = pipeline.run(&input, content_type, &ctx, &store, &cfg, mode);
        write_stdout(&output).with_context(|| "failed to write pipeline output")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_clap_error_does_not_panic() {
        let err = Cli::command().error(clap::error::ErrorKind::UnknownArgument, "test error");
        print_clap_error(&err);
    }

    #[test]
    fn exit_code_maps_input_too_large_top_level_to_dataerr() {
        let err = anyhow::Error::from(input::InputTooLarge::new(16, 64));
        assert_eq!(exit_code(&err), exit::EX_DATAERR);
    }

    #[test]
    fn exit_code_maps_input_too_large_root_cause_to_dataerr() {
        let inner = input::InputTooLarge::new(16, 64);
        let err = anyhow::Error::from(inner).context("failed while reading /tmp/x");
        assert_eq!(exit_code(&err), exit::EX_DATAERR);
    }
}
