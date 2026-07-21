// Shared CLI definition used by both the binary and the build script.
//
// Keeping the clap structure in one place means build.rs can generate the
// man page from the real Cli (via include!) without drifting out of sync
// with the runtime parser.

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

/// Output format for non-interactive sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Plain text output.
    Text,
    /// Single JSON object containing the full session summary.
    Json,
    /// One JSON object per message, streamed as NDJSON.
    StreamJson,
}

/// Command-line interface for `kirkforge`.
#[derive(Parser, Debug)]
#[command(
    name = "kirkforge",
    version,
    about = "Native Ollama CLI coding agent — static binary, TUI, cloud-routed models",
    after_help = "Exit codes:\n  0  success\n  1  general error\n  2  bad arguments\n  3  model unreachable\n  4  permission / sandbox denied\n  5  config parse error"
)]
pub struct Cli {
    /// Log verbosity. Overridden by RUST_LOG if set.
    #[arg(
        long,
        default_value = "warn",
        env = "KIRKFORGE_LOG_LEVEL",
        global = true
    )]
    pub log_level: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
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
        output: OutputFormat,

        /// Cap on the number of turns in non-interactive mode. Each
        /// non-empty line on stdin is one turn. 0 = unlimited (run
        /// until EOF or a blank line). Defaults to 0.
        #[arg(long, default_value_t = 0)]
        max_turns: usize,

        /// Resume a prior session by id prefix (or full path).
        #[arg(long)]
        continue_session: Option<String>,

        /// Resume the most recent session via the session daemon.
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
        #[arg(long)]
        no_tui: bool,

        /// Deterministic mode: pin temperature=0 and set model seed for
        /// reproducible planning. Best-effort — model providers don't
        /// guarantee identical outputs even with the same seed, but the
        /// tool-call *sequence* is reproducible enough for regression
        /// testing. Also forces sequential tool dispatch (no tokio::spawn).
        #[arg(long)]
        seed: Option<u64>,

        /// Create an isolated git worktree for the session. Edits land in
        /// the worktree, not the user's working tree. The worktree is
        /// removed when the session ends.
        #[arg(long)]
        worktree: bool,

        /// Execute bash commands in a Docker container with resource limits.
        /// Requires Docker to be installed and running. When set, the bash
        /// tool spawns in a container with --memory and --cpus limits.
        #[arg(long)]
        docker: bool,
    },
    /// Print shell completion script and exit.
    /// Example: kirkforge completions bash >> ~/.bashrc
    Completions { shell: Shell },
    /// Show operational metrics summary (tool calls, verifiers, turns, approvals).
    Metrics,
    /// List, search, and export past sessions.
    /// Without arguments, lists recent sessions (newest first).
    /// With --export, writes the session to stdout or a file.
    /// With --search, filters sessions by id, date, or message count.
    Sessions {
        /// Session id or id prefix to export. Omit to list all sessions.
        id: Option<String>,

        /// Export format: markdown, json, or ndjson.
        #[arg(long, value_name = "FORMAT")]
        export: Option<String>,

        /// Write export to this file instead of stdout.
        #[arg(long, short)]
        output: Option<PathBuf>,

        /// Search sessions by id, date, message count, or message content.
        #[arg(long, value_name = "QUERY", conflicts_with = "export")]
        search: Option<String>,
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
    /// Run the background scheduled-job daemon.
    Jobd {
        /// Stay in the foreground instead of detaching.
        #[arg(long)]
        foreground: bool,

        /// Stop a running daemon.
        #[arg(long, conflicts_with = "foreground")]
        stop: bool,
    },
    /// Run benchmark tasks and collect metrics.
    Bench {
        /// Directory containing TOML task definitions.
        #[arg(long, default_value = "benches/tasks")]
        tasks: PathBuf,

        /// Model to benchmark.
        #[arg(long)]
        model: Option<String>,

        /// Write JSON report to this file.
        #[arg(long)]
        output: Option<PathBuf>,

        /// Write markdown summary to this file.
        #[arg(long)]
        summary: Option<PathBuf>,

        /// Timeout per task in seconds.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
}
