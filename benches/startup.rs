use clap::{Parser, Subcommand};
use std::time::Instant;

/// Minimal CLI clone used only for the startup benchmark. This avoids
/// needing a lib target for the binary crate.
#[derive(Parser)]
#[command(name = "kirkforge")]
struct Cli {
    #[arg(
        long,
        default_value = "warn",
        env = "KIRKFORGE_LOG_LEVEL",
        global = true
    )]
    _log_level: String,

    #[command(subcommand)]
    _command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        _non_interactive: bool,
    },
}

/// Benchmark CLI parsing, the cheapest measurable part of cold start.
fn main() {
    let iterations = 1000;
    let start = Instant::now();
    for _ in 0..iterations {
        let _cli = Cli::parse_from(["kirkforge", "run", "--non-interactive"]);
    }
    let elapsed = start.elapsed();
    println!(
        "cli_parse: {} iterations in {:?} ({:?} per iteration)",
        iterations,
        elapsed,
        elapsed / iterations
    );
}
