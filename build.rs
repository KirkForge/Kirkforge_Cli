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
    #[arg(long, default_value = "warn", env = "KIRKFORGE_LOG_LEVEL", global = true)]
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
        auto_approve: bool,
        #[arg(short, long)]
        system: Option<String>,
        #[arg(long)]
        non_interactive: bool,
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

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let man = clap_mangen::Man::new(Cli::command());
    let mut buf = vec![];
    man.render(&mut buf).expect("man page render failed");
    std::fs::write(out.join("kirkforge.1"), buf).expect("write kirkforge.1");
    println!("cargo:rerun-if-changed=build.rs");
}
