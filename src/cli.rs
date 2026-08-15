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

/// Command-line interface for `kf-code`.
#[derive(Parser, Debug)]
#[command(
    name = "kf-code",
    version,
    about = "Native Ollama CLI coding agent — static binary, TUI, cloud-routed models",
    after_help = "Exit codes:\n  0  success\n  1  general error\n  2  bad arguments\n  3  model unreachable\n  4  permission / sandbox denied\n  5  config parse error"
)]
pub struct Cli {
    /// Log verbosity. Overridden by RUST_LOG if set.
    #[arg(long, default_value = "warn", env = "KF_CODE_LOG_LEVEL", global = true)]
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

        /// Apply lightweight rlimit sandbox hardening to the non-Docker
        /// bash path (Unix only). Caps CPU seconds (SIGXCPU), address
        /// space (ENOMEM), and max file size (SIGXFSZ) on each child
        /// shell. Ignored when --docker is set (Docker already enforces
        /// --memory and --cpus). No-op on Windows with a warning.
        /// See ADR-054.
        #[arg(long)]
        harden: bool,

        /// Disable network access for bash commands (Linux only, requires --harden).
        /// Places each bash child in an empty network namespace so curl, wget, etc.
        /// cannot reach the network. No-op on non-Linux with a warning.
        #[arg(long, requires = "harden")]
        no_network: bool,

        /// Block file edits outright in --harden mode.
        /// edit_file and write_file will return failure instead of applying.
        #[arg(long, requires = "harden")]
        block_edits: bool,

        /// Suppress the production-mode sandbox refusal when no sandbox
        /// is configured (WO 21.7-R5), and let landlock fail-closed fall
        /// through to unconfined on kernels where `restrict_self` errors
        /// (WO 27.1). The operator explicitly accepts unsandboxed
        /// operation; a loud warning is logged either way.
        #[arg(long)]
        i_accept_unsandboxed: bool,

        /// Disable turn tracing. By default every turn is recorded to
        /// `<data-dir>/<session-id>.trace.ndjson` for later replay.
        #[arg(long)]
        no_trace: bool,
    },
    /// Print shell completion script and exit.
    /// Example: kf-code completions bash >> ~/.bashrc
    Completions { shell: Shell },
    /// Show operational metrics summary (tool calls, verifiers, turns, approvals).
    Metrics,
    /// Show recent verifier verdicts from the metrics log (WO 11.7).
    /// Renders a table of the last 20 `MetricEvent::Verifier` entries
    /// with the verifier name, source (`built-in` vs `plugin:<name>`),
    /// and verdict.
    Verify,
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
    /// Replay a past session turn-by-turn, showing what the model saw,
    /// what tools it called, and the outcome of each turn.
    Replay {
        /// Session id or id prefix to replay.
        id: String,

        /// Data directory containing trace files.
        #[arg(long)]
        data_dir: Option<PathBuf>,

        /// Show only this specific turn number.
        #[arg(long)]
        turn: Option<u32>,

        /// Show turns from this number (inclusive).
        #[arg(long)]
        from: Option<u32>,

        /// Show turns up to this number (inclusive).
        #[arg(long)]
        to: Option<u32>,

        /// Launch an interactive TUI stepper instead of printing all turns
        /// at once. j/k (or arrows) step forward/back, g jumps, Enter
        /// expands/collapses tool-call detail, q quits. Full-fidelity
        /// render — no 200/300-char truncation.
        #[arg(long)]
        interactive: bool,
    },
    /// Run benchmark tasks and collect metrics.
    Bench {
        #[command(subcommand)]
        command: BenchCommand,
    },
    /// Manage plugins from the CLI (headless equivalent of `/plugins`).
    ///
    /// Mutations persist to the config file; the next `kf-code run`
    /// (or a TUI `/plugins reload`) picks them up. There is no live
    /// registry to reload from the CLI path.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Test doctor: profile, classify, partition, suggest, and diagnose
    /// test coverage gaps (WO 12.4, ADR-0029).
    Doctor {
        #[command(subcommand)]
        command: DoctorCommand,
    },
    /// Self-update: download the latest GitHub release, verify SHA256, and
    /// replace this binary in place. `--check` only prints current vs
    /// latest without installing.
    Update {
        /// Print current vs latest version without installing.
        #[arg(long)]
        check: bool,
    },
}

/// Subcommands for the `doctor` command (WO 12.4, ADR-0029).
#[derive(Subcommand, Debug)]
pub enum DoctorCommand {
    /// Run `cargo test --workspace --no-fail-fast` and capture per-binary timings.
    Profile,
    /// Capture per-test timings (nightly JSON if available, per-binary
    /// fallback otherwise). WO 12.5.
    ProfilePerTest,
    /// Read the profile and classify tests as fast/medium/slow/ignored.
    Classify,
    /// Generate fast-suite.json, full-suite.json, coverage-suite.json.
    Partition,
    /// Print fix suggestions for slow tests.
    Suggest,
    /// Print smart (source-aware) fix suggestions for slow tests, using
    /// per-test timings + source-file pattern analysis. WO 12.6.
    SuggestDetailed {
        /// Optional substring filter on the test name.
        #[arg(long)]
        filter: Option<String>,
    },
    /// Apply a suggestion to a test file (text-based rewrite). WO 12.6.
    /// Always prints the diff first; requires `--yes` to write.
    Apply {
        /// Suggestion id (as printed by `suggest-detailed`).
        suggestion: String,
        /// Path to the test source file to rewrite.
        test: String,
        /// Confirm the write. Without this flag, only the diff is printed.
        #[arg(long)]
        yes: bool,
    },
    /// Analyze coverage gaps from a Cobertura XML file (tarpaulin output).
    Gaps {
        /// Path to the tarpaulin Cobertura XML.
        #[arg(long)]
        xml: PathBuf,
    },
    /// Self-diagnose: scan source files for untested public functions.
    Diagnose {
        /// Project root to scan (default: current directory).
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Detect a flaky test by running it N times (WO 12.5). Slow —
    /// default 10 runs × the test's duration. Developer tool, NOT run
    /// in CI.
    Flaky {
        /// Test filter (passed to `cargo test -- <filter> --exact`).
        filter: String,
        /// Number of runs. Default 10.
        #[arg(long, default_value_t = 10)]
        runs: u32,
    },
}

/// Subcommands for the `plugin` command (WO 11.0, ADR-056).
#[derive(Subcommand, Debug)]
pub enum PluginCommand {
    /// List active, blocked, and available plugins.
    List,
    /// Enable a plugin by name (persists to config).
    Enable { name: String },
    /// Disable a plugin by name (persists to config).
    Disable { name: String },
    /// Toggle a workspace plugin source on/off (persists to config).
    Toggle { name: String },
    /// Validate a plugin manifest at <path> (dir or `kf-code.toml`).
    Validate { path: PathBuf },
    /// Reload the plugin registry from disk and report the result.
    Reload,
    /// List configured workspace plugin sources.
    Sources,
    /// Register a workspace plugin source pointing at <path>.
    Add { name: String, path: PathBuf },
    /// Remove a workspace plugin source by name.
    Remove { name: String },
    /// Run the plugin health check (probe each enabled plugin's commands).
    Doctor,
    /// Scaffold a new plugin directory with a valid `kf-code.toml`
    /// (WO 11.8, ADR-063). Default path: `plugins/<name>/`. Override
    /// with `--path <dir>`. The scaffolded manifest uses
    /// `trust = "read-only"` (safest default) and a placeholder skill.
    Init {
        /// Plugin name (kebab-case).
        name: String,
        /// Parent directory to scaffold into. Defaults to `plugins/`
        /// in the current working directory.
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

/// Subcommands for the `bench` command.
#[derive(Subcommand, Debug)]
pub enum BenchCommand {
    /// Run all benchmark tasks.
    Run {
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
    /// Compare two benchmark reports.
    Compare {
        /// Baseline JSON report path.
        #[arg(long)]
        baseline: PathBuf,

        /// Current JSON report path.
        #[arg(long)]
        current: PathBuf,

        /// Write markdown delta summary to this file.
        #[arg(long)]
        summary: Option<PathBuf>,

        /// Fail with a non-zero exit code if the success rate dropped by
        /// more than this many percentage points (WO 10.9). The value is
        /// a percentage (e.g. `10` = 10 percentage points). When omitted,
        /// the command always exits 0 (the historical behavior).
        #[arg(long)]
        fail_on_regression: Option<f64>,
    },
    /// List all benchmark tasks.
    List {
        /// Directory containing TOML task definitions.
        #[arg(long, default_value = "benches/tasks")]
        tasks: PathBuf,
    },
    /// Verify task definitions without running LLM.
    VerifyOnly {
        /// Directory containing TOML task definitions.
        #[arg(long, default_value = "benches/tasks")]
        tasks: PathBuf,

        /// Verify only this task (by name).
        #[arg(long)]
        task: Option<String>,
    },
    /// Run all bench tasks across multiple models and produce a comparison table.
    RunModels {
        /// Directory containing TOML task definitions.
        #[arg(long, default_value = "benches/tasks")]
        tasks: PathBuf,

        /// Comma-separated list of model names to benchmark.
        #[arg(long, value_delimiter = ',')]
        models: Vec<String>,

        /// Directory to write per-model JSON reports to (one file per model).
        #[arg(long)]
        output: Option<PathBuf>,

        /// Write the markdown comparison table to this file.
        #[arg(long)]
        summary: Option<PathBuf>,

        /// Timeout per task in seconds.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_minimal_parses() {
        let cli = Cli::try_parse_from(["kf-code", "run"]).expect("parse");
        assert!(matches!(cli.command, Command::Run { .. }));
    }

    #[test]
    fn run_defaults_are_correct() {
        let cli = Cli::try_parse_from(["kf-code", "run"]).expect("parse");
        match cli.command {
            Command::Run {
                output,
                max_turns,
                seed,
                auto_resume,
                ..
            } => {
                assert_eq!(output, OutputFormat::Text);
                assert_eq!(max_turns, 0);
                assert!(seed.is_none());
                assert!(!auto_resume);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn log_level_defaults_to_warn() {
        let cli = Cli::try_parse_from(["kf-code", "run"]).expect("parse");
        assert_eq!(cli.log_level, "warn");
    }

    #[test]
    fn auto_resume_conflicts_with_continue() {
        let err = Cli::try_parse_from([
            "kf-code",
            "run",
            "--auto-resume",
            "--continue-session",
            "abc",
        ])
        .expect_err("should conflict");
        assert!(err.kind() == clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn auto_resume_conflicts_with_resume() {
        let err = Cli::try_parse_from(["kf-code", "run", "--auto-resume", "--resume", "abc"])
            .expect_err("should conflict");
        assert!(err.kind() == clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn attach_conflicts_with_auto_resume() {
        let err = Cli::try_parse_from(["kf-code", "run", "--auto-resume", "--attach", "abc"])
            .expect_err("should conflict");
        assert!(err.kind() == clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn attach_conflicts_with_continue() {
        let err = Cli::try_parse_from([
            "kf-code",
            "run",
            "--continue-session",
            "abc",
            "--attach",
            "def",
        ])
        .expect_err("should conflict");
        assert!(err.kind() == clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn daemon_stop_conflicts_with_foreground() {
        let err = Cli::try_parse_from(["kf-code", "daemon", "--stop", "--foreground"])
            .expect_err("should conflict");
        assert!(err.kind() == clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn jobd_stop_conflicts_with_foreground() {
        let err = Cli::try_parse_from(["kf-code", "jobd", "--stop", "--foreground"])
            .expect_err("should conflict");
        assert!(err.kind() == clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn output_format_text_parses() {
        let cli = Cli::try_parse_from(["kf-code", "run", "--output", "text"]).expect("parse");
        match cli.command {
            Command::Run { output, .. } => assert_eq!(output, OutputFormat::Text),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn output_format_json_parses() {
        let cli = Cli::try_parse_from(["kf-code", "run", "--output", "json"]).expect("parse");
        match cli.command {
            Command::Run { output, .. } => assert_eq!(output, OutputFormat::Json),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn output_format_stream_json_parses() {
        let cli =
            Cli::try_parse_from(["kf-code", "run", "--output", "stream-json"]).expect("parse");
        match cli.command {
            Command::Run { output, .. } => assert_eq!(output, OutputFormat::StreamJson),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn plugin_list_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "list"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::List
            }
        ));
    }

    #[test]
    fn plugin_enable_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "enable", "my-plugin"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Enable { name }
            } if name == "my-plugin"
        ));
    }

    #[test]
    fn plugin_disable_subcommand_parses() {
        let cli =
            Cli::try_parse_from(["kf-code", "plugin", "disable", "my-plugin"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Disable { name }
            } if name == "my-plugin"
        ));
    }

    #[test]
    fn plugin_init_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "init", "my-plugin"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Init { name, path: None }
            } if name == "my-plugin"
        ));
    }

    #[test]
    fn plugin_init_with_path_parses() {
        let cli = Cli::try_parse_from([
            "kf-code",
            "plugin",
            "init",
            "my-plugin",
            "--path",
            "/tmp/plugins",
        ])
        .expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Init { name, path }
            } if name == "my-plugin" && path == Some(PathBuf::from("/tmp/plugins"))
        ));
    }

    #[test]
    fn bench_run_models_comma_split() {
        let cli = Cli::try_parse_from(["kf-code", "bench", "run-models", "--models", "a,b,c"])
            .expect("parse");
        match cli.command {
            Command::Bench {
                command: BenchCommand::RunModels { models, .. },
            } => {
                assert_eq!(models, vec!["a", "b", "c"]);
            }
            _ => panic!("expected RunModels"),
        }
    }

    #[test]
    fn bench_run_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "bench", "run"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Bench {
                command: BenchCommand::Run { .. }
            }
        ));
    }

    #[test]
    fn bench_compare_subcommand_parses() {
        let cli = Cli::try_parse_from([
            "kf-code",
            "bench",
            "compare",
            "--baseline",
            "a.json",
            "--current",
            "b.json",
        ])
        .expect("parse");
        assert!(matches!(
            cli.command,
            Command::Bench {
                command: BenchCommand::Compare { .. }
            }
        ));
    }

    #[test]
    fn bench_list_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "bench", "list"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Bench {
                command: BenchCommand::List { .. }
            }
        ));
    }

    #[test]
    fn bench_verify_only_parses() {
        let cli = Cli::try_parse_from(["kf-code", "bench", "verify-only"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Bench {
                command: BenchCommand::VerifyOnly { .. }
            }
        ));
    }

    #[test]
    fn sessions_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "sessions"]).expect("parse");
        assert!(matches!(cli.command, Command::Sessions { .. }));
    }

    #[test]
    fn replay_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "replay", "abc123"]).expect("parse");
        match cli.command {
            Command::Replay { id, .. } => assert_eq!(id, "abc123"),
            _ => panic!("expected Replay"),
        }
    }

    #[test]
    fn metrics_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "metrics"]).expect("parse");
        assert!(matches!(cli.command, Command::Metrics));
    }

    #[test]
    fn verify_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "verify"]).expect("parse");
        assert!(matches!(cli.command, Command::Verify));
    }

    #[test]
    fn completions_subcommand_parses() {
        let cli = Cli::try_parse_from(["kf-code", "completions", "bash"]).expect("parse");
        assert!(matches!(cli.command, Command::Completions { .. }));
    }

    #[test]
    fn run_with_seed_parses() {
        let cli = Cli::try_parse_from(["kf-code", "run", "--seed", "42"]).expect("parse");
        match cli.command {
            Command::Run { seed, .. } => assert_eq!(seed, Some(42)),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_with_max_turns_parses() {
        let cli = Cli::try_parse_from(["kf-code", "run", "--max-turns", "5"]).expect("parse");
        match cli.command {
            Command::Run { max_turns, .. } => assert_eq!(max_turns, 5),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_with_model_parses() {
        let cli = Cli::try_parse_from(["kf-code", "run", "-m", "qwen2.5:0.5b"]).expect("parse");
        match cli.command {
            Command::Run { model, .. } => assert_eq!(model.as_deref(), Some("qwen2.5:0.5b")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_flags_parse() {
        let cli = Cli::try_parse_from([
            "kf-code",
            "run",
            "--dry-run",
            "--no-tui",
            "--worktree",
            "--docker",
            "--harden",
            "--no-trace",
            "--non-interactive",
            "--auto-approve",
        ])
        .expect("parse");
        match cli.command {
            Command::Run {
                dry_run,
                no_tui,
                worktree,
                docker,
                harden,
                no_trace,
                non_interactive,
                auto_approve,
                ..
            } => {
                assert!(dry_run);
                assert!(no_tui);
                assert!(worktree);
                assert!(docker);
                assert!(harden);
                assert!(no_trace);
                assert!(non_interactive);
                assert!(auto_approve);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn plugin_doctor_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "doctor"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Doctor
            }
        ));
    }

    #[test]
    fn plugin_reload_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "reload"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Reload
            }
        ));
    }

    #[test]
    fn plugin_sources_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "sources"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Sources
            }
        ));
    }

    #[test]
    fn plugin_validate_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "validate", "/tmp/my-plugin"])
            .expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Validate { .. }
            }
        ));
    }

    #[test]
    fn plugin_add_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "add", "my-plugin", "/tmp/my-plugin"])
            .expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Add { .. }
            }
        ));
    }

    #[test]
    fn plugin_remove_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "remove", "my-plugin"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Remove { name }
            } if name == "my-plugin"
        ));
    }

    #[test]
    fn plugin_toggle_parses() {
        let cli = Cli::try_parse_from(["kf-code", "plugin", "toggle", "my-plugin"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Plugin {
                command: PluginCommand::Toggle { name }
            } if name == "my-plugin"
        ));
    }

    #[test]
    fn sessions_with_export_conflicts_with_search() {
        let err =
            Cli::try_parse_from(["kf-code", "sessions", "--export", "json", "--search", "foo"])
                .expect_err("should conflict");
        assert!(err.kind() == clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn doctor_profile_per_test_parses() {
        let cli = Cli::try_parse_from(["kf-code", "doctor", "profile-per-test"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Doctor {
                command: DoctorCommand::ProfilePerTest
            }
        ));
    }

    #[test]
    fn doctor_flaky_parses_with_default_runs() {
        let cli = Cli::try_parse_from(["kf-code", "doctor", "flaky", "foo::bar"]).expect("parse");
        match cli.command {
            Command::Doctor {
                command: DoctorCommand::Flaky { filter, runs },
            } => {
                assert_eq!(filter, "foo::bar");
                assert_eq!(runs, 10);
            }
            _ => panic!("expected Flaky"),
        }
    }

    #[test]
    fn doctor_flaky_parses_with_custom_runs() {
        let cli = Cli::try_parse_from(["kf-code", "doctor", "flaky", "foo::bar", "--runs", "3"])
            .expect("parse");
        match cli.command {
            Command::Doctor {
                command: DoctorCommand::Flaky { filter, runs },
            } => {
                assert_eq!(filter, "foo::bar");
                assert_eq!(runs, 3);
            }
            _ => panic!("expected Flaky"),
        }
    }

    #[test]
    fn doctor_suggest_detailed_parses() {
        let cli = Cli::try_parse_from(["kf-code", "doctor", "suggest-detailed"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Doctor {
                command: DoctorCommand::SuggestDetailed { filter: None }
            }
        ));
    }

    #[test]
    fn doctor_suggest_detailed_with_filter_parses() {
        let cli =
            Cli::try_parse_from(["kf-code", "doctor", "suggest-detailed", "--filter", "sleep"])
                .expect("parse");
        match cli.command {
            Command::Doctor {
                command: DoctorCommand::SuggestDetailed { filter },
            } => {
                assert_eq!(filter.as_deref(), Some("sleep"));
            }
            _ => panic!("expected SuggestDetailed"),
        }
    }

    #[test]
    fn doctor_apply_parses_dry_run() {
        let cli = Cli::try_parse_from([
            "kf-code",
            "doctor",
            "apply",
            "test_foo::env_guard",
            "tests/foo.rs",
        ])
        .expect("parse");
        match cli.command {
            Command::Doctor {
                command:
                    DoctorCommand::Apply {
                        suggestion,
                        test,
                        yes,
                    },
            } => {
                assert_eq!(suggestion, "test_foo::env_guard");
                assert_eq!(test, "tests/foo.rs");
                assert!(!yes);
            }
            _ => panic!("expected Apply"),
        }
    }

    #[test]
    fn doctor_apply_parses_with_yes() {
        let cli = Cli::try_parse_from([
            "kf-code",
            "doctor",
            "apply",
            "test_foo::env_guard",
            "tests/foo.rs",
            "--yes",
        ])
        .expect("parse");
        match cli.command {
            Command::Doctor {
                command: DoctorCommand::Apply { yes, .. },
            } => {
                assert!(yes);
            }
            _ => panic!("expected Apply"),
        }
    }

    #[test]
    fn update_subcommand_parses_default() {
        let cli = Cli::try_parse_from(["kf-code", "update"]).expect("parse");
        match cli.command {
            Command::Update { check } => assert!(!check),
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn update_check_flag_parses() {
        let cli = Cli::try_parse_from(["kf-code", "update", "--check"]).expect("parse");
        match cli.command {
            Command::Update { check } => assert!(check),
            _ => panic!("expected Update"),
        }
    }
}
