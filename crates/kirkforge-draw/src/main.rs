#![cfg_attr(not(test), deny(clippy::unwrap_used))]

//! `kfd` — the standalone KirkForge-Draw terminal editor.
//!
//! Modes:
//!   * `kfd --render --load FILE --plain`  → write plain text to stdout
//!   * `kfd --render --load FILE --fenced` → write fenced markdown to stdout
//!   * `kfd --validate FILE`                → print diagnostic report, exit 0/1
//!   * `kfd --load FILE`                    → launch the interactive TUI
//!   * `kfd`                                → launch an empty TUI
//!
//! See `docs/adr/0004-tui-pane.md`.

mod app;
mod event;
mod render;
mod scene_render;
mod tui;
mod ui;

use clap::Parser;

use crate::app::App;
use crate::event::atomic_write;
use crate::render::{
    format_validate_report, format_validate_report_json, load_doc, render_ansi, render_fenced,
    run_validate,
};
use crate::tui::TerminalGuard;
use kirkforge_draw_core::render_plain;

#[derive(Debug, Parser)]
#[command(name = "kfd", about = "KirkForge-Draw: terminal diagram editor")]
struct Cli {
    /// Path to a `.td.json` document to open.
    #[arg(long)]
    load: Option<String>,

    /// Path to write the rendered art to (instead of stdout).
    #[arg(long, short = 'o')]
    output: Option<String>,

    /// Render the document non-interactively (requires --load).
    #[arg(long)]
    render: bool,

    /// Output a fenced markdown code block (with --render).
    #[arg(long)]
    fenced: bool,

    /// Output plain text (default; with --render).
    #[arg(long)]
    plain: bool,

    /// Output ANSI-colored terminal text (with --render).
    #[arg(long)]
    ansi: bool,

    /// Validate a `.td.json` file (requires --load). Prints a report
    /// and exits 0 on clean, 1 on issues.
    #[arg(long)]
    validate: bool,

    /// Emit the `--validate` report as pretty-printed JSON instead of
    /// the human-readable block. Stable shape; safe for `jq` /
    /// build-pipeline consumers.
    #[arg(long)]
    json: bool,

    /// Print the version and exit.
    #[arg(long, short = 'v')]
    version: bool,
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => render_cli_error(&e),
    }
}

/// Pretty-print a CLI error to stderr. Replaces Rust's default
/// `Termination` impl for `Result<_, anyhow::Error>` (which calls
/// `Debug`, losing context). Format:
///
///   kfd: <top-level message>
///   caused by:
///     <chain>
///     <chain>
///
/// `RUST_BACKTRACE=1` appends a backtrace at the end. Exit code is
/// always `FAILURE` — cli branches that need a specific success /
/// failure split already return their own `ExitCode` via `Ok(_)`.
fn render_cli_error(err: &anyhow::Error) -> std::process::ExitCode {
    eprint!("{}", format_cli_error(err));
    std::process::ExitCode::FAILURE
}

/// Format an `anyhow::Error` chain as a single string. Pure — does
/// no I/O so the CLI's error rendering can be unit-tested without
/// capturing stderr. The chain walker caps at 8 levels; the cap is
/// chosen because `anyhow::Context` can theoretically wrap an error
/// in itself, and a tight loop should never reach 8 in practice.
fn format_cli_error(err: &anyhow::Error) -> String {
    let mut out = String::new();
    out.push_str(&format!("kfd: {err}\n"));
    let mut src = err.source();
    if src.is_some() {
        out.push_str("caused by:\n");
        let mut depth = 0;
        while let Some(c) = src {
            out.push_str(&format!("  {c}\n"));
            src = c.source();
            depth += 1;
            if depth >= 8 {
                out.push_str("  ... (chain truncated)\n");
                break;
            }
        }
    }
    out
}

/// Entry point. Lives behind `main()` so the outer `Termination`
/// impl can pretty-print errors before the process exits. The body
/// is the prior `main()` — clap parse + dispatch over `--validate`,
/// `--render`, and the interactive TUI.
fn run() -> anyhow::Result<std::process::ExitCode> {
    let cli = Cli::parse();
    if cli.version {
        println!("kfd {}", env!("CARGO_PKG_VERSION"));
        return Ok(std::process::ExitCode::SUCCESS);
    }

    if cli.validate {
        let path = cli
            .load
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--validate requires --load <path>"))?;
        let report = run_validate(path)?;
        if cli.json {
            println!("{}", format_validate_report_json(&report, path)?);
        } else {
            print!("{}", format_validate_report(&report, path));
        }
        // Exit 0 on a clean report, 1 on any flagged issue. Returning
        // an explicit `ExitCode` here replaces the prior
        // `std::process::exit(1)` short-circuit and lets every CLI
        // branch share the `Termination` for `Result<ExitCode, Error>`.
        // `?` errors above still go through the failure path
        // (ExitCode::FAILURE).
        return Ok(validate_exit_code(&report));
    }

    if cli.render {
        let path = cli
            .load
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--render requires --load <path>"))?;
        let doc = load_doc(path)?;
        let rendered = if cli.fenced {
            render_fenced(&doc)
        } else if cli.ansi {
            render_ansi(&doc)
        } else {
            render_plain(&doc)
        };
        if let Some(out) = cli.output.as_deref() {
            // Same crash-safety contract as the editor's Ctrl-S
            // path: write-tmp-then-rename + sync_all. A bare
            // `fs::write` truncates the target first; a crash in
            // the middle leaves the on-disk file shorter than the
            // rendered buffer.
            atomic_write(std::path::Path::new(out), rendered.as_bytes())?;
        } else {
            print!("{rendered}");
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Interactive TUI path.
    let state = match cli.load.as_deref() {
        Some(path) => {
            let doc = load_doc(path)?;
            kirkforge_draw_core::DrawState::with_document(doc)
        }
        None => kirkforge_draw_core::DrawState::new(),
    };
    let mut app = App::new(state);
    if let Some(path) = cli.load.clone() {
        app = app.with_source(path);
    }

    let mut guard = TerminalGuard::new()?;
    event::run(&mut app, guard.terminal())?;
    Ok(std::process::ExitCode::SUCCESS)
}

/// Translate a `ValidateReport` into the process exit code that the
/// `--validate` CLI surfaces. Clean reports exit SUCCESS; any flagged
/// issue (parse error, unknown-type warning whose flag is set,
/// duplicate id, degenerate geometry) exits FAILURE. Pure — kept
/// here so the regression test in this crate pins the policy.
fn validate_exit_code(report: &kirkforge_draw_core::ValidateReport) -> std::process::ExitCode {
    if report.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirkforge_draw_core::{
        types::{BoxObject, BoxStyle, DrawDocument, DrawObject, InkColor},
        validate_document, ValidateReport, DRAW_DOCUMENT_VERSION,
    };

    fn clean_report() -> ValidateReport {
        let json = format!(
            r#"{{"version":{DRAW_DOCUMENT_VERSION},"objects":[{{"type":"box","id":"b","z":1,"color":"white","left":0,"top":0,"right":5,"bottom":3,"style":"light"}}]}}"#
        );
        validate_document(&json)
    }

    fn dirty_report_via_doc() -> ValidateReport {
        // Degenerate box (left == right, top == bottom — zero-area)
        // feeds the degenerate_object_ids bucket. Build via the
        // public validate_document path so the report exercises the
        // same code that the CLI does. The validator's `is_degenerate`
        // is the single-cell predicate, not "left > right", so a
        // point-shaped box is what we need.
        let doc = DrawDocument {
            version: 1,
            objects: vec![DrawObject::Box(BoxObject {
                id: "d".into(),
                z: 1,
                parent_id: None,
                color: InkColor::White,
                left: 5,
                top: 3,
                right: 5,  // == left
                bottom: 3, // == top → degenerate (zero-area)
                style: BoxStyle::Light,
            })],
        };
        let json = serde_json::to_string(&doc).unwrap();
        validate_document(&json)
    }

    #[test]
    fn validate_exit_code_is_success_on_clean() {
        assert_eq!(
            validate_exit_code(&clean_report()),
            std::process::ExitCode::SUCCESS
        );
    }

    #[test]
    fn validate_exit_code_is_failure_on_degenerate_box() {
        let report = dirty_report_via_doc();
        assert!(!report.is_ok(), "fixture should be flagged");
        assert_eq!(validate_exit_code(&report), std::process::ExitCode::FAILURE);
    }

    // CLI error formatting. `format_cli_error` is a pure helper
    // that returns the same multi-line message `render_cli_error`
    // prints to stderr, so the tests below pin the rendered output
    // without touching the process's stderr stream.

    #[test]
    fn format_cli_error_renders_top_level_message() {
        let err = anyhow::anyhow!("boom");
        let out = format_cli_error(&err);
        assert!(out.starts_with("kfd: boom\n"), "got: {out:?}");
        // No `caused by:` block when there's only one error in
        // the chain — a single-cause error shouldn't grow a
        // header line.
        assert!(!out.contains("caused by:"), "got: {out:?}");
    }

    #[test]
    fn format_cli_error_walks_the_context_chain() {
        // Build an io error, wrap it in `anyhow::Context`, then
        // wrap again. The chain should print three lines under
        // `caused by:`.
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err = anyhow::Error::new(io)
            .context("read /tmp/foo")
            .context("load_doc");
        let out = format_cli_error(&err);
        assert!(out.contains("kfd: load_doc"), "got: {out:?}");
        assert!(out.contains("caused by:"), "got: {out:?}");
        assert!(out.contains("read /tmp/foo"), "got: {out:?}");
        assert!(out.contains("no such file"), "got: {out:?}");
    }

    #[test]
    fn format_cli_error_truncates_long_chains() {
        // Wrap the same root error many times. The cap should
        // kick in around depth 8 and emit the truncation marker
        // so a runaway chain doesn't produce thousands of lines.
        let mut err = anyhow::anyhow!("root");
        for i in 0..20 {
            err = err.context(format!("level {i}"));
        }
        let out = format_cli_error(&err);
        assert!(out.contains("(chain truncated)"), "got: {out:?}");
        // The most recently added level (level 19) is the
        // top of the chain and is printed first by anyhow's
        // `Display`. The chain walker walks toward the root,
        // so an early level (e.g., "level 0") should be cut
        // off by the depth-8 cap.
        assert!(out.contains("level 19"), "top of chain must print");
        assert!(
            !out.contains("level 0"),
            "deepest level should be cut: {out:?}"
        );
    }

    #[test]
    fn render_cli_error_always_returns_failure() {
        // Even when the error is informational (e.g., a user
        // "missing arg"), the CLI should exit non-zero so a
        // wrapper script can detect it.
        let err = anyhow::anyhow!("user-friendly");
        assert_eq!(render_cli_error(&err), std::process::ExitCode::FAILURE);
    }
}
