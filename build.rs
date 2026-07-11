// Generate kirkforge.1 man page at build time.
// The full CLI definition lives in src/main.rs (binary); we mirror
// the top-level structure here so clap_mangen can produce a man page
// without making Cli pub from main.rs or adding a lib target.

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kirkforge",
    version,
    about = "Native Ollama CLI coding agent — static binary, TUI, potato hardware",
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

#[derive(Subcommand)]
enum Command {
    /// Start an interactive coding session.
    Run {
        #[arg(short, long)]
        model: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        model_type: Option<String>,
        #[arg(long)]
        auto_approve: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(short, long)]
        system: Option<String>,
        #[arg(short, long)]
        resume: Option<String>,
        #[arg(long)]
        non_interactive: bool,
        #[arg(long, default_value = "text")]
        output: String,
        #[arg(long, default_value_t = 0)]
        max_turns: usize,
        #[arg(long)]
        continue_session: Option<String>,
        #[arg(long, conflicts_with = "continue_session", conflicts_with = "resume")]
        auto_resume: bool,
        #[arg(
            long,
            conflicts_with = "continue_session",
            conflicts_with = "resume",
            conflicts_with = "auto_resume"
        )]
        attach: Option<String>,
        #[arg(long)]
        no_tui: bool,
    },
    /// Print shell completion script and exit.
    Completions { shell: Shell },
    /// List and export past sessions.
    Sessions {
        id: Option<String>,
        #[arg(long, value_name = "FORMAT")]
        export: Option<String>,
        #[arg(long, short)]
        output: Option<std::path::PathBuf>,
    },
    /// Run the background session daemon.
    Daemon {
        #[arg(long)]
        foreground: bool,
        #[arg(long, conflicts_with = "foreground")]
        stop: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(std::env::var("OUT_DIR")?);
    let man = clap_mangen::Man::new(Cli::command());
    let mut buf = vec![];
    man.render(&mut buf)?;
    std::fs::write(out.join("kirkforge.1"), buf)?;
    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}
