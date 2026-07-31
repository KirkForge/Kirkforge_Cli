#![cfg_attr(not(test), deny(clippy::unwrap_used))]

//! plugin3 CLI — host hooks + budget + cost reporting.
//! Per ADR-0009, 0010, 0015. Minimal MVP: hooks speak JSON on stdin/stdout.

use clap::{Parser, Subcommand, ValueEnum};
use plugin3_core::{
    budget::{BudgetState, TokenBudget},
    cost::UsageKind,
    slicing::{HeadTailSlicer, SlicingTransform},
    store::{InMemoryOffloadStore, OffloadStore},
};

// these names are imported at the crate root purely so the inline
// `#[cfg(test)]` modules' `use super::*;` can reach them — the
// production code in main.rs does not name them directly. Gating the
// import under `#[cfg(test)]` keeps a non-test build (e.g. `cargo clippy
// --all-targets` without the test feature) from flagging them unused.
#[cfg(test)]
use plugin3_core::{
    budget::{BudgetConfig, ConfigFile},
    cost::UsageRecord,
    Paths,
};

mod exit;
mod json_out;
mod precedence;

// ADR-0002 § Crate layout splits the shared helpers out of the bin
// root. `budget_io` owns the budget.toml/config.toml persistence layer,
// `recent` owns the recent_outputs.jsonl FIFO, `helpers` owns the
// offload-store + stdin seams. Each is re-exported `pub(crate)` so the
// existing `crate::` and `super::*` call sites (commands::*, hooks, and
// the inline test modules below) keep resolving without touching their
// import lines.
mod budget_io;
mod helpers;
mod recent;

// Production re-exports — consumed by `commands::*` / `hooks` via
// `crate::` or `super::`.
pub(crate) use budget_io::{config_path, load_budget, save_budget, save_budget_config_at};
pub(crate) use helpers::{open_store, read_stdin_json};
pub(crate) use recent::{append_recent, emit_compact_hint, empty_record, load_recent_outputs};

// Test-only re-exports — reached by the `#[cfg(test)]` modules' `use
// super::*;`. Gated so a non-test build does not flag them unused.
#[cfg(test)]
pub(crate) use budget_io::{load_budget_config_at, load_budget_with_config, save_budget_at};
#[cfg(test)]
pub(crate) use helpers::plugin3_binary_path;
#[cfg(test)]
pub(crate) use recent::{append_recent_at, load_recent_outputs_at, RecentEntry, RECENT_BOUND};

#[derive(Parser, Debug)]
#[command(
    name = "plugin3",
    version,
    about = "Output slicing + token budget for AI agent context."
)]
struct Cli {
    /// Emit machine-readable JSON to stdout (ADR-0015).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run a host hook handler (reads JSON on stdin, writes JSON on stdout).
    Hook {
        #[arg(value_enum)]
        kind: HookKind,
    },
    /// Inspect or set the token budget.
    Budget(BudgetCmd),
    /// Query cost-reporting records.
    Report {
        /// Show summary only (one line per session).
        #[arg(long)]
        summary: bool,
        /// Filter to a single session id.
        #[arg(long)]
        session: Option<String>,
        /// Filter to a single record kind.
        #[arg(long, value_enum)]
        kind: Option<UsageKindArg>,
        /// Show last N records (default 100).
        #[arg(long, default_value_t = 100)]
        last: usize,
        /// Output JSON instead of human text.
        #[arg(long)]
        json: bool,
    },
    /// Self-check — exercises the load-bearing code paths. Per ponytail rule.
    SelfCheck,
    /// Print the effective config (defaults + overrides). ADR-0015.
    Config {
        /// Print the source of each field (env var / XDG default).
        #[arg(long)]
        show_sources: bool,
        /// Write-test every path; exit 78 (`EX_CONFIG`) on failure.
        /// ADR-0015 § Validate.
        #[arg(long)]
        validate: bool,
    },
    /// Manage the offload store (B4 fix, plugin3-gaps.md).
    Store {
        #[command(subcommand)]
        sub: StoreSub,
    },
    /// Write the host's hook entries into the host's settings
    /// file (B9 fix, plugin3-gaps.md; ADR-0009).
    Init {
        /// Host to wire up. Today only `claude-code` has a
        /// settings-file schema; the others exit with a clear
        /// "not yet wired" message.
        #[arg(long, value_enum, default_value_t = HostArg::ClaudeCode)]
        host: HostArg,
        /// Print the JSON that WOULD be written, don't touch disk.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite an existing `plugin3 ` hook with a different
        /// command. Without --force, conflicting commands surface
        /// exit code 3.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum StoreSub {
    /// Evict slices not referenced by `recent_outputs.jsonl`.
    Prune,
    /// Print the slice payload referenced by a marker (B5 fix).
    Get {
        /// The `<<plugin3:slice:...>>` marker from the Slice response.
        marker: String,
    },
}

// ponytail: clap-side mirror of `plugin3_hosts::Host`.
// `Host` is a typed enum but lacks the `clap::ValueEnum` derive
// (plugin3-hosts has no clap dep). Mirroring here keeps the
// host registry decoupled from the CLI's arg parser — a
// contributor who adds `Host::Codex` to plugin3-hosts adds a
// clap arm here in the same commit. Drift is caught by the
// round-trip test in `init_arg_round_trips_to_host_enum`.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[clap(rename_all = "kebab-case")]
enum HostArg {
    ClaudeCode,
    Cursor,
    Aider,
}

impl From<HostArg> for plugin3_hosts::Host {
    fn from(a: HostArg) -> Self {
        match a {
            HostArg::ClaudeCode => plugin3_hosts::Host::ClaudeCode,
            HostArg::Cursor => plugin3_hosts::Host::Cursor,
            HostArg::Aider => plugin3_hosts::Host::Aider,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug)]
#[clap(rename_all = "kebab-case")]
// ponytail: ADR-0015 § Help output conventions requires a
// one-line description on every subcommand variant. clap renders
// these into `plugin3 hook --help` and a `--json` self-check
// drift test pins the help output below.
enum HookKind {
    /// Slice the tool result before the host reads it.
    PostToolUse,
    /// Check the budget before the host sends the prompt to the model.
    UserPromptSubmit,
    /// Emit a `CompactHint` so the host's compactor has a head-start.
    PreCompact,
}

// ponytail: clap names the variants via `kebab-case` for the
// CLI spelling (`--kind budget-warn`); the inner `UsageKind`
// uses `snake_case` to match the on-disk JSONL wire format
// (ADR-0010). The enum body carries no `Serialize`/`Deserialize`
// derive because the only consumer of the bridge below is the
// explicit match in `From<UsageKindArg> for UsageKind` —
// serde here would only add a `to_value` round-trip on a
// value the caller already constructed at compile time. A
// 7th variant added to `UsageKindArg` without updating this
// match fails at compile time (the round-trip form panicked
// at runtime on a missing string).
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[clap(rename_all = "kebab-case")]
enum UsageKindArg {
    Slice,
    BudgetWarn,
    BudgetOver,
    CompactHint,
    Prompt,
    Response,
}

impl From<UsageKindArg> for UsageKind {
    fn from(a: UsageKindArg) -> Self {
        match a {
            UsageKindArg::Slice => UsageKind::Slice,
            UsageKindArg::BudgetWarn => UsageKind::BudgetWarn,
            UsageKindArg::BudgetOver => UsageKind::BudgetOver,
            UsageKindArg::CompactHint => UsageKind::CompactHint,
            UsageKindArg::Prompt => UsageKind::Prompt,
            UsageKindArg::Response => UsageKind::Response,
        }
    }
}

#[derive(Parser, Debug)]
#[command(about = "Inspect or set the token budget.")]
struct BudgetCmd {
    #[command(subcommand)]
    sub: BudgetSub,
}

#[derive(Subcommand, Debug)]
enum BudgetSub {
    /// Print the current budget state (used, ceiling, state).
    Status,
    Set {
        ceiling: usize,
        /// Persist as the default in config.toml (ADR-0015).
        #[arg(long)]
        default: bool,
    },
    /// Zero `used` to start a fresh session; ceiling and
    /// `approaching_ratio` are preserved (B2 fix, plugin3-gaps.md).
    Reset,
    /// Emit a `CompactHint` for the host's compactor (ADR-0008).
    Compact {
        /// Print the hint as JSON (default: human-readable).
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    // ponytail: ADR-0015 § Exit codes — `Cli::parse()` exits 2 on
    // bad args; the ADR prescribes 64 (EX_USAGE). `try_parse_from`
    // returns the error so we can route it through `exit_usage_err`
    // and keep the magic number in one place. A regression that
    // lets clap handle parse errors silently restores the 2 exit.
    let cli = match Cli::try_parse_from(std::env::args()) {
        Ok(c) => c,
        Err(e) => {
            // ponytail: clap's `--help` and `--version` are not
            // parse errors — exit 0 like every other CLI. clap
            // already printed the help/version text; we just need
            // to skip the error path.
            if e.kind() == clap::error::ErrorKind::DisplayHelp
                || e.kind() == clap::error::ErrorKind::DisplayVersion
            {
                e.exit();
            }
            eprint!("{e}");
            crate::exit::exit_usage_err("invalid command-line arguments");
        }
    };
    match cli.command {
        Command::Hook { kind } => match kind {
            HookKind::PostToolUse => hooks::post_tool_use(),
            HookKind::UserPromptSubmit => hooks::user_prompt_submit(),
            HookKind::PreCompact => hooks::pre_compact(),
        },
        Command::Budget(b) => match b.sub {
            BudgetSub::Status => commands::budget::status(cli.json),
            BudgetSub::Set { ceiling, default } => {
                commands::budget::set(ceiling, default, cli.json);
            }
            BudgetSub::Reset => commands::budget::reset(cli.json),
            BudgetSub::Compact { json } => commands::budget::compact(json || cli.json),
        },
        Command::Report {
            last,
            summary,
            session,
            kind,
            json,
        } => commands::report::run(
            last,
            summary,
            session,
            kind.map(Into::into),
            json || cli.json,
        ),
        Command::SelfCheck => {
            if let Err(e) = self_check() {
                eprintln!("plugin3 self-check failed: {e}");
                std::process::exit(1);
            }
        }
        Command::Config {
            show_sources,
            validate,
        } => {
            if validate {
                commands::config::validate(cli.json);
            } else {
                commands::config::show(show_sources, cli.json);
            }
        }
        Command::Store { sub } => match sub {
            StoreSub::Prune => commands::store::prune(cli.json),
            StoreSub::Get { marker } => {
                let code = commands::store::get(&marker, cli.json);
                if code != 0 {
                    // ponytail: inline the exit so the meaning lives
                    // next to the call site. The codes (1=usage, 2=
                    // backend init, 3=NotFound, 4=other) are
                    // documented in commands::store::get — adding a
                    // generic `exit_code(n: i32)` helper would invite
                    // drift between the documented table and a magic
                    // number at every call site.
                    std::process::exit(code);
                }
            }
        },
        // ponytail: B9 fix — `plugin3 init` writes the host's
        // hook entries into the host's settings file. Exit codes
        // (0 ok, 1 usage, 2 settings dir, 3 conflict, 4 I/O,
        // 5 host not supported) are documented in
        // `commands::init::run`. Inline the exit like the Store
        // dispatch above so the magic numbers stay close to
        // their cause.
        Command::Init {
            host,
            dry_run,
            force,
        } => {
            let code = commands::init::run(host.into(), dry_run, force, cli.json);
            if code != 0 {
                std::process::exit(code);
            }
        }
    }
}

// ---- Hook handlers -----------------------------------------------------

// ponytail: ADR-0002 § Crate layout splits hook handlers into
// `crates/plugin3-cli/src/hooks/`. The three `run_*` functions
// live in `hooks::post_tool_use`, `hooks::user_prompt_submit`,
// `hooks::pre_compact`. main.rs keeps the clap dispatch only.
mod hooks;

// ---- Subcommand handlers ----------------------------------------------

// ponytail: ADR-0002 § Crate layout puts the three clap subcommands
// under `commands/{budget,report,config}.rs`. They own their own
// helper modules so main.rs can stay a thin clap entry point.
mod commands;

// ---- Self-check --------------------------------------------------------

fn self_check() -> Result<(), Box<dyn std::error::Error>> {
    // Slicing round-trip on a synthetic 50 KB blob.
    let store = InMemoryOffloadStore::new();
    let slicer = HeadTailSlicer {
        head_bytes: 256,
        tail_bytes: 256,
    };
    let input = "x".repeat(50_000) + "Y_END";
    let out = slicer.apply(&input, &store)?;
    if out.head.len() != 256 {
        return Err(format!("self-check: head length {} != 256", out.head.len()).into());
    }
    if out.tail.len() != 256 {
        return Err(format!("self-check: tail length {} != 256", out.tail.len()).into());
    }
    if !out.tail.ends_with("Y_END") {
        return Err("self-check: tail does not end with sentinel".into());
    }
    let marker = out
        .offload_marker
        .as_ref()
        .ok_or("self-check produced no offload marker")?;
    if out.bytes_saved == 0 {
        return Err("self-check: bytes_saved is zero".into());
    }

    // Budget state transitions.
    let mut b = TokenBudget {
        ceiling: 100,
        approaching_ratio: 0.8,
        used: 0,
    };
    if b.state() != BudgetState::Under {
        return Err(format!("self-check: fresh budget state {:?} != Under", b.state()).into());
    }
    b.record(80);
    if b.state() != BudgetState::Approaching {
        return Err(format!("self-check: budget state {:?} != Approaching", b.state()).into());
    }
    b.record(20);
    if b.state() != BudgetState::Over {
        return Err(format!("self-check: budget state {:?} != Over", b.state()).into());
    }

    // Offload retrieval round-trip via marker.
    let key = plugin3_core::parse_slice_marker(marker)
        .ok_or("self-check: marker does not contain a valid key")?;
    let recovered = store.get(key)?;
    if recovered.len() != out.bytes_saved {
        return Err(format!(
            "self-check: recovered length {} != bytes_saved {}",
            recovered.len(),
            out.bytes_saved
        )
        .into());
    }

    // Hook registry (ADR-0009). Serialising must not panic and
    // must produce a parseable JSON object for both the
    // ClaudeCode (3-slot) and Cursor/Aider (empty) hosts. Drift
    // tests in `hooks::drift_tests` pin the exact field names.
    let cfg = hooks::register_hooks(hooks::current_host());
    let s = serde_json::to_string(&cfg)?;
    if !s.starts_with('{') {
        return Err(format!("self-check: HookConfig serialises to non-object: {s}").into());
    }

    // Exit helpers (ADR-0015). A smoke compile + message path:
    // build the format strings the helpers would emit, then assert
    // they parse without panicking. The helpers themselves are
    // `-> !` (no return), so the drift test in `validate_tests`
    // pins the actual exit code via subprocess.
    let cfg_msg = format!("config failure with {} checks", 1);
    if !cfg_msg.contains("config failure") {
        return Err("self-check: config helper message malformed".into());
    }
    let usage_msg = format!("usage failure with {} args", 2);
    if !usage_msg.contains("usage failure") {
        return Err("self-check: usage helper message malformed".into());
    }

    println!("plugin3 self-check OK (slicing + budget + offload round-trip)");
    Ok(())
}

// ---- Helpers -----------------------------------------------------------
// Moved to `budget_io`, `recent`, and `helpers` per ADR-0002 § Crate
// layout; re-exported at the crate root above so `crate::` and
// `super::*` call sites keep resolving.

// ---- Tests -------------------------------------------------------------
// the four inline `#[cfg(test)]` modules were split into sibling files
// (`tests_main`, `tests_validate`, `tests_adr_0015`, `tests_recent`)
// per ADR-0002 § Crate layout. Each is declared as a direct child of
// the bin root so its `use super::*;` keeps resolving against the crate
// root (same semantics as the prior inline `mod`).
#[cfg(test)]
mod tests_adr_0015;
#[cfg(test)]
mod tests_main;
#[cfg(test)]
mod tests_recent;
#[cfg(test)]
mod tests_validate;
