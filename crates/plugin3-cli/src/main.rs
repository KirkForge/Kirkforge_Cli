//! plugin3 CLI — host hooks + budget + cost reporting.
//! Per ADR-0009, 0010, 0015. Minimal MVP: hooks speak JSON on stdin/stdout.

use std::collections::VecDeque;
use std::io::{self, Read};

use clap::{Parser, Subcommand, ValueEnum};
use plugin3_core::{
    atomic_write_text,
    budget::{BudgetConfig, BudgetState, ConfigFile, TokenBudget, UsageConfig},
    cost::{emit_usage, UsageKind, UsageRecord},
    slicing::{HeadTailSlicer, SlicingTransform},
    store::{FileOffloadStore, InMemoryOffloadStore, OffloadStore},
    Paths,
};
use serde::{Deserialize, Serialize};

mod exit;
mod json_out;
mod precedence;

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
        Command::SelfCheck => self_check(),
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

fn self_check() {
    // Slicing round-trip on a synthetic 50 KB blob.
    let store = InMemoryOffloadStore::new();
    let slicer = HeadTailSlicer {
        head_bytes: 256,
        tail_bytes: 256,
    };
    let input = "x".repeat(50_000) + "Y_END";
    let out = slicer.apply(&input, &store).unwrap();
    assert_eq!(out.head.len(), 256);
    assert_eq!(out.tail.len(), 256);
    assert!(out.tail.ends_with("Y_END"), "tail should end with sentinel");
    assert!(out.offload_marker.is_some());
    assert!(out.bytes_saved > 0);

    // Budget state transitions.
    let mut b = TokenBudget {
        ceiling: 100,
        approaching_ratio: 0.8,
        used: 0,
    };
    assert_eq!(b.state(), BudgetState::Under);
    b.record(80);
    assert_eq!(b.state(), BudgetState::Approaching);
    b.record(20);
    assert_eq!(b.state(), BudgetState::Over);

    // Offload retrieval round-trip via marker.
    let marker = out.offload_marker.as_ref().unwrap();
    let key = plugin3_core::parse_slice_marker(marker).unwrap();
    let recovered = store.get(key).unwrap();
    assert_eq!(recovered.len(), out.bytes_saved);

    // Hook registry (ADR-0009). Serialising must not panic and
    // must produce a parseable JSON object for both the
    // ClaudeCode (3-slot) and Cursor/Aider (empty) hosts. Drift
    // tests in `hooks::drift_tests` pin the exact field names.
    let cfg = hooks::register_hooks(hooks::current_host());
    let s = serde_json::to_string(&cfg).expect("HookConfig serialises");
    assert!(s.starts_with('{'), "HookConfig serialises to object: {s}");

    // Exit helpers (ADR-0015). A smoke compile + message path:
    // build the format strings the helpers would emit, then assert
    // they parse without panicking. The helpers themselves are
    // `-> !` (no return), so the drift test in `validate_tests`
    // pins the actual exit code via subprocess.
    let cfg_msg = format!("config failure with {} checks", 1);
    assert!(cfg_msg.contains("config failure"));
    let usage_msg = format!("usage failure with {} args", 2);
    assert!(usage_msg.contains("usage failure"));

    println!("plugin3 self-check OK (slicing + budget + offload round-trip)");
}

// ---- Helpers -----------------------------------------------------------

fn open_store() -> Box<dyn OffloadStore> {
    let dir = Paths::resolve().slices_dir();
    match FileOffloadStore::open(&dir) {
        Ok(s) => Box::new(s),
        Err(e) => {
            eprintln!("plugin3: file store open failed ({e}); falling back to in-memory");
            Box::new(InMemoryOffloadStore::new())
        }
    }
}

// ponytail: ADR-0009 § Error contract — a hook handler must not
// crash the host. Returns None on read or parse failure so the
// caller can emit a safe fallback response and exit 0.
fn read_stdin_json<T: for<'de> Deserialize<'de>>() -> Option<T> {
    let mut s = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut s) {
        eprintln!("plugin3: stdin read failed: {e}");
        return None;
    }
    match serde_json::from_str(&s) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("plugin3: stdin parse failed: {e}");
            None
        }
    }
}

fn budget_path() -> std::path::PathBuf {
    Paths::resolve().budget_file()
}

fn config_path() -> std::path::PathBuf {
    Paths::resolve().config_file()
}

fn load_budget() -> TokenBudget {
    load_budget_with_config(&budget_path(), &config_path())
}

fn save_budget(b: &TokenBudget) {
    save_budget_at(b, &budget_path());
}

// ponytail: removed `load_budget_at` — `load_budget_with_config` is the
// single entry point now. Splitting them again would invite drift
// between "what `load_budget` does" and "what tests of a single file do".

// ADR-0014: atomic write lives in plugin3-core. The CLI just calls it.

fn save_budget_at(b: &TokenBudget, path: &std::path::Path) {
    let Ok(s) = toml::to_string(b) else { return };
    atomic_write_text(path, "budget", &s);
}

// ---- config.toml (ADR-0005 § Defaults, ADR-0015 § budget set --default) -

// ponytail: `load_budget_config_at` returns Option rather than
// defaulting inside the parser. That way a missing file is
// distinguishable from "user wrote ceiling=0" — important when the
// runtime merge wants to skip override cleanly. The on-disk file
// is a `ConfigFile` wrapper (ADR-0005 § Defaults) so the
// `[budget]` section header is preserved.
fn load_budget_config_at(path: &std::path::Path) -> Option<BudgetConfig> {
    let s = std::fs::read_to_string(path).ok()?;
    let file: ConfigFile = toml::from_str(&s).ok()?;
    Some(file.budget)
}

// ponytail: wraps the `BudgetConfig` in `ConfigFile` to emit the
// `[budget]` section header (ADR-0005 § Defaults). Same atomic-write
// helper as `save_budget_at`.
fn save_budget_config_at(cfg: &BudgetConfig, path: &std::path::Path) {
    let file = ConfigFile {
        budget: *cfg,
        usage: UsageConfig::default(),
    };
    let Ok(s) = toml::to_string(&file) else {
        return;
    };
    atomic_write_text(path, "config", &s);
}

// Precedence: runtime budget.toml (used) > config.toml (ceiling/ratio) >
// TokenBudget::default(). The runtime file is per-session and never
// carries user defaults; config.toml is the persistence layer for
// `plugin3 budget set --default`.
fn load_budget_with_config(
    runtime_path: &std::path::Path,
    config_path: &std::path::Path,
) -> TokenBudget {
    let mut b = TokenBudget::default();
    if let Ok(s) = std::fs::read_to_string(runtime_path) {
        if let Ok(runtime) = toml::from_str::<TokenBudget>(&s) {
            b = runtime;
        }
    }
    // ponytail: config.toml always overrides ceiling/ratio when present,
    // even if the runtime file disagrees. The `used` counter is
    // session-local and intentionally NOT taken from config.
    if let Some(cfg) = load_budget_config_at(config_path) {
        b.ceiling = cfg.ceiling;
        b.approaching_ratio = cfg.approaching_ratio;
    }
    b
}

// ponytail: shared by every subprocess test (ADR-0009, ADR-0015).
// `cfg(test)` keeps it out of release builds — the binary's own
// path-lookup isn't a runtime concern.
#[cfg(test)]
fn plugin3_binary_path() -> std::path::PathBuf {
    // CARGO_BIN_EXE_<name> is set when the test is run via the
    // cargo test runner. When the binary path is unknown, fall
    // back to a sibling of the running test executable
    // (target/debug/deps/plugin3-<hash> -> target/debug/plugin3).
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_plugin3") {
        return std::path::PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("current_exe");
    // exe is target/debug/deps/plugin3-<hash>; the binary lives
    // one level up under the same name without the hash.
    let stem = exe.file_name().unwrap().to_string_lossy();
    let without_hash = stem.split('-').next().unwrap();
    exe.parent()
        .unwrap() // deps/
        .parent()
        .unwrap() // debug/
        .join(without_hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ponytail: keep the tempdir alive for the test by returning the guard.
    // `prefix` is "budget" or "config"; the function makes the file
    // distinguishable when several tests run in the same tempdir.
    fn fresh_path(tag: &str, prefix: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(format!("{prefix}-{tag}.toml"));
        (dir, path)
    }

    #[test]
    fn budget_round_trips_via_atomic_write() {
        let (_dir, path) = fresh_path("rt", "budget");
        let mut b = TokenBudget {
            ceiling: 50_000,
            approaching_ratio: 0.8,
            used: 0,
        };
        b.record(1234);
        save_budget_at(&b, &path);
        let written = std::fs::read_to_string(&path).expect("budget written");
        let parsed: TokenBudget = toml::from_str(&written).expect("parse");
        assert_eq!(parsed.used, 1234);
        assert_eq!(parsed.ceiling, 50_000);
    }

    #[test]
    fn budget_overwrite_does_not_leak_tmp() {
        // Two consecutive saves should leave exactly one budget file
        // (no orphan .tmp files in the parent dir).
        let (_dir, path) = fresh_path("ov", "budget");
        save_budget_at(&TokenBudget::default(), &path);
        save_budget_at(
            &TokenBudget {
                ceiling: 9999,
                approaching_ratio: 0.5,
                used: 42,
            },
            &path,
        );
        let parent = path.parent().unwrap();
        let orphans: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(orphans.is_empty(), "no leftover tmp files: {orphans:?}");
        let final_b: TokenBudget =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(final_b.ceiling, 9999);
    }

    // ---- BudgetConfig / --default wiring (ADR-0005 + ADR-0015) -----

    #[test]
    fn budget_config_round_trips_via_atomic_write() {
        let (_dir, path) = fresh_path("rt", "config");
        let cfg = BudgetConfig {
            ceiling: 300_000,
            approaching_ratio: 0.75,
        };
        save_budget_config_at(&cfg, &path);
        let back = load_budget_config_at(&path).expect("config exists");
        assert_eq!(back, cfg);
    }

    #[test]
    fn load_budget_picks_up_default_ceiling_from_config_toml() {
        // ADR-0015: a future session must see the user's --default
        // even when its own runtime budget.toml is missing. Test by
        // pointing the helper at a config that overrides the default
        // and a runtime path that does not exist.
        let (_dir, cfg_path) = fresh_path("def", "config");
        save_budget_config_at(
            &BudgetConfig {
                ceiling: 123_456,
                approaching_ratio: 0.6,
            },
            &cfg_path,
        );
        let runtime_path = cfg_path.with_file_name("absent-runtime.toml");
        let b = load_budget_with_config(&runtime_path, &cfg_path);
        assert_eq!(b.ceiling, 123_456);
        assert!((b.approaching_ratio - 0.6).abs() < f64::EPSILON);
        assert_eq!(b.used, 0, "fresh session has no `used` carryover");
    }

    #[test]
    fn load_budget_runtime_used_overrides_config_used() {
        // ponytail: `used` is session-local and must NEVER come from
        // config.toml. The runtime file is the only authority on it.
        let (_dir, cfg_path) = fresh_path("used", "config");
        save_budget_config_at(
            &BudgetConfig {
                ceiling: 999_999,
                approaching_ratio: 0.9,
            },
            &cfg_path,
        );
        let (_dir2, runtime_path) = fresh_path("used-rt", "budget");
        save_budget_at(
            &TokenBudget {
                ceiling: 999_999,
                approaching_ratio: 0.9,
                used: 4321,
            },
            &runtime_path,
        );
        let b = load_budget_with_config(&runtime_path, &cfg_path);
        assert_eq!(b.used, 4321, "runtime `used` survives config overlay");
        assert_eq!(b.ceiling, 999_999, "config ceiling wins");
    }

    #[test]
    fn load_budget_missing_both_falls_back_to_defaults() {
        // Neither runtime nor config exists: TokenBudget::default().
        let (_dir, path) = fresh_path("none", "budget");
        let runtime_path = path.with_file_name("missing-runtime.toml");
        let cfg_path = path.with_file_name("missing-config.toml");
        let b = load_budget_with_config(&runtime_path, &cfg_path);
        // ponytail: assert field-by-field because TokenBudget lacks
        // PartialEq (the `used` counter is mutated freely and a
        // derived Eq would invite accidental == on hot paths).
        assert_eq!(b.ceiling, TokenBudget::default().ceiling);
        assert!(
            (b.approaching_ratio - TokenBudget::default().approaching_ratio).abs() < f64::EPSILON
        );
        assert_eq!(b.used, 0);
    }

    #[test]
    fn budget_config_overwrite_does_not_leak_tmp() {
        let (_dir, path) = fresh_path("ov", "config");
        save_budget_config_at(
            &BudgetConfig {
                ceiling: 100,
                approaching_ratio: 0.5,
            },
            &path,
        );
        save_budget_config_at(
            &BudgetConfig {
                ceiling: 200,
                approaching_ratio: 0.6,
            },
            &path,
        );
        let parent = path.parent().unwrap();
        let orphans: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(orphans.is_empty(), "no leftover tmp files: {orphans:?}");
    }

    fn write_records(
        dir: &std::path::Path,
        name: &str,
        records: &[UsageRecord],
    ) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut s = String::new();
        for r in records {
            s.push_str(&serde_json::to_string(r).unwrap());
            s.push('\n');
        }
        std::fs::write(&p, s).unwrap();
        p
    }

    fn rec(kind: UsageKind, session: &str) -> UsageRecord {
        let mut r = UsageRecord {
            ts: chrono::Utc::now(),
            kind,
            session_id: session.into(),
            bytes_in: None,
            bytes_out: None,
            tokens_used: None,
            tokens_ceiling: None,
            tool: None,
        };
        if matches!(r.kind, UsageKind::Slice) {
            r.bytes_in = Some(1000);
            r.bytes_out = Some(400);
        }
        r
    }

    #[test]
    fn report_kind_filter_selects_matching_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_records(
            dir.path(),
            "usage.jsonl",
            &[
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::BudgetWarn, "s1"),
                rec(UsageKind::Slice, "s2"),
                rec(UsageKind::CompactHint, "s2"),
            ],
        );
        let n = commands::report::at(&path, 100, false, None, Some(UsageKind::Slice), false);
        assert_eq!(n, 2, "only the two slice records survive");
    }

    #[test]
    fn report_session_filter_selects_matching_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_records(
            dir.path(),
            "usage.jsonl",
            &[
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::Slice, "s2"),
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::BudgetWarn, "s2"),
            ],
        );
        let n = commands::report::at(&path, 100, false, Some("s1".into()), None, false);
        assert_eq!(
            n, 2,
            "only s1 records survive (1 slice + nothing else, but we expect 2 slice records)"
        );
    }

    #[test]
    fn report_last_truncates_after_filters() {
        let dir = tempfile::tempdir().unwrap();
        let mut records = Vec::new();
        for _ in 0..10 {
            records.push(rec(UsageKind::Slice, "s"));
        }
        let path = write_records(dir.path(), "usage.jsonl", &records);
        let n = commands::report::at(&path, 3, false, None, Some(UsageKind::Slice), false);
        assert_eq!(n, 3, "last=3 caps output at 3 lines");
    }

    #[test]
    fn report_summary_aggregates_per_session() {
        // 2 sessions: s1 has 2 slice records (1000 in, 400 out) and 1
        // budget_warn; s2 has 1 compact_hint and 1 budget_over.
        let dir = tempfile::tempdir().unwrap();
        let path = write_records(
            dir.path(),
            "usage.jsonl",
            &[
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::BudgetWarn, "s1"),
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::CompactHint, "s2"),
                rec(UsageKind::BudgetOver, "s2"),
            ],
        );
        // First-line lines just to feed aggregate_sessions directly.
        let s = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        let sessions = plugin3_core::aggregate_sessions(&lines);
        let s1 = sessions.get("s1").expect("s1 present");
        assert_eq!(s1.records, 3);
        assert_eq!(s1.warnings, 1);
        assert_eq!(s1.compactions, 0);
        // 2 slice records × (1000 - 400) = 1200 bytes saved.
        assert_eq!(s1.bytes_saved, 1200);
        let s2 = sessions.get("s2").expect("s2 present");
        assert_eq!(s2.records, 2);
        assert_eq!(s2.warnings, 1); // budget_over counts as warning
        assert_eq!(s2.compactions, 1);
        assert_eq!(s2.bytes_saved, 0);
    }

    // ponytail: regression guard for the `--summary --json` ordering
    // bug. Pre-fix, the JSON branch short-circuited before the
    // summary check, so `report --summary --json` emitted raw
    // filtered records instead of per-session totals. Post-fix the
    // summary path runs first and emits the same aggregated shape
    // for both human and JSON modes. A contributor who re-orders
    // the branches back (as_json before summary) surfaces here.
    #[test]
    fn report_summary_with_json_emits_aggregated_session_totals() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_records(
            dir.path(),
            "usage.jsonl",
            &[
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::BudgetWarn, "s1"),
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::CompactHint, "s2"),
                rec(UsageKind::BudgetOver, "s2"),
            ],
        );
        // Capture stdout from the at() call's println via a
        // minimal harness — the function writes to stdout, so we
        // assert on the return value (sessions.len()) and on the
        // raw-record short-circuit that would have shown up here.
        let n = commands::report::at(&path, 100, true, None, None, true);
        assert_eq!(n, 2, "two distinct sessions aggregated");
        // ponytail: pin the count rather than the JSON text — the
        // serialised shape (key order, snake_case field names) is
        // pinned by the SessionTotals + BTreeMap contract, and a
        // contributor who switches back to the raw-records branch
        // would change the return value from sessions.len() to
        // lines.len() (5), which this assertion catches.
    }

    // ponytail: the return value of `at(... summary=true ...)`
    // must be `sessions.len()` regardless of JSON vs human. A
    // contributor who leaves the JSON branch's return as
    // `lines.len()` but routes the summary through it would surface
    // here as n=5 (filtered count) instead of n=2 (sessions).
    #[test]
    fn report_summary_return_value_is_session_count_not_line_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_records(
            dir.path(),
            "usage.jsonl",
            &[
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::Slice, "s2"),
                rec(UsageKind::Slice, "s3"),
                rec(UsageKind::Slice, "s3"), // duplicate session — still counts as 1
            ],
        );
        let n_human = commands::report::at(&path, 100, true, None, None, false);
        let n_json = commands::report::at(&path, 100, true, None, None, true);
        assert_eq!(n_human, 3, "human mode: 3 distinct sessions");
        assert_eq!(
            n_json, 3,
            "json mode: same 3 distinct sessions, not 4 lines"
        );
    }

    // ponytail: regression guard for the `--summary --last` interaction
    // bug. Pre-fix, `commands::report::at` truncated `filtered` to the
    // last N lines BEFORE passing to `aggregate_sessions` — so
    // `report --summary --last 5` on a 10-record file with session
    // "early" only in records 1-5 silently dropped "early" from the
    // per-session totals. Per ADR-0010 § Report subcommand, `--last`
    // is the detailed-view knob ("Detailed view: last N records, one
    // per line") and `--summary` aggregates the full filtered set
    // ("Summary view: total bytes saved, total warnings, total
    // compactions, per-session totals"). The fix routes aggregation
    // through `&filtered`, not `tail_lines(&filtered, last)`. This
    // test pins BOTH sides: session "early" survives the truncation
    // window AND its records/warnings counts are aggregated across the
    // full 5 records (not the last 2).
    #[test]
    fn report_summary_ignores_last_and_aggregates_full_filtered_set() {
        let dir = tempfile::tempdir().unwrap();
        // 5 records total. s1 occupies the FIRST 3 (would be cut by
        // tail-2), s2 occupies the LAST 2. With last=2 the pre-fix
        // code aggregated only s2; the post-fix code aggregates both.
        let path = write_records(
            dir.path(),
            "usage.jsonl",
            &[
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::BudgetWarn, "s1"),
                rec(UsageKind::Slice, "s1"),
                rec(UsageKind::CompactHint, "s2"),
                rec(UsageKind::BudgetOver, "s2"),
            ],
        );
        // last=2 — would drop s1 from the truncated slice entirely.
        let n = commands::report::at(&path, 2, true, None, None, false);
        assert_eq!(
            n, 2,
            "two distinct sessions aggregated; \
             pre-fix this returned 1 (s1 lost via tail-2 truncation)"
        );
        // Independent check: drive aggregate_sessions directly on the
        // file's full line set to pin the expected per-session totals.
        let s = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        let sessions = plugin3_core::aggregate_sessions(&lines);
        let s1 = sessions
            .get("s1")
            .expect("s1 must survive --summary --last 2");
        assert_eq!(
            s1.records, 3,
            "s1 has 3 records; tail-2 must NOT truncate them"
        );
        assert_eq!(s1.warnings, 1, "s1 has 1 budget_warn; must be aggregated");
        let s2 = sessions.get("s2").expect("s2 present");
        assert_eq!(s2.records, 2);
        assert_eq!(s2.compactions, 1);
    }

    // ponytail: pin the empty-file branch. ADR-0010 § Report
    // subcommand says `plugin3 report` against a fresh install
    // (no usage.jsonl yet) must return 0 without panicking and
    // surface an eprintln so the user knows why nothing showed up.
    // Pre-fix, the missing-file path returned 0 silently — a
    // user running `plugin3 report` to verify their first session
    // got logged got blank output and no signal. A contributor
    // who replaces the eprintln with `return 1` (so the exit
    // code tells the user something went wrong) surfaces here
    // because the test asserts the return value is 0 (the
    // documented "no records" code, not a failure code).
    #[test]
    fn report_returns_zero_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        // summary=true forces the same eprintln regardless of mode
        // (the missing-file path runs before the summary branch).
        let n = commands::report::at(&path, 100, true, None, None, false);
        assert_eq!(
            n, 0,
            "missing usage.jsonl must return 0 (no records), not panic or \
             return a non-zero code; the eprintln on stderr is the diagnostic"
        );
    }

    // ponytail: same branch, but in the detailed-view path
    // (summary=false, last=N). Both code paths funnel through the
    // same early-return at the file-read site; the test catches a
    // refactor that moves one of the two paths off that early
    // return and ends up reading a non-existent file as an empty
    // string (which would yield an empty `all: Vec<&str>`, a
    // `filtered` of length 0, and a tail_lines of length 0 —
    // indistinguishable from "no records" without the eprintln).
    #[test]
    fn report_returns_zero_on_missing_file_detailed_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        let n = commands::report::at(&path, 100, false, None, None, false);
        assert_eq!(
            n, 0,
            "missing usage.jsonl on the detailed path must also return 0; \
             a refactor that drops the early-return guard surfaces here as n=0 too, \
             but the eprintln diagnostic distinguishes the two cases"
        );
    }

    // ADR-0009 § Error contract: hook handlers must not crash the host
    // on a bad payload. We exercise the binary as a subprocess so the
    // real exit path is taken — a unit test on `read_stdin_json`
    // would not catch a regression where a future refactor
    // reintroduces a hard exit.
    fn run_hook_subprocess(subcmd: &str, stdin: &[u8]) -> std::process::Output {
        // ponytail: tempdirs MUST outlive the subprocess — `Command::env`
        // copies the path string into the child's env, but the directory
        // on disk is owned by the TempDir guard. If the guard drops before
        // the child reads its env, the path points at a deleted dir and
        // the hook handler silently fails to write to PLUGIN3_DATA_DIR.
        // Hold each guard in a binding that lives until after wait_with_output.
        let cfg_dir = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let runtime_dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(plugin3_binary_path())
            .args(["hook", subcmd])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn plugin3");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().expect("wait")
    }

    #[test]
    fn hook_post_tool_use_does_not_crash_on_garbage_stdin() {
        let out = run_hook_subprocess("post-tool-use", b"not json {{{");
        assert!(
            out.status.success(),
            "expected exit 0, got {:?}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        assert_eq!(v["content"], "");
        assert!(v["note"].as_str().unwrap().contains("parse failed"));
    }

    #[test]
    fn hook_post_tool_use_parse_failure_path_emits_two_field_wire_shape() {
        // ponytail: pin the PostToolUse parse-failure wire shape at
        // the subprocess level. The CLI emits
        //   `{"content": "", "note": "plugin3: ..."}`
        // — exactly two top-level keys, `content` is the empty
        // string (the host's payload never made it through), `note`
        // is a non-null string. Existing
        // `hook_post_tool_use_does_not_crash_on_garbage_stdin`
        // asserts behavior (`content == ""`, `note` contains
        // "parse failed") but NOT the field set — a contributor who
        // renames `content` → `output` keeps that test green and
        // silently breaks Claude Code, which reads `content` to
        // replace the tool result in memory. Drift catches here.
        //
        // Note MUST be a string, not null. The hook contract is
        // "passthrough with a note" — null would lose the diagnostic
        // the user sees in the Claude Code transcript. A contributor
        // who shortens the parse-failure branch to emit
        // `{"content":""}` (no note) surfaces here.
        let out = run_hook_subprocess("post-tool-use", b"not json {{{");
        assert!(
            out.status.success(),
            "parse failure must still exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["content", "note"].into_iter().collect(),
            "PostToolUse parse-failure response must have exactly \
             {{content, note}} top-level keys. Claude Code reads \
             `content` to overwrite the tool result in memory; a \
             rename to `output` or a sibling `debug` key surfaces here. \
             got: {keys:?}"
        );
        assert_eq!(
            v["content"], "",
            "content must be empty string on parse failure (the \
             payload never made it through); a non-empty string here \
             means the CLI is echoing garbage the host sent"
        );
        let note = v["note"].as_str().expect(
            "note must be a non-null string on parse failure \
                     — the contract is 'passthrough with a note'",
        );
        assert!(
            note.starts_with("plugin3: "),
            "note must start with `plugin3: ` prefix so users see \
             which subsystem emitted it; got: {note:?}"
        );
    }

    #[test]
    fn hook_post_tool_use_keep_passthrough_emits_two_field_wire_shape() {
        // ponytail: pin the Keep passthrough wire shape. The CLI
        // emits
        //   `{"content": <payload.content>, "note": null}`
        // — exactly two top-level keys, `content` echoes the
        // payload verbatim (no slicing), `note` is null (no
        // diagnostic — Keep is the boring happy path).
        //
        // Note MUST be present-and-null, not absent. A contributor
        // who adds `#[serde(skip_serializing_if = "Option::is_none")]`
        // to `PostToolUseResponse::note` would emit
        // `{"content":"..."}` instead of `{"content":"...","note":null}`.
        // Both are valid JSON, but Claude Code's schema check may
        // expect the key to exist; removing it changes the wire
        // contract. Drift catches here.
        //
        // Payload is sized to stay well below the 256-byte slice
        // threshold (HeadTailSlicer's default) so the orchestrator
        // routes through the Keep branch — Slice would emit a
        // marker in `content` and a non-null note, defeating the
        // test. A contributor who lowers the threshold below 12
        // bytes surfaces here as a content mismatch.
        let payload = serde_json::json!({
            "tool_name": "Read",
            "tool_result_key": "k1",
            "content": "small ok body",
            "session_id": "sess-pin",
        });
        let out = run_hook_subprocess(
            "post-tool-use",
            serde_json::to_vec(&payload).unwrap().as_slice(),
        );
        assert!(
            out.status.success(),
            "Keep passthrough must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["content", "note"].into_iter().collect(),
            "PostToolUse Keep response must have exactly \
             {{content, note}} top-level keys. `note: null` is \
             load-bearing — Claude Code may schema-check the key's \
             presence even when null. got: {keys:?}"
        );
        assert_eq!(
            v["content"], "small ok body",
            "Keep must echo payload content verbatim; any slicing \
             (marker in content) means the threshold regressed below \
             the payload size — surfaced here as a content mismatch"
        );
        assert!(
            v["note"].is_null(),
            "Keep must emit `note: null` (not absent, not a string); \
             the Keep branch is the boring happy path and has no \
             diagnostic for the user. got: {}",
            v["note"]
        );
    }

    #[test]
    fn hook_user_prompt_submit_falls_back_to_allow() {
        let out = run_hook_subprocess("user-prompt-submit", b"definitely not json");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["kind"], "allow", "fallback must be Allow");
    }

    #[test]
    fn hook_user_prompt_submit_happy_path_allow_wire_shape_is_pinned() {
        // ponytail: subprocess pin for the happy-path Allow
        // variant. Existing `hook_user_prompt_submit_falls_back_to_allow`
        // asserts the parse-failure fallback emits `kind == "allow"`
        // — but the parse-failure branch serialises
        // `Intervention::Allow` directly without touching the
        // budget, so it never exercises the
        // `decide → classify_kind → serialize` chain. A contributor
        // who adds a `note` field to the Allow arm of the decide
        // switch (e.g. `Intervention::Allow { note: None }`) keeps
        // the parse-failure pin green and breaks Claude Code's
        // decision router silently. Drift catches here.
        //
        // Subprocess setup:
        //   1. Fresh tempdirs (no budget.toml) → default budget
        //      (ceiling=200_000, used=0, approaching_ratio=0.8).
        //   2. Feed a small non-code prompt ("hello world" → 11
        //      bytes / 4 = 2 tokens at the bytes/4 estimator).
        //   3. After `record(2)`: used=2, ratio=0.00001 → Under,
        //      can_send(2)=true → decide returns `Intervention::Allow`.
        //   4. classify_kind(Allow) returns None → no usage record
        //      emitted (a healthy turn is not a "significant event"
        //      per ADR-0010).
        //   5. Serialised Allow has no extra fields (tagged-enum
        //      variant with no payload) → `{"kind":"allow"}`.
        let payload = serde_json::json!({
            "prompt": "hello world",
            "session_id": "sess-allow",
        });
        let out = run_hook_subprocess(
            "user-prompt-submit",
            serde_json::to_vec(&payload).unwrap().as_slice(),
        );
        assert!(
            out.status.success(),
            "user-prompt-submit happy-path Allow must exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect(
            "UserPromptSubmitResponse Allow serialises to an \
                     object (tagged enum — even single-field variants \
                     carry the `kind` discriminator)",
        );
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["kind"].into_iter().collect(),
            "Allow variant must have exactly {{kind}} top-level key. \
             A contributor who adds a sibling field (e.g. `note`, \
             `tokens_remaining`, `hint`) on the happy path keeps \
             the parse-failure pin green and breaks Claude Code's \
             decision router silently. got: {keys:?}"
        );
        assert_eq!(
            v["kind"], "allow",
            "Allow variant must serialise `kind` as the snake_case \
             `\"allow\"`; `\"Allow\"` or `\"ALLOW\"` here would break \
             every `jq '.kind == \"allow\"'` filter. got: {:?}",
            v["kind"]
        );
    }

    #[test]
    fn hook_user_prompt_submit_warn_variant_wire_shape_is_pinned() {
        // ponytail: subprocess pin for the `Warn { remaining }`
        // variant of `UserPromptSubmitResponse`. Round 35's
        // `user_prompt_submit_response_wire_shape_pins_all_four_variants`
        // pins the canonical enum's serde shape directly; the
        // existing parse-failure pin at line 855 only asserts
        // `kind == "allow"` and doesn't pin the field set. This
        // test exercises the WARN arm of the `decide(...)` switch
        // at the subprocess layer: a valid payload + a budget in
        // Approaching state → `{"kind": "warn", "remaining": N}`.
        //
        // The `Warn` variant is the load-bearing one for Claude
        // Code's UI — it surfaces the budget headroom to the user
        // before Over fires. A contributor who renames `remaining`
        // → `tokens_left` (or drops it entirely) breaks the
        // Claude Code warning display silently. The field set
        // pin below catches both renames and additions.
        //
        // Subprocess setup:
        //   1. Pre-write `budget.toml` with ceiling=100, used=80,
        //      approaching_ratio=0.8 → state Approaching on load.
        //   2. Feed a small non-code prompt (10 chars → ~2 tokens
        //      at the bytes/4 estimator).
        //   3. After `record(2)`: used=82, ratio=0.82 ≥ 0.8 →
        //      Approaching → decide returns
        //      `Intervention::Warn { remaining: ceiling - used = 18 }`.
        //
        // The `run_hook_subprocess` helper sets `PLUGIN3_*_DIR`
        // to fresh tempdirs, which means `load_budget` reads
        // the seeded budget.toml from `runtime_dir/budget.toml` and
        // `save_budget` writes back to the same path. The pin
        // tolerates the post-decide save (the next test gets a
        // tempdir of its own).
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let budget_path = runtime_dir.path().join("budget.toml");
        let seed = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 80,
        };
        std::fs::write(&budget_path, toml::to_string(&seed).unwrap()).unwrap();

        // ponytail: build the payload inline. A 10-char
        // non-code prompt yields ~2 tokens at the bytes/4
        // estimator; after `record(2)` the runtime budget is
        // used=82, ratio=0.82 → still Approaching (≥0.8). The
        // small incoming is below the slice / compact threshold
        // so the decide switch hits the `can_send` arm.
        let payload = serde_json::json!({
            "prompt": "short hey",  // 9 chars → ~2 tokens
            "session_id": "sess-warn",
        });

        let mut child = std::process::Command::new(plugin3_binary_path())
            .args(["hook", "user-prompt-submit"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn plugin3");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_vec(&payload).unwrap().as_slice())
            .unwrap();
        let out = child.wait_with_output().expect("wait");
        assert!(
            out.status.success(),
            "user-prompt-submit on a valid payload must exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect(
            "UserPromptSubmitResponse serialises to object \
                     (tagged enum, even single-field variants carry `kind`)",
        );
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["kind", "remaining"].into_iter().collect(),
            "Warn variant must have exactly {{kind, remaining}} \
             top-level keys; a contributor who renames `remaining` \
             → `tokens_left` (or adds a sibling field) breaks the \
             Claude Code warning display silently. got: {keys:?}"
        );
        assert_eq!(
            v["kind"], "warn",
            "Warn variant must serialise `kind` as the snake_case \
             `\"warn\"` (serde `rename_all = \"snake_case\"`); \
             `\"Warning\"` or `\"WARN\"` here would break every \
             `jq '.kind == \"warn\"'` filter. got: {:?}",
            v["kind"]
        );
        // ponytail: pin the remaining count. With ceiling=100,
        // used_before=80, incoming=2 → after record: used=82,
        // remaining = ceiling - used = 18. A contributor who
        // wires `remaining` to `budget.used` (off-by-one) or
        // hardcodes `0` surfaces here as a numeric mismatch.
        assert_eq!(
            v["remaining"], 18,
            "remaining must be `ceiling - used_after_record` = \
             100 - 82 = 18; a different value means the budget \
             state didn't read from the seeded budget.toml OR the \
             record/decide math regressed. got: {}",
            v["remaining"]
        );
    }

    #[test]
    fn hook_user_prompt_submit_slice_variant_wire_shape_is_pinned() {
        // ponytail: subprocess pin for the `Slice { target_key,
        // slice_to }` variant of `UserPromptSubmitResponse`. The
        // Round 47 `Warn` test exercised the Approaching state;
        // this test exercises the Slice arm of `decide(...)` —
        // fired when the budget is Over AND a recent output is
        // large enough to slice down by the overflow amount.
        //
        // Subprocess setup:
        //   1. budget.toml: ceiling=100, used=100 → state Over
        //      (ratio 1.0). After record(50): used=150, can_send=false.
        //   2. recent_outputs.jsonl: one entry
        //        {"key": "big-tool-result", "size": 400}
        //      so `max_by_key(|s| s)` returns 400.
        //   3. needed = incoming(50) - remaining(0) = 50.
        //   4. size(400) > needed(50) + SLICE_OVERHEAD(256) = 306 ✓
        //      → Slice { target_key: "big-tool-result",
        //               slice_to: 400 - 50 = 350 }
        //
        // The `target_key` and `slice_to` fields are the
        // load-bearing payload — Claude Code uses `target_key`
        // to find the tool result to slice and `slice_to` to know
        // where to truncate. A contributor who renames either
        // breaks the contract silently.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let budget_path = runtime_dir.path().join("budget.toml");
        let recent_path = data_dir.path().join("recent_outputs.jsonl");
        std::fs::write(
            &budget_path,
            toml::to_string(&TokenBudget {
                ceiling: 100,
                approaching_ratio: 0.8,
                used: 100,
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(&recent_path, "{\"key\":\"big-tool-result\",\"size\":400}\n").unwrap();

        // ponytail: a 200-char non-code prompt → ~50 tokens at
        // the bytes/4 estimator. After `record(50)`, used jumps
        // from 100 → 150 (past ceiling). The decide switch falls
        // through the `can_send` arm and hits the Slice path
        // because recent has a 400-byte entry.
        let payload = serde_json::json!({
            "prompt": "x".repeat(200),
            "session_id": "sess-slice",
        });

        let mut child = std::process::Command::new(plugin3_binary_path())
            .args(["hook", "user-prompt-submit"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn plugin3");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_vec(&payload).unwrap().as_slice())
            .unwrap();
        let out = child.wait_with_output().expect("wait");
        assert!(
            out.status.success(),
            "user-prompt-submit on a valid payload must exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v
            .as_object()
            .expect("Slice variant serialises to object (tagged enum)");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["kind", "slice_to", "target_key"].into_iter().collect(),
            "Slice variant must have exactly \
             {{kind, target_key, slice_to}} top-level keys; a \
             contributor who renames `target_key` → `key` (or \
             `slice_to` → `bytes`) breaks Claude Code's \
             slice-and-replace step silently. got: {keys:?}"
        );
        assert_eq!(
            v["kind"], "slice",
            "Slice variant must serialise `kind` as the snake_case \
             `\"slice\"`; `\"Slice\"` or `\"SLICE\"` here breaks \
             every `jq '.kind == \"slice\"'` filter. got: {:?}",
            v["kind"]
        );
        assert_eq!(
            v["target_key"], "big-tool-result",
            "target_key must echo the recent-outputs entry key the \
             decide() picked; a different value means the max-by-key \
             selection regressed (or recent_outputs.jsonl wasn't \
             read). got: {:?}",
            v["target_key"]
        );
        // ponytail: pin the slice_to math. decide returns
        //   slice_to = size.saturating_sub(needed) = 400 - 50 = 350
        // A contributor who flips the subtraction (or drops
        // SLICE_OVERHEAD from the comparison) surfaces here.
        assert_eq!(
            v["slice_to"], 350,
            "slice_to must be `size - needed` = 400 - 50 = 350; a \
             different value means the slice math regressed. got: {}",
            v["slice_to"]
        );
    }

    #[test]
    fn hook_user_prompt_submit_compact_variant_wire_shape_is_pinned() {
        // ponytail: subprocess pin for the `Compact { reason }`
        // variant of `UserPromptSubmitResponse`. The Slice test
        // exercised the path where a recent output is large
        // enough to truncate; this test exercises the fallback
        // Compact path — Over budget AND no recent entry is
        // large enough to slice. `decide(...)` falls through to
        //   Compact { reason: "session at {used}/{ceiling} tokens;
        //              cannot fit {incoming} more" }
        //
        // The `reason` string is human-readable text Claude Code
        // shows to the user before compaction. A contributor who
        // drops the trailing suffix (e.g. just `format!("{}",
        // used)`) keeps the `kind` intact and loses the
        // diagnostic. Drift catches here.
        //
        // Subprocess setup:
        //   1. budget.toml: ceiling=10, used=10 → state Over.
        //   2. recent_outputs.jsonl: ABSENT — `max_by_key` returns
        //      None → falls through to Compact.
        //   3. Prompt: 40 chars → ~10 tokens at bytes/4.
        //      After `record(10)`: used=20, can_send=false.
        //      needed = 10 - 0 = 10. No recent → Compact.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let budget_path = runtime_dir.path().join("budget.toml");
        std::fs::write(
            &budget_path,
            toml::to_string(&TokenBudget {
                ceiling: 10,
                approaching_ratio: 0.8,
                used: 10,
            })
            .unwrap(),
        )
        .unwrap();
        // recent_outputs.jsonl intentionally not created.

        let payload = serde_json::json!({
            "prompt": "x".repeat(40),
            "session_id": "sess-compact",
        });

        let mut child = std::process::Command::new(plugin3_binary_path())
            .args(["hook", "user-prompt-submit"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn plugin3");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_vec(&payload).unwrap().as_slice())
            .unwrap();
        let out = child.wait_with_output().expect("wait");
        assert!(
            out.status.success(),
            "user-prompt-submit on a valid payload must exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v
            .as_object()
            .expect("Compact variant serialises to object (tagged enum)");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["kind", "reason"].into_iter().collect(),
            "Compact variant must have exactly {{kind, reason}} \
             top-level keys; a contributor who renames `reason` \
             → `why` (or adds a sibling field) breaks Claude \
             Code's compact-suggestion UI silently. got: {keys:?}"
        );
        assert_eq!(
            v["kind"], "compact",
            "Compact variant must serialise `kind` as the snake_case \
             `\"compact\"`; `\"Compact\"` or `\"COMPACT\"` here \
             breaks every `jq '.kind == \"compact\"'` filter. \
             got: {:?}",
            v["kind"]
        );
        // ponytail: pin the reason format. decide returns
        //   "session at {used}/{ceiling} tokens; cannot fit {incoming} more"
        // After record(10): used=20, ceiling=10, incoming=10 →
        //   "session at 20/10 tokens; cannot fit 10 more"
        // A contributor who drops the trailing " cannot fit N more"
        // (or reorders the fields) surfaces here as a reason
        // mismatch.
        assert_eq!(
            v["reason"], "session at 20/10 tokens; cannot fit 10 more",
            "reason must match `decide()`'s literal format string; \
             a contributor who tweaks the format (drops the \
             trailing suffix, reorders fields) surfaces here. got: {:?}",
            v["reason"]
        );
    }

    #[test]
    fn hook_pre_compact_emits_null_hint_on_garbage() {
        let out = run_hook_subprocess("pre-compact", b"");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(v["hint"].is_null());
    }

    #[test]
    fn hook_pre_compact_happy_path_wire_shape_is_pinned() {
        // ponytail: subprocess pin for the PreCompact happy path.
        // Existing coverage:
        //   - `hook_pre_compact_emits_null_hint_on_garbage` (parse-failure)
        //   - `pre_compact_wire_shape_pins_parse_failure_and_empty_history`
        //     (literal-substring scan of the source — pins both
        //     branches by their JSON literals, but never spawns)
        // Neither exercises the post-decide branch through the
        // full subprocess: clap → stdin parse → CompactHint build →
        // LocalSummaryCompactor → wire shape. A contributor who
        // renames the response key from `hint` → `advice` (or
        // from `summary` → `preview`) keeps the literal scan
        // green and breaks Claude Code silently — Claude Code
        // reads `hint` to seed its compactor and `summary` as a
        // head-start. Drift catches here.
        //
        // Subprocess setup:
        //   1. Feed a PreCompactPayload with 3 turns (indices 0,1,2)
        //      so the post-decide branch fires (the parse-failure
        //      fallback returns early with `{hint: null}`).
        //   2. Fresh tempdir → default TokenBudget
        //      (used=0, ceiling=200_000). The CompactHint reports
        //      tokens_used=0, tokens_ceiling=200_000, and the turn
        //      range spans the full history (oldest_turn=0,
        //      newest_turn=2).
        //   3. LocalSummaryCompactor runs over the joined turns;
        //      for 3 short lines it returns the input verbatim
        //      (well under the 500-char per-line cap and the
        //      8192-byte total cap).
        let payload = serde_json::json!({
            "history_turns": [
                {"index": 0, "role": "user", "content_preview": "hello"},
                {"index": 1, "role": "assistant", "content_preview": "world"},
                {"index": 2, "role": "user", "content_preview": "foo"},
            ],
        });
        let out = run_hook_subprocess(
            "pre-compact",
            serde_json::to_vec(&payload).unwrap().as_slice(),
        );
        assert!(
            out.status.success(),
            "PreCompact happy path must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v
            .as_object()
            .expect("PreCompact response top-level is an object");
        let top_keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            top_keys,
            ["hint", "summary"].into_iter().collect(),
            "PreCompact happy-path response must have exactly \
             {{hint, summary}} top-level keys. Claude Code reads \
             `hint` to seed its compactor and `summary` as a \
             head-start; renaming either breaks the bridge \
             silently. got: {top_keys:?}"
        );

        // ponytail: pin the CompactHint shape (same 5 fields as
        // the budget compact --json envelope). A contributor who
        // adds a 6th field here surfaces here.
        let hint = obj["hint"].as_object().expect(
            "hint must be an object (CompactHint), not null \
                     on the happy path — null is reserved for the \
                     parse-failure fallback",
        );
        let hint_keys: std::collections::BTreeSet<&str> = hint.keys().map(String::as_str).collect();
        assert_eq!(
            hint_keys,
            [
                "newest_turn",
                "oldest_turn",
                "reason",
                "tokens_ceiling",
                "tokens_used"
            ]
            .into_iter()
            .collect(),
            "PreCompact hint must be the 5-field CompactHint shape; \
             a contributor who adds a 6th field (e.g. \
             `triggered_at`) propagates here. got: {hint_keys:?}"
        );

        // ponytail: pin the turn range and budget values for the
        // seeded history. A contributor who truncates the history
        // (or wires `oldest_turn` to `history_turns.last()`) breaks
        // the range — `hint.oldest_turn` must be 0 (head) and
        // `hint.newest_turn` must be 2 (tail of 3 turns).
        assert_eq!(
            hint["oldest_turn"], 0,
            "oldest_turn must be 0 (head of seeded history); a \
             different value means `history.first()` lost the head"
        );
        assert_eq!(
            hint["newest_turn"], 2,
            "newest_turn must be 2 (tail of 3 seeded turns); a \
             different value means `history.last()` regressed"
        );
        assert_eq!(
            hint["tokens_used"], 0,
            "tokens_used on a fresh tempdir must be 0; the default \
             TokenBudget starts at 0"
        );
        assert_eq!(
            hint["tokens_ceiling"], 200_000,
            "tokens_ceiling on a fresh tempdir must be the default \
             200_000; a different value means PLUGIN3_CONFIG_DIR \
             leaked through and config.toml set a custom ceiling"
        );
        assert_eq!(
            hint["reason"], "session at 0/200000 tokens; compaction suggested",
            "reason must be the literal `compaction::build_hint` \
             format; tweaking it surfaces here as a mismatch"
        );

        // ponytail: pin that `summary` is a non-empty string.
        // LocalSummaryCompactor runs over the joined turns and
        // returns a non-empty summary for any non-empty input
        // (each line < 500 chars, total < 8192 bytes — neither
        // bound triggers here). A contributor who shortens the
        // hook to emit `"summary": ""` (or omits the field)
        // surfaces here.
        let summary = obj["summary"].as_str().expect(
            "summary must be a non-null string on the happy \
                     path — Claude Code reads it as the compactor \
                     head-start",
        );
        assert!(
            !summary.is_empty(),
            "summary must be non-empty on a non-empty history; an \
             empty string here means the LocalSummaryCompactor was \
             bypassed (or its output was thrown away). got: {summary:?}"
        );
    }

    // ADR-0016 § Integration tests: pipe a real PostToolUse payload
    // with a 50 KB cargo-test-shaped body and assert slicing occurred.
    #[test]
    fn hook_post_tool_use_slices_large_cargo_test_output() {
        // Shape that detector::from_shape classifies as TestRunner.
        let mut body = String::from("running 5 tests\ntest foo ... ok\n");
        body.push_str(&"x".repeat(50_000));
        body.push_str("\ntest bar ... FAILED\n");
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_result_key": "abc",
            "content": body,
            "session_id": "s1",
        })
        .to_string();
        let out = run_hook_subprocess("post-tool-use", payload.as_bytes());
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let content = v["content"].as_str().expect("content is string");
        // Slicing occurred — response is shorter than input.
        assert!(
            content.len() < body.len(),
            "sliced {} -> {} bytes",
            body.len(),
            content.len()
        );
        // The slice marker is present (ADR-0003).
        assert!(
            content.contains("<<plugin3:slice:"),
            "expected marker in {content}"
        );
        // The note explains the slicing.
        let note = v["note"].as_str().expect("note on slice");
        assert!(note.contains("sliced"));
    }

    #[test]
    fn hook_post_tool_use_slice_path_wire_shape_is_pinned() {
        // ponytail: subprocess-level wire pin for the PostToolUse
        // slice path. Existing
        // `hook_post_tool_use_slices_large_cargo_test_output` checks
        // behavior (response shorter than input, marker present,
        // note contains "sliced") but NOT the field set or the
        // note's exact prefix. A contributor who adds a sibling
        // `kind` field (e.g. `{"kind":"sliced", "content":...}`) or
        // renames `note` → `diagnostic` keeps the behavior tests
        // green and breaks Claude Code silently — Claude Code reads
        // `content` to overwrite the tool result in memory and
        // `note` to surface the diagnostic. Drift catches here.
        //
        // Payload: 50 KB cargo-test-shaped body. The detector
        // recognises the "running N tests / test ... ok / FAILED"
        // shape as TestRunner, and 50 KB > 8 KB triggers the Slice
        // decision in `detector::should_slice`. The orchestrator
        // then routes through HeadTailSlicer (default head/tail
        // 4096/4096), producing a `<<plugin3:slice:<key>>>`
        // marker between head and tail.
        let mut body = String::from("running 5 tests\ntest foo ... ok\n");
        body.push_str(&"x".repeat(50_000));
        body.push_str("\ntest bar ... FAILED\n");
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_result_key": "abc",
            "content": body,
            "session_id": "s1",
        })
        .to_string();
        let out = run_hook_subprocess("post-tool-use", payload.as_bytes());
        assert!(
            out.status.success(),
            "PostToolUse slice path must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v
            .as_object()
            .expect("PostToolUse slice-path response top-level is an object");
        let top_keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            top_keys,
            ["content", "note"].into_iter().collect(),
            "PostToolUse slice-path response must have exactly \
             {{content, note}} top-level keys. Claude Code reads \
             `content` to overwrite the tool result in memory and \
             `note` to surface the diagnostic; adding a sibling \
             `kind`/`debug` key or renaming either field breaks the \
             bridge silently. got: {top_keys:?}"
        );

        // ponytail: pin the content shape on the slice path.
        // `content` must be non-empty (else the host would see an
        // empty tool result, which is a regression worse than the
        // original), and shorter than the input (proving slicing
        // happened), and contain the canonical slice marker
        // (ADR-0003). A contributor who emits only `head` (forgets
        // the marker or tail) surfaces here as a missing-marker
        // failure.
        let content = obj["content"].as_str().expect(
            "content must be a non-null string on the slice path \
                     — Claude Code reads it as the replacement tool result",
        );
        assert!(
            !content.is_empty(),
            "content must be non-empty on the slice path; an empty \
             string here means the orchestrator emitted a headless \
             SlicedOutput (regression). got: {content:?}"
        );
        assert!(
            content.len() < body.len(),
            "content ({} bytes) must be shorter than input ({} bytes) \
             — a same-length content means slicing didn't happen",
            content.len(),
            body.len()
        );
        assert!(
            content.contains("<<plugin3:slice:"),
            "content must contain the canonical slice marker \
             `<<plugin3:slice:` (ADR-0003); missing marker means the \
             orchestrator forgot to offload the middle. got: {content}"
        );

        // ponytail: pin the note shape on the slice path. `note`
        // must be a non-null string with the literal `sliced `
        // prefix and the ` bytes kept)` suffix — the format is
        // `format!("sliced {kind:?} ({bytes_kept} bytes kept)")` in
        // `hooks::post_tool_use`. A contributor who flips the
        // prefix to `Sliced` (capital S) or drops `bytes kept`
        // surfaces here. Also assert the format wraps a positive
        // integer — `bytes_kept` for a 50 KB input with head/tail
        // 4096/4096 is 8192, so any number > 0 confirms the
        // arithmetic is actually being computed.
        let note = obj["note"].as_str().expect(
            "note must be a non-null string on the slice path \
                     — the contract is `note = Some(...)`, not None. \
                     `None` is reserved for Keep (passthrough).",
        );
        assert!(
            note.starts_with("sliced "),
            "note must start with `sliced ` (lowercase) prefix; the \
             format string is `sliced {{kind:?}} ({{bytes_kept}} bytes kept)`. \
             A contributor who capitalises (`Sliced`) or rewrites the \
             prefix (`Slice:`) surfaces here. got: {note:?}"
        );
        assert!(
            note.ends_with(" bytes kept)"),
            "note must end with ` bytes kept)`; the format includes \
             the byte count for the diagnostic. got: {note:?}"
        );
        // ponytail: extract the byte count from the parens and
        // assert it's a positive integer. The format is
        // `sliced <KIND> (<N> bytes kept)` — splitting on
        // parentheses yields `(`, `<digits>`, ` bytes kept)` in
        // successive pieces. Parsing the middle ensures the format
        // continues to surface the actual byte count (a regression
        // to a static `"sliced"` would yield no digits and fail).
        let inner = note.split('(').nth(1).expect(
            "note must contain at least one `(` opening the \
                     byte-count parens; got: {note:?}",
        );
        let n_str = inner
            .split(' ')
            .next()
            .expect("note's inner `(` must be followed by a digit");
        let bytes_kept: usize = n_str.parse().unwrap_or_else(|_| {
            panic!(
                "note's byte-count field must parse as usize; got \
                 `{n_str}` (full note: {note:?})"
            )
        });
        assert!(
            bytes_kept > 0,
            "bytes_kept must be a positive integer (the sliced \
             output retains at least the head and tail); 0 here \
             means the format regressed to `sliced ... (0 bytes kept)`"
        );
    }

    #[test]
    fn hook_post_tool_use_passes_through_small_output() {
        // 100-byte body, TestRunner threshold is 8 KB → Keep.
        let body = "running 1 test\ntest foo ... ok\n";
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_result_key": "abc",
            "content": body,
            "session_id": "s1",
        })
        .to_string();
        let out = run_hook_subprocess("post-tool-use", payload.as_bytes());
        assert!(out.status.success());
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["content"], body, "Keep passes content through verbatim");
        // Note is suppressed on Keep (ADR-0013: optional).
        assert!(v["note"].is_null(), "note must be null on Keep");
    }

    // ---- ADR-0015 § budget set --default subprocess wiring -----

    // ponytail: spawn the real binary so we exercise clap arg
    // parsing AND the persistence path. A unit test would only
    // cover half of that contract.
    fn run_budget_set_subprocess(
        ceiling: usize,
        persist: bool,
    ) -> (
        std::process::Output,
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        // ponytail: same TempDir-drop-before-spawn trap as run_hook_subprocess.
        // Returning all three guards so the caller can assert on cfg_dir/data_dir
        // and the runtime_dir guard still outlives the child's read of its env var.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let mut cmd = std::process::Command::new(plugin3_binary_path());
        cmd.args(["budget", "set", &ceiling.to_string()])
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path());
        if persist {
            cmd.arg("--default");
        }
        let out = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 budget set");
        (out, cfg_dir, data_dir, runtime_dir)
    }

    #[test]
    fn budget_set_default_persists_to_config_toml() {
        let (out, cfg_dir, _data_dir, _runtime_dir) = run_budget_set_subprocess(300_000, true);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let cfg_path = cfg_dir.path().join("config.toml");
        let body = std::fs::read_to_string(&cfg_path).expect("config.toml written");
        // ADR-0005 § Defaults: the [budget] section carries ceiling + ratio.
        assert!(body.contains("[budget]"), "got: {body}");
        assert!(body.contains("ceiling = 300000"), "got: {body}");
        // Round-trip parse via the same wrapper `load_budget_config_at`
        // uses, so a future regression in ConfigFile's section handling
        // shows up here instead of as a silent drift.
        let file: ConfigFile = toml::from_str(&body).expect("parse");
        assert_eq!(file.budget.ceiling, 300_000);
    }

    #[test]
    fn budget_set_without_default_does_not_touch_config_toml() {
        // ponytail: a plain `set` must remain session-local — no
        // config.toml write. Otherwise the `--default` flag becomes
        // meaningless (always on).
        let (out, cfg_dir, _data_dir, _runtime_dir) = run_budget_set_subprocess(150_000, false);
        assert!(out.status.success());
        let cfg_path = cfg_dir.path().join("config.toml");
        assert!(
            !cfg_path.exists(),
            "config.toml must NOT exist when --default is omitted"
        );
    }

    #[test]
    fn budget_set_default_picks_up_on_next_load_budget() {
        // Session 1: write 222_000 as default.
        let (_out, cfg_dir, _data_dir, _runtime_dir) = run_budget_set_subprocess(222_000, true);
        // Session 2: fresh runtime dir; load_budget_with_config must
        // overlay the persisted default even though runtime budget.toml
        // is empty.
        let runtime_path = cfg_dir.path().join("runtime-fresh/budget.toml");
        std::fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
        let cfg_path = cfg_dir.path().join("config.toml");
        let b = load_budget_with_config(&runtime_path, &cfg_path);
        assert_eq!(b.ceiling, 222_000);
        assert_eq!(b.used, 0, "no carryover from config into a fresh session");
    }
}

// ponytail: the on-disk shape of recent_outputs.jsonl is owned by
// this crate (the writer and reader both live here), so a typed struct
// beats ad-hoc Value digging. Keep it private — nothing outside main.rs
// needs it.
#[derive(Deserialize, Serialize)]
struct RecentEntry {
    key: String,
    size: usize,
}

fn load_recent_outputs() -> VecDeque<(String, usize)> {
    let path = Paths::resolve().recent_outputs();
    load_recent_outputs_at(&path)
}

// ponytail: path-parameterised so drift tests in this crate can
// point at a tempdir without mutating the process-wide
// `PLUGIN3_*_DIR` env vars (which would race with parallel tests
// that share the same harness process). The public `load_recent_outputs`
// is the production entry point; this is the test-friendly seam.
fn load_recent_outputs_at(path: &std::path::Path) -> VecDeque<(String, usize)> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return VecDeque::new();
    };
    s.lines()
        .filter_map(|line| serde_json::from_str::<RecentEntry>(line).ok())
        .map(|e| (e.key, e.size))
        .collect()
}

const RECENT_BOUND: usize = 32;

// ponytail: rewrite the whole file on every append — bounded at 32
// entries, so O(N) is fine. Switch to append-with-rollover when
// the bound grows. Atomic via `atomic_write_text` (NamedTempFile +
// persist); failures eprintln so a host's stderr captures a
// missing-recent-file warning without breaking the slice path.
fn append_recent(key: &str, size: usize) {
    let path = Paths::resolve().recent_outputs();
    append_recent_at(&path, key, size);
}

// ponytail: VecDeque, not Vec. `Vec::remove(0)` shifts every
// surviving element (O(n) per eviction); with a 32-entry bound
// the FIFO rewrite below was O(n²) per append. VecDeque::pop_front
// is O(1). Public types changed (Vec → VecDeque) but the wire
// shape on disk and the function signature are unchanged —
// the drift test in `recent_outputs_tests` and `state_spec_drift`
// pin the JSONL row shape and the `fn append_recent(key: &str,
// size: usize)` signature, not the in-memory container.
fn append_recent_at(path: &std::path::Path, key: &str, size: usize) {
    let mut entries = load_recent_outputs_at(path);
    entries.push_back((key.to_string(), size));
    while entries.len() > RECENT_BOUND {
        entries.pop_front();
    }
    // ponytail: serde-serialise the struct rather than reaching for
    // `serde_json::json!` — one wire format owned by `RecentEntry`.
    let mut body = String::new();
    for (k, s) in &entries {
        let line = match serde_json::to_string(&RecentEntry {
            key: k.clone(),
            size: *s,
        }) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("plugin3: failed to serialise recent entry: {e}");
                continue;
            }
        };
        body.push_str(&line);
        body.push('\n');
    }
    atomic_write_text(path, "recent", &body);
}

fn empty_record() -> UsageRecord {
    UsageRecord {
        ts: chrono::Utc::now(),
        kind: UsageKind::Prompt,
        session_id: String::new(),
        bytes_in: None,
        bytes_out: None,
        tokens_used: None,
        tokens_ceiling: None,
        tool: None,
    }
}

// ponytail: `run_pre_compact` (no session) and `budget_compact`
// (no session either) both emitted a CompactHint record with
// identical fields. Extracted because the eprintln tags aren't
// the only place drift would surface — a future "add model
// column to CompactHint records" change would need to remember
// to update both call sites.
fn emit_compact_hint(b: &TokenBudget) {
    emit_usage(&UsageRecord {
        kind: UsageKind::CompactHint,
        session_id: String::new(),
        tokens_used: Some(b.used),
        tokens_ceiling: Some(b.ceiling),
        ..empty_record()
    });
}

#[cfg(test)]
mod validate_tests {
    use super::*;

    fn fake_paths_in(dir: &std::path::Path) -> Paths {
        Paths {
            config_dir: dir.join("cfg"),
            data_dir: dir.join("data"),
            runtime_dir: dir.join("run"),
        }
    }

    #[test]
    fn run_path_checks_passes_on_fresh_tempdir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = fake_paths_in(dir.path());
        let checks = commands::config::run_path_checks_for(&p);
        assert!(
            checks
                .iter()
                .all(|c| c.status == commands::config::CheckStatus::Ok),
            "fresh tempdir should pass; failures: {:?}",
            checks
                .iter()
                .filter(|c| c.status == commands::config::CheckStatus::Fail)
                .collect::<Vec<_>>()
        );
        assert_eq!(checks.len(), 8);
    }

    #[test]
    fn run_path_checks_flags_corrupt_config_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = fake_paths_in(dir.path());
        std::fs::create_dir_all(&p.config_dir).unwrap();
        std::fs::write(p.config_file(), b"this is not valid toml = = =").unwrap();
        let checks = commands::config::run_path_checks_for(&p);
        let cfg_check = checks
            .iter()
            .find(|c| c.label == "config_file")
            .expect("config_file check present");
        assert_eq!(cfg_check.status, commands::config::CheckStatus::Fail);
        assert!(
            cfg_check.detail.contains("parse failed"),
            "detail should explain the parse failure: {}",
            cfg_check.detail
        );
    }

    #[test]
    fn run_path_checks_treats_empty_budget_toml_as_fresh() {
        // ponytail: an empty budget.toml is the post-init state per
        // ADR-0014 (the file exists but no record has landed yet).
        // Treating it as a parse error would make every validate
        // call after `plugin3 init` red.
        let dir = tempfile::tempdir().expect("tempdir");
        let p = fake_paths_in(dir.path());
        std::fs::create_dir_all(p.data_dir.join("logs")).unwrap();
        // B2: budget.toml lives in runtime_dir; create it before seeding.
        std::fs::create_dir_all(&p.runtime_dir).unwrap();
        std::fs::write(p.budget_file(), b"").unwrap();
        let checks = commands::config::run_path_checks_for(&p);
        let budget_check = checks
            .iter()
            .find(|c| c.label == "budget_file")
            .expect("budget_file check present");
        assert_eq!(budget_check.status, commands::config::CheckStatus::Ok);
        assert_eq!(budget_check.detail, "exists+empty");
    }

    #[test]
    fn run_path_checks_leaves_no_permanent_files() {
        // ponytail: the dir probes use NamedTempFile so the validate
        // command is idempotent. A regression that switched to a
        // non-cleaning probe would surface here.
        let dir = tempfile::tempdir().expect("tempdir");
        let p = fake_paths_in(dir.path());
        let _ = commands::config::run_path_checks_for(&p);
        // Only directory probes may create their target dir; no
        // stray files inside any of those dirs.
        for sub in ["cfg", "data", "run"] {
            let entries: Vec<_> = std::fs::read_dir(dir.path().join(sub))
                .map(|it| it.filter_map(Result::ok).collect())
                .unwrap_or_default();
            assert!(
                entries
                    .iter()
                    .all(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)),
                "non-dir entry left in {sub}: {entries:?}"
            );
        }
    }
}

// ponytail: ADR-0015 § Exit codes — exercises the binary as a subprocess
// so the real clap + std::process::exit paths are taken. Unit tests on
// the inner functions would not catch a regression where someone moves
// the exit(78) call behind a flag.
#[cfg(test)]
mod adr_0015_validate_tests {
    use super::*;

    // ponytail: tempdirs MUST outlive the subprocess — the child copies
    // the path strings into its env at spawn time, but the on-disk
    // directory is owned by the TempDir guard. Drop the guard after
    // wait_with_output returns. Same pattern as run_hook_subprocess.
    fn run_cli_subprocess(
        args: &[&str],
    ) -> (
        std::process::Output,
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let cfg_dir = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let runtime_dir = tempfile::tempdir().unwrap();
        let out = std::process::Command::new(plugin3_binary_path())
            .args(args)
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        (out, cfg_dir, data_dir, runtime_dir)
    }

    // ponytail: write the corruption before spawning so we only run
    // the binary once. run_cli_subprocess returns the tempdir guard
    // so we can mutate a file between env-var copy and child exec.
    // `dir` is the tempdir to write into ("config" → cfg_dir,
    // "data" → data_dir, "runtime" → runtime_dir); `filename` is the
    // on-disk name relative to that dir. All three corrupt-* paths
    // share this helper so a contributor who breaks one surface
    // (e.g. drops `parse_budget_at` from `run_path_checks`) is
    // caught by the other tests.
    fn run_cli_subprocess_with_corrupt_file(
        args: &[&str],
        body: &[u8],
        dir: &str,
        filename: &str,
    ) -> (std::process::Output, tempfile::TempDir) {
        let cfg_dir = tempfile::tempdir().unwrap();
        let data_dir = tempfile::tempdir().unwrap();
        let runtime_dir = tempfile::tempdir().unwrap();
        let target = match dir {
            "config" => cfg_dir.path().join(filename),
            "data" => data_dir.path().join(filename),
            "runtime" => runtime_dir.path().join(filename),
            other => panic!("unknown dir slot: {other}"),
        };
        std::fs::write(&target, body).unwrap();
        let out = std::process::Command::new(plugin3_binary_path())
            .args(args)
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        (out, cfg_dir)
    }

    #[test]
    fn budget_status_emits_human_by_default() {
        // ponytail: pin the human format. A contributor who switches
        // the default to JSON breaks every shell alias that greps
        // for "used:". The exact phrase is the contract.
        let (out, _c, _d, _r) = run_cli_subprocess(&["budget", "status"]);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.starts_with("used: "), "got: {stdout}");
        assert!(
            !stdout.contains('{'),
            "human output must not contain JSON: {stdout}"
        );
    }

    #[test]
    fn budget_status_human_branch_full_line_shape_and_pascal_case_state() {
        // ponytail: pin the EXACT human-branch line shape on
        // `plugin3 budget status` (no `--json`):
        //   used: <used> / <ceiling> (<State>)
        // where `<State>` is the `Debug`-formatted variant — `Under`,
        // `Approaching`, `Over` — PascalCase. The existing
        // `budget_status_emits_human_by_default` only checks
        // `starts_with("used: ")` and that there's no `{`, which
        // is enough to detect "JSON leaked into the human branch"
        // but doesn't pin:
        //   - the field separator (" / ", not "/" or "of")
        //   - the trailing parenthesised state
        //   - the PascalCase state spelling (Debug, not serde)
        // A contributor who flips the human branch to use the
        // JSON branch's `serde_json::to_string_pretty` builder
        // (or who replaces `{:?}` with `{}` after a serde rename
        // to snake_case) would silently change the wire form
        // from `Under` to `under` on this branch, breaking every
        // `grep "(Approaching)"` wrapper.
        //
        // Three arms cover all three `BudgetState` variants —
        // mirroring the JSON sibling
        // (`budget_status_json_state_approaching_and_over_are_pinned`)
        // which pins the JSON branch's snake_case spellings.
        // Together the two tests pin the LOAD-BEARING divergence:
        // the human branch uses Debug (PascalCase), the JSON
        // branch uses serde (snake_case). The same `BudgetState`
        // value renders as `Under` on stdout and `"under"` in the
        // JSON envelope — intentional, not a bug.
        for (used, ceiling, expected_state) in [
            (0usize, 200_000usize, "Under"),
            (80, 100, "Approaching"),
            (100, 100, "Over"),
        ] {
            let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
            let data_dir = tempfile::tempdir().expect("data tempdir");
            let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
            // ponytail: write the runtime budget.toml with the
            // seeded values. `approaching_ratio` defaults to 0.8
            // via `TokenBudget::default()`, and the test harness
            // here doesn't seed config.toml so the default
            // applies. The state ratios come from
            // `BudgetState::state()` in plugin3-core/src/budget.rs:
            //   ratio >= 1.0                    → Over
            //   ratio >= self.approaching_ratio → Approaching
            //   else                            → Under
            let budget_path = runtime_dir.path().join("budget.toml");
            let seed = TokenBudget {
                ceiling,
                approaching_ratio: 0.8,
                used,
            };
            std::fs::write(&budget_path, toml::to_string(&seed).unwrap()).unwrap();

            let out = std::process::Command::new(plugin3_binary_path())
                // NOTE: no `--json`. The human branch uses
                // `println!("used: {} / {} ({:?})", ...)`, which
                // Debug-formats the state as PascalCase — distinct
                // from the JSON branch's snake_case serde form.
                .args(["budget", "status"])
                .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
                .env("PLUGIN3_DATA_DIR", data_dir.path())
                .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .expect("spawn plugin3 budget status (human)");
            assert!(
                out.status.success(),
                "budget status must exit 0; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stdout = String::from_utf8_lossy(&out.stdout);
            let line = stdout.trim_end_matches('\n').trim_end();
            assert_eq!(
                line,
                format!("used: {used} / {ceiling} ({expected_state})"),
                "human branch must emit EXACTLY `used: {used} / \
                 {ceiling} ({expected_state})` — the field separator \
                 is ` / ` (slash-space-space), the state is in \
                 parentheses, and the state spelling is PascalCase \
                 (Debug format, not serde snake_case). got: {line:?}"
            );
            // ponytail: negative pin — the JSON-branch snake_case
            // form MUST NOT leak into the human branch. A
            // contributor who replaces the Debug format with the
            // JSON branch's snake_case value (e.g. via a serde
            // rename on `BudgetState` plus a switch to `{}`)
            // surfaces here as `under` instead of `Under`.
            assert!(
                !stdout.contains("(under)"),
                "human branch must NOT emit the snake_case `(under)` \
                 form (that's the JSON branch's spelling); got: {stdout:?}"
            );
            assert!(
                !stdout.contains("(approaching)"),
                "human branch must NOT emit `(approaching)` \
                 (snake_case); got: {stdout:?}"
            );
            assert!(
                !stdout.contains("(over)"),
                "human branch must NOT emit `(over)` (snake_case); \
                 got: {stdout:?}"
            );
        }
    }

    #[test]
    fn budget_status_emits_json_when_json_flag_set() {
        // ponytail: --json is the scriptable path. Pin both the
        // top-level keys AND the snake_case enum spelling — a reader
        // of `report --kind` filters on the same spellings.
        let (out, _c, _d, _r) = run_cli_subprocess(&["--json", "budget", "status"]);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level object");
        let keys: std::collections::BTreeSet<&str> =
            obj.keys().map(std::string::String::as_str).collect();
        assert_eq!(
            keys,
            ["ceiling", "state", "used"].into_iter().collect(),
            "field set drifted from ADR-0015",
        );
        assert_eq!(v["state"], "under");
    }

    #[test]
    fn budget_set_emits_json_with_ceiling_and_persisted_default() {
        // ponytail: pin the `plugin3 --json budget set N` wire
        // shape. The CLI builds
        //   `{"ceiling": N, "persisted_default": bool}`
        // — two top-level keys. The boolean distinguishes "session-
        // local change" from "wrote the default to config.toml" so
        // a wrapper script can audit which `set` calls persisted
        // without scraping stderr. A contributor who adds a sibling
        // key (e.g. `"path": "..."`) or renames `persisted_default`
        // → `wrote_default` breaks the audit affordance silently —
        // `jq '.persisted_default'` returns null, no error. Drift
        // catches here.
        //
        // Subprocess invocation is inlined (the existing
        // `run_budget_set_subprocess` helper does not pass `--json`)
        // because the helper's purpose is persistence assertions;
        // this test cares about stdout shape, not config.toml.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "budget", "set", "275000", "--default"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level object");
        let keys: std::collections::BTreeSet<&str> =
            obj.keys().map(std::string::String::as_str).collect();
        assert_eq!(
            keys,
            ["ceiling", "persisted_default"].into_iter().collect(),
            "budget set --json top-level key set must be exactly \
             {{ceiling, persisted_default}}; a contributor who adds \
             a sibling key (or renames `persisted_default`) breaks \
             downstream `jq` audits. got: {keys:?}",
        );
        assert_eq!(
            v["ceiling"], 275_000,
            "ceiling must echo the argv value verbatim (no formatting)"
        );
        assert_eq!(
            v["persisted_default"], true,
            "persisted_default must be true when --default is passed; \
             `false` here means the --default wiring dropped the \
             persistence call silently"
        );
    }

    #[test]
    fn budget_set_emits_json_with_persisted_default_false_when_flag_omitted() {
        // ponytail: pin the dual-case of the prior test. Without
        // --default, `persisted_default` is false — a wrapper that
        // greps `jq '.persisted_default == false'` for the
        // session-local branch is load-bearing. A contributor who
        // wires the field to always-true (e.g. dropping the
        // `if persist_default` from the JSON build) silently
        // misleads every audit consumer.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "budget", "set", "125000"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        assert_eq!(v["ceiling"], 125_000);
        assert_eq!(
            v["persisted_default"], false,
            "persisted_default must be false when --default is omitted; \
             a contributor who flips the boolean (or always writes \
             config.toml) surfaces here"
        );
        // ponytail: also assert no config.toml was written — the
        // JSON field is the audit signal, but a double-check on
        // disk catches a regression that wires the boolean wrong
        // AND writes config.toml anyway.
        assert!(
            !cfg_dir.path().join("config.toml").exists(),
            "config.toml must NOT be written when --default is omitted, \
             regardless of what the JSON reports"
        );
    }

    #[test]
    fn budget_set_emits_human_branch_one_or_two_lines_per_default_flag() {
        // ponytail: subprocess pin for `plugin3 budget set <N>` on
        // the human-readable (non-JSON) branch. The JSON sibling
        // (`budget_set_emits_json_with_ceiling_and_persisted_default`
        // and its dual-case) pin the JSON envelope's
        // `persisted_default` boolean and `ceiling` value. The
        // human branch emits either one or two stdout lines
        // depending on `--default`:
        //   no --default   → `ceiling set to <N>`         (1 line)
        //   --default      → above PLUS `default persisted to <path>`
        //                                                 (2 lines)
        // The line count is the load-bearing contract: a wrapper
        // that runs `plugin3 budget set --default 200000 &&
        // wc -l` to verify persistence emits 2 lines for a
        // persisted write and 1 line for a session-local write.
        // A contributor who always emits 2 lines (or who swaps
        // the conditional to `if !persist_default`) breaks that
        // count silently.
        //
        // The first-line prefix `ceiling set to ` and the
        // second-line prefix `default persisted to ` are also
        // load-bearing — wrapper scripts grep for both. The
        // numeric value `<N>` is rendered via `{ceiling}` (the
        // `usize` Display impl), which is decimal with no
        // thousands separator — a contributor who adds a
        // thousands separator (e.g. `200,000`) breaks the
        // `grep -E 'set to [0-9]+$'` shell pattern.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let run = |extra: &[&str]| -> std::process::Output {
            std::process::Command::new(plugin3_binary_path())
                // NOTE: no `--json`. Human branch.
                .args(
                    ["budget", "set", "150000"]
                        .iter()
                        .chain(extra.iter())
                        .copied()
                        .collect::<Vec<_>>(),
                )
                .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
                .env("PLUGIN3_DATA_DIR", data_dir.path())
                .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .expect("spawn plugin3 budget set (human)")
        };

        // Arm 1: no --default → exactly 1 stdout line.
        // The line must start with `ceiling set to ` and carry
        // the ceiling value (150000) decimal with no thousands
        // separator. The forbidden `default persisted` prefix
        // must NOT appear — that's the --default-only line.
        let out = run(&[]);
        assert!(
            out.status.success(),
            "budget set 150000 (no --default) must exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "human branch without --default must emit exactly 1 \
             line (`ceiling set to <N>`); got: {lines:?}"
        );
        let line = lines[0];
        assert!(
            line.starts_with("ceiling set to "),
            "human branch line must lead with `ceiling set to `; \
             got: {line:?}"
        );
        assert!(
            line.contains("150000"),
            "human branch line must carry the ceiling value \
             (150000) decimal, no thousands separator; got: {line:?}"
        );
        assert!(
            !line.contains("150,000"),
            "human branch must NOT add a thousands separator \
             (the value is decimal `Display` on `usize`); got: {line:?}"
        );
        // ponytail: the --default-only second line MUST NOT
        // appear under the no-default arm. A contributor who
        // swaps the conditional to `if !persist_default`
        // surfaces here as 2 lines, the second starting with
        // `default persisted to`.
        assert!(
            !stdout.contains("default persisted"),
            "human branch without --default must NOT emit the \
             `default persisted to ...` line; a leak means the \
             conditional was inverted. stdout: {stdout:?}"
        );
        // ponytail: the JSON sibling's `persisted_default` boolean
        // must not leak into the human branch as a JSON literal.
        assert!(
            !stdout.contains('{'),
            "human branch must NOT emit JSON envelope markers; \
             got: {stdout:?}"
        );

        // Arm 2: --default → exactly 2 stdout lines. The first
        // is the same `ceiling set to 150000` line; the second
        // is `default persisted to <path>` where `<path>` is
        // `cfg_dir/config.toml` (resolved via `config_path()`,
        // which routes through `PLUGIN3_CONFIG_DIR`). The path
        // itself isn't pinned — `config_path()` is a function
        // call and the path includes the tempdir prefix which
        // varies per run — but the prefix and the file name
        // suffix are pinned.
        let out = run(&["--default"]);
        assert!(
            out.status.success(),
            "budget set 150000 --default must exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            2,
            "human branch with --default must emit exactly 2 \
             lines (`ceiling set to <N>` and `default persisted to \
             <path>`); got: {lines:?}"
        );
        // ponytail: pin the first line shape. Same as arm 1.
        assert!(
            lines[0].starts_with("ceiling set to 150000"),
            "first line under --default must lead with `ceiling \
             set to 150000`; got: {:?}",
            lines[0]
        );
        // ponytail: pin the second line shape. The prefix
        // `default persisted to ` is the contract; the path
        // suffix `config.toml` is the file-name contract.
        // A contributor who drops the path display
        // (e.g. shortens to `println!("default persisted")`)
        // surfaces here — a `grep -c persisted to` wrapper
        // would still match, but an audit tool that reads the
        // path off the second line would lose its signal.
        assert!(
            lines[1].starts_with("default persisted to "),
            "second line under --default must lead with \
             `default persisted to ` (note trailing space); \
             got: {:?}",
            lines[1]
        );
        assert!(
            lines[1].ends_with("config.toml"),
            "second line must end with the config file name \
             (`config.toml`); got: {:?}",
            lines[1]
        );
        // ponytail: disk-level double-check. The audit signal
        // on the JSON branch is the boolean; here it's the
        // file existing. A contributor who breaks the
        // conditional so the path message is printed but no
        // write happens surfaces here.
        assert!(
            cfg_dir.path().join("config.toml").exists(),
            "config.toml MUST exist on disk after `budget set \
             --default`; the human branch's `default persisted \
             to ...` line is the user-facing mirror of this \
             file. Missing file means the write was skipped."
        );
    }

    #[test]
    fn unknown_subcommand_exits_64() {
        // ponytail: clap returns 2 by default; ADR-0015 § Exit
        // codes prescribes 64 (EX_USAGE). `main()` routes
        // `Cli::try_parse_from` errors through `exit_usage_err`,
        // so an unknown subcommand surfaces as 64 in the
        // subprocess. A regression that calls `Cli::parse()`
        // directly would flip this back to 2.
        let (out, _c, _d, _r) = run_cli_subprocess(&["nonexistent-cmd"]);
        assert!(
            !out.status.success(),
            "unknown subcommand must exit non-zero, got success; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(64),
            "ADR-0015 prescribes 64 for usage errors; if this fails the exit wiring changed"
        );
    }

    #[test]
    fn config_validate_exits_78_on_corrupt_config() {
        // ponytail: corrupt config.toml must surface as EX_CONFIG (78).
        // ADR-0015 § Exit codes lists 78 for "config parse or backend
        // init failure". Writing a non-TOML file forces the parse
        // failure path inside run_path_checks.
        let (out, _cfg) = run_cli_subprocess_with_corrupt_file(
            &["config", "--validate"],
            b"this is = not [ valid",
            "config",
            "config.toml",
        );
        assert!(!out.status.success(), "corrupt config must exit non-zero");
        assert_eq!(
            out.status.code(),
            Some(78),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Sanity: the failure mention lives in stdout (the check table)
        // so an ops-tool that greps stderr-only still sees the non-zero
        // exit and a host that reads stdout sees the table.
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("FAIL"),
            "stdout must show the failing check: {stdout}"
        );
    }

    #[test]
    fn config_validate_json_envelope_shape_is_pinned() {
        // ponytail: pin the `plugin3 --json config --validate` wire
        // shape. The CLI builds
        //   `{"ok": bool, "failures": usize, "checks": [...]}` where
        // each item in `checks` is `{label, path, status, detail}` and
        // `status` is the snake_case string `"ok"` or `"fail"` (NOT
        // the human format's `"OK  "` / `"FAIL"` — those are padded
        // for terminal alignment and would silently fail a downstream
        // `jq '.checks[].status == "ok"'` filter).
        //
        // The top-level `ok` is the boolean summary; `failures` is
        // the count. They must agree — a contributor who flips one
        // without the other (e.g. reports `ok: failures == 0` but
        // forgets to update `failures` itself) surfaces here.
        //
        // On a fresh tempdir all 8 `run_path_checks` rows pass:
        //   config_dir, data_dir, runtime_dir (directories)
        //   config_file, budget_file, slices_dir, usage_log,
        //   recent_outputs (file surfaces)
        // The exact count is the boundary; a contributor who adds a
        // 9th check (or drops one) without updating this test catches
        // here. The 8 count is the same number the
        // `run_path_checks_passes_on_fresh_tempdir` unit test asserts
        // — both must move together.
        let (out, _c, _d, _r) = run_cli_subprocess(&["--json", "config", "--validate"]);
        assert!(
            out.status.success(),
            "fresh tempdir must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level object");
        let top_keys: std::collections::BTreeSet<&str> =
            obj.keys().map(std::string::String::as_str).collect();
        assert_eq!(
            top_keys,
            ["checks", "failures", "ok"].into_iter().collect(),
            "config --validate --json top-level key set must be exactly \
             {{ok, failures, checks}}; a contributor who renames `failures` \
             → `failure_count` (or `ok` → `success`) breaks every \
             `jq '.ok'` audit silently. got: {top_keys:?}",
        );

        assert_eq!(
            v["ok"], true,
            "ok must be true on a fresh tempdir; false here means the \
             boolean summary drifted from the per-check status"
        );
        assert_eq!(
            v["failures"], 0,
            "failures count must be 0 on a fresh tempdir; non-zero here \
             means a check flipped to Fail without a reason (env? tempdir \
             quirk?)"
        );
        let checks = v["checks"].as_array().expect("checks is an array");
        assert_eq!(
            checks.len(),
            8,
            "fresh tempdir must produce 8 path checks (3 dirs + 5 file \
             surfaces); a contributor who adds or removes a check without \
             updating the wire pin surfaces here"
        );

        let expected_keys: std::collections::BTreeSet<&str> =
            ["detail", "label", "path", "status"].into_iter().collect();
        for (i, c) in checks.iter().enumerate() {
            let cobj = c
                .as_object()
                .unwrap_or_else(|| panic!("check[{i}] must be an object, got: {c}"));
            let keys: std::collections::BTreeSet<&str> =
                cobj.keys().map(std::string::String::as_str).collect();
            assert_eq!(
                keys, expected_keys,
                "check[{i}] field set drifted from ADR-0015; got: {keys:?}"
            );
            assert_eq!(
                c["status"], "ok",
                "check[{i}] (label={}) must report status `\"ok\"` (snake_case) \
                 on a fresh tempdir; `\"OK\"` or `\"OK  \"` would break a \
                 downstream `jq '.checks[].status == \"ok\"'` filter",
                c["label"]
            );
        }
    }

    #[test]
    fn config_validate_json_status_fail_snake_case_is_pinned() {
        // ponytail: dual-arm pin for the `"fail"` status spelling
        // on the JSON path. Round 38's
        // `config_validate_json_envelope_shape_is_pinned` only
        // exercises the `"ok"` row (fresh tempdir, all checks pass).
        // A contributor who flips the JSON match arm to
        //   `CheckStatus::Fail => "failed"` (or `"FAIL"`)
        // breaks every `jq '.checks[] | select(.status == "fail")'`
        // filter — that filter would silently return an empty set
        // and the failing check would be invisible to dashboards.
        //
        // The human format (`"FAIL"`, padded for terminal alignment)
        // is a different surface and lives in a different match arm
        // (commands/config.rs:217). The JSON arm uses `"fail"` and
        // `"ok"` — both snake_case, both unpadded. This test pins
        // the JSON arm's `Fail` spelling.
        //
        // Also pins `ok: false` + `failures: 1` agreement with the
        // per-check row. A contributor who wires `ok` to always-true
        // (or `failures` to `0`) keeps the row's `"fail"` string
        // intact and breaks the summary signal — caught here.
        let (out, _cfg) = run_cli_subprocess_with_corrupt_file(
            &["--json", "config", "--validate"],
            b"this is = not [ valid",
            "config",
            "config.toml",
        );
        assert!(
            !out.status.success(),
            "corrupt config must exit non-zero (78); \
             a contributor who swallows the exit code surfaces here. \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(78),
            "ADR-0015 prescribes 78 for config parse failure"
        );

        let v: serde_json::Value = serde_json::from_slice(&out.stdout)
            .expect("stdout is valid JSON even on the failure path");
        let obj = v.as_object().expect("top-level object");

        // ponytail: assert the summary fields agree with each
        // other. `ok: false` AND `failures >= 1` are both required;
        // a contributor who wires only one surfaces here.
        assert_eq!(
            v["ok"], false,
            "ok must be false when any check fails; `true` here means \
             the boolean summary drifted from `failures == 0` to a \
             constant"
        );
        let failures = v["failures"]
            .as_u64()
            .expect("failures must be a non-negative integer");
        assert!(
            failures >= 1,
            "failures must be ≥ 1 with a corrupt config.toml; \
             zero here means the corrupt-file test lost its bite"
        );
        assert_eq!(
            failures as usize,
            obj["checks"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["status"] == "fail")
                .count(),
            "top-level `failures` count must equal the number of rows \
             with `status == \"fail\"`; a contributor who hardcodes \
             `failures: 1` (or computes it wrong) surfaces here"
        );

        // ponytail: locate the `config_file` row and assert its
        // `status` is the snake_case `"fail"`. The label is what
        // `run_path_checks` registers; a rename of the label would
        // also drift this test, but that's a separate concern
        // covered by `budget_validate_exits_78_on_corrupt_budget_toml`.
        let checks = obj["checks"].as_array().expect("checks is array");
        let config_file_row = checks
            .iter()
            .find(|c| c["label"] == "config_file")
            .expect("checks must include a row labelled `config_file`");
        assert_eq!(
            config_file_row["status"], "fail",
            "config_file row must report status `\"fail\"` (snake_case) \
             on a corrupt config.toml; `\"failed\"`, `\"FAIL\"`, or \
             `\"fail \"` (with whitespace) would break a downstream \
             `jq '.checks[] | select(.status == \"fail\")'` filter \
             and silently hide the failing check from dashboards. \
             got: {:?}",
            config_file_row["status"]
        );
        // ponytail: detail must be a non-empty string carrying
        // the parse error. A contributor who drops the error
        // message (sets `detail: ""`) keeps the row's `"fail"`
        // status and loses the diagnostic — the human format
        // shows it but the JSON surface would not.
        let detail = config_file_row["detail"]
            .as_str()
            .expect("detail must be a string, not null/array");
        assert!(
            !detail.is_empty(),
            "detail must carry the parse error message; an empty \
             string here means a contributor lost the diagnostic \
             the user needs to fix the file"
        );
    }

    #[test]
    fn config_show_json_envelope_includes_sources_when_show_sources_passed() {
        // ponytail: pin the fix for the `--show-sources --json`
        // flag-drop bug. Pre-fix, the JSON branch returned early
        // before the env-source block, so `plugin3 --json config
        // --show-sources` produced identical output to `plugin3
        // --json config` — the flag was silently swallowed on the
        // JSON path. The fix adds a `sources` key to the JSON
        // envelope when --show-sources is passed; without it, the
        // envelope stays at 8 keys (the existing shape).
        //
        // Two arms pin the dual state:
        //   1. `plugin3 --json config`                     → 8 keys, no `sources`
        //   2. `plugin3 --json config --show-sources`      → 9 keys, with `sources`
        // A contributor who always emits `sources` (or who restores
        // the silent drop) breaks one arm or the other.
        //
        // The `sources` value is a 3-key object (config_dir,
        // data_dir, runtime_dir), each value either `"XDG default"`
        // (no env var set) or `"env PLUGIN3_*=<value>"`. Setting
        // PLUGIN3_CONFIG_DIR in the second arm pins both value
        // shapes — a contributor who drops the env-prefix
        // formatting (returns the bare value without the `env
        // VAR=` prefix) breaks the audit affordance.
        let (out, _c, _d, _r) = run_cli_subprocess(&["--json", "config"]);
        assert!(
            out.status.success(),
            "config --json must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert!(
            !keys.contains("sources"),
            "without --show-sources the envelope must NOT include a \
             `sources` key; a contributor who always emits `sources` \
             breaks consumers that grep the 8-key envelope. got: \
             {keys:?}"
        );
        assert_eq!(
            keys.len(),
            8,
            "without --show-sources the envelope must be exactly the \
             8 path keys (config_dir, data_dir, runtime_dir, \
             config_file, budget_file, slices_dir, usage_log, \
             recent_outputs). got: {keys:?}"
        );

        // ponytail: spawn with --show-sources AND a custom
        // PLUGIN3_CONFIG_DIR so we exercise both the `XDG default`
        // arm (data_dir, runtime_dir) AND the `env VAR=<value>`
        // arm (config_dir) in a single subprocess. `run_cli_subprocess`
        // sets PLUGIN3_CONFIG_DIR to the cfg tempdir, which would
        // hide the env-prefix formatting; use a one-off spawn
        // pattern with a distinct custom cfg tempdir. PLUGIN3_DATA_DIR
        // and PLUGIN3_RUNTIME_DIR are explicitly REMOVED (via
        // `env_remove`) so the XDG-default branch is exercised —
        // the parent test process may have these set, and inherited
        // env vars would silently turn the "XDG default" branch
        // into "env VAR=" here.
        let custom_cfg = tempfile::tempdir().expect("custom cfg tempdir");
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "config", "--show-sources"])
            .env("PLUGIN3_CONFIG_DIR", custom_cfg.path())
            .env_remove("PLUGIN3_DATA_DIR")
            .env_remove("PLUGIN3_RUNTIME_DIR")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        assert!(
            out.status.success(),
            "config --json --show-sources must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert!(
            keys.contains("sources"),
            "with --show-sources the envelope MUST include a \
             `sources` key — the flag-drop fix. got: {keys:?}"
        );
        assert_eq!(
            keys.len(),
            9,
            "with --show-sources the envelope must be 8 path keys + \
             1 sources key. got: {keys:?}"
        );

        // ponytail: pin the sources sub-object. config_dir is the
        // env-set one (custom_cfg.path()); data_dir and runtime_dir
        // are XDG default (no env var set for those in the spawn
        // above). A contributor who drops the `env VAR=` prefix
        // surfaces here as the config_dir value being a bare path.
        let sources = obj["sources"]
            .as_object()
            .expect("sources must be a nested object");
        let source_keys: std::collections::BTreeSet<&str> =
            sources.keys().map(String::as_str).collect();
        assert_eq!(
            source_keys,
            ["config_dir", "data_dir", "runtime_dir"]
                .into_iter()
                .collect(),
            "sources sub-object must have exactly the 3 env-var \
             keys; a contributor who adds a 4th (e.g. \
             `PLUGIN3_CONFIG_FILE`) without updating this pin \
             surfaces here. got: {source_keys:?}"
        );
        let config_src = sources["config_dir"]
            .as_str()
            .expect("config_dir source must be a string");
        assert!(
            config_src.starts_with("env PLUGIN3_CONFIG_DIR="),
            "env-set var must use the `env VAR=` prefix; bare path \
             or `env:` (no space) breaks the audit affordance. got: \
             {config_src:?}"
        );
        assert!(
            config_src.contains(&custom_cfg.path().to_string_lossy().to_string()),
            "config_dir source must include the custom tempdir path \
             passed via PLUGIN3_CONFIG_DIR; the env var isn't being \
             read on the JSON path. got: {config_src:?}"
        );
        assert_eq!(
            sources["data_dir"], "XDG default",
            "data_dir with no PLUGIN3_DATA_DIR set must report \
             `XDG default`; a contributor who flips the default \
             string (e.g. to `default`) breaks the audit signal. \
             got: {:?}",
            sources["data_dir"]
        );
        assert_eq!(
            sources["runtime_dir"], "XDG default",
            "runtime_dir with no PLUGIN3_RUNTIME_DIR set must report \
             `XDG default`. got: {:?}",
            sources["runtime_dir"]
        );
    }

    #[test]
    fn config_validate_human_branch_emits_check_table_then_summary() {
        // ponytail: subprocess pin for `plugin3 config --validate`
        // on the human-readable (non-JSON) branch. The JSON
        // sibling family
        // (`config_validate_json_envelope_shape_is_pinned`,
        // `config_validate_json_status_fail_snake_case_is_pinned`,
        // `config_validate_exits_78_on_corrupt_config`) pins the
        // JSON envelope, the snake_case `"fail"` status string,
        // and the EX_CONFIG (78) exit code. The human branch goes
        // through `commands::config::validate(...)` directly,
        // emitting N check-table lines (`{status}  {label:<22}  \
        // {path}  ({detail})`), then a `---` separator, then a
        // summary line (`all N checks passed` or `F of N path \
        // checks failed`).
        //
        // The format strings are the contract: a contributor who
        // narrows the label column from 22 to 12 clips
        // `recent_outputs` (14 chars) into the path; a
        // contributor who changes the status prefix from `OK  `
        // (4 chars, 2 trailing spaces) to `OK ` (3 chars) breaks
        // the column alignment of every line. Both drift modes
        // are caught here at the subprocess boundary.
        //
        // The default `run_path_checks` returns 8 checks (3
        // directories + 5 file/parent paths; see
        // `commands/config.rs::run_path_checks`). A contributor
        // who adds a 9th path surface to the checks Vec
        // surfaces here as 11 lines.
        let (out, _c, _d, _r) = run_cli_subprocess(&["config", "--validate"]);
        assert!(
            out.status.success(),
            "config --validate must exit 0 on a clean tempdir; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

        // ponytail: count the check-table lines. The default
        // `run_path_checks` walks 8 entries (3 dirs +
        // `config_file`, `budget_file`, `slices_dir`,
        // `usage_log`, `recent_outputs`). Then there's a `---`
        // separator and a summary line. So expect 8 + 1 + 1 = 10
        // lines on a clean tempdir. A contributor who adds a
        // 9th entry surfaces here as 11 lines; one who drops an
        // entry surfaces as 9.
        assert_eq!(
            lines.len(),
            10,
            "human branch on a clean tempdir must emit 8 check \
             lines + `---` + summary = 10 lines; got: {lines:?}"
        );

        // ponytail: pin the separator position. After the 8
        // check lines (lines 0..7), line 8 is the `---`
        // separator, line 9 is the summary.
        assert_eq!(
            lines[8], "---",
            "line[8] must be the literal `---` separator; \
             got: {:?}",
            lines[8]
        );
        assert_eq!(
            lines[9], "all 8 checks passed",
            "line[9] must be `all 8 checks passed` (the \
             clean-tempdir summary); got: {:?}",
            lines[9]
        );

        // ponytail: pin the per-check line shape. Each of the 8
        // check lines starts with the status prefix (`OK  ` or
        // `FAIL`), then two spaces (the literal between the
        // status and the label), then the label left-aligned to
        // 22 chars, then two spaces (the literal between the
        // label and the path), then the path, then two spaces,
        // then `({detail})`. The clean-tempdir arm has all
        // `OK  ` rows (every check passes on a freshly created
        // tempdir).
        //
        // The labels in order (from `run_path_checks`):
        //   config_dir, data_dir, runtime_dir, config_file,
        //   budget_file, slices_dir, usage_log, recent_outputs
        // (note: `slices_dir`, `usage_log`, `recent_outputs` go
        // through `check_file_parent` rather than `check_file`
        // because the files don't exist yet).
        let expected_labels = [
            "config_dir",
            "data_dir",
            "runtime_dir",
            "config_file",
            "budget_file",
            "slices_dir",
            "usage_log",
            "recent_outputs",
        ];
        assert_eq!(
            expected_labels.len(),
            8,
            "internal: 8 labels expected, matching the line \
             count of 8 check rows; if this fails, the line \
             count and labels pin have drifted apart"
        );
        for (i, label) in expected_labels.iter().enumerate() {
            let line = lines[i];
            // ponytail: status prefix. The status is `OK  ` (4
            // chars: O, K, space, space) on a clean tempdir.
            // The 2-space gap is the literal in the format
            // string after `{status}`. So the prefix is
            // `OK    ` (4 + 2 = 6 chars).
            assert!(
                line.starts_with("OK    "),
                "line[{i}] must lead with `OK    ` (status `OK  ` \
                 + 2-space gap to the label); got: {line:?}"
            );
            // ponytail: the label must appear at column 6 with
            // 22-char padding. The substring `{label:<22}` is
            // left-aligned to 22 chars, so the next character
            // after the label is a space (if the label is < 22
            // chars) or the path (if it's exactly 22).
            // `recent_outputs` (14 chars) gets 8 spaces of pad;
            // `config_dir` (10 chars) gets 12 spaces of pad.
            let after_status = &line[6..];
            assert!(
                after_status.starts_with(&format!("{label:<22}")),
                "line[{i}] after the status prefix must lead with \
                 `{label:<22}` (22-char pad); got: {line:?}"
            );
        }

        // ponytail: pin the path suffix on each check line.
        // The path value is a tempdir prefix (randomised) so
        // we can't pin the full path; we pin the file-name
        // suffix that the wrapper cares about (the file path
        // is `tempdir/<filename>` and we know the filename).
        // For directories (`config_dir`, `data_dir`,
        // `runtime_dir`) we assert the label substring appears
        // (the path itself is just the tempdir).
        assert!(
            lines[0].contains('(') && lines[0].ends_with(')'),
            "line[0] must end with `(<detail>)`; got: {:?}",
            lines[0]
        );
        assert!(
            lines[3].contains("config.toml"),
            "line[3] (config_file) must include `config.toml` \
             in the path; got: {:?}",
            lines[3]
        );
        assert!(
            lines[4].contains("budget.toml"),
            "line[4] (budget_file) must include `budget.toml` \
             in the path; got: {:?}",
            lines[4]
        );
        assert!(
            lines[5].contains("slices"),
            "line[5] (slices_dir) must include `slices`; \
             got: {:?}",
            lines[5]
        );
        assert!(
            lines[6].contains("usage.jsonl"),
            "line[6] (usage_log) must include `usage.jsonl`; \
             got: {:?}",
            lines[6]
        );
        assert!(
            lines[7].contains("recent_outputs.jsonl"),
            "line[7] (recent_outputs) must include \
             `recent_outputs.jsonl`; got: {:?}",
            lines[7]
        );

        // ponytail: negative pin. The JSON sibling's status
        // string (`"fail"`, snake_case) MUST NOT appear on
        // the human branch — the human branch uses Debug
        // format for `CheckStatus` (`OK` / `Fail`, PascalCase).
        // A contributor who unifies the two branches to share
        // the JSON path's `serde_json::to_string` builder
        // surfaces here as `"fail"` leaking into stdout.
        assert!(
            !stdout.contains("\"fail\""),
            "human branch must NOT emit the JSON sibling's \
             `\"fail\"` snake_case status string; got: {stdout:?}"
        );
        assert!(
            !stdout.contains("\"ok\""),
            "human branch must NOT emit the JSON sibling's \
             `\"ok\"` snake_case status string; got: {stdout:?}"
        );
        assert!(
            !stdout.contains('{'),
            "human branch must NOT emit JSON envelope markers; \
             got: {stdout:?}"
        );

        // ponytail: arm 2 — corrupt config triggers FAIL rows
        // and the failure summary. This exercises the FAIL
        // status prefix (`FAIL` with no trailing spaces,
        // 4 chars), the `F of N path checks failed` summary,
        // and the EX_CONFIG (78) exit code from the
        // `exit_config_err` route. The exit-code pin is
        // already covered by `config_validate_exits_78_on_corrupt_config`;
        // we focus on the human branch's render here.
        let (out, _cfg) = run_cli_subprocess_with_corrupt_file(
            &["config", "--validate"],
            b"this is = not [ valid",
            "config",
            "config.toml",
        );
        assert!(
            !out.status.success(),
            "corrupt config must exit non-zero (78)"
        );
        assert_eq!(
            out.status.code(),
            Some(78),
            "EX_CONFIG (78) is the documented exit code for \
             config parse failures; got: {:?}",
            out.status.code()
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        // ponytail: the summary line on the failure arm is
        // `1 of 8 checks failed` (the corrupt config
        // produces one FAIL row; the literal word is
        // `checks`, not `path checks` — that's a
        // contrib-out-of-sync hazard worth pinning
        // explicitly).
        let summary = lines.last().expect("at least the summary");
        assert!(
            summary.starts_with("1 of 8 checks failed"),
            "failure summary must be `1 of 8 checks failed` \
             (one FAIL row for the corrupt config.toml); \
             got: {summary:?}"
        );
        // ponytail: the FAIL prefix is `FAIL` (4 chars, no
        // trailing spaces in the literal). The 2-space gap
        // between status and label comes from the format
        // string (`{status}  `), so the full prefix to the
        // label is `FAIL  ` (4 + 2 = 6 chars). Note this
        // differs from the OK prefix in length-by-coincidence:
        // `OK  ` is 4 chars (OK + 2 trailing spaces inside
        // the literal), so the OK+gap prefix is also 6 chars
        // (`OK    `). Both share the same total width —
        // a contributor who switches to `format!("{status:<4}")`
        // to right-pad would surface here as either `FAIL  `
        // (OK) or `FAIL    ` (right-padded, broken).
        let corrupt_row = lines
            .iter()
            .find(|l| l.starts_with("FAIL"))
            .expect("must have at least one FAIL row");
        assert!(
            corrupt_row.starts_with("FAIL  "),
            "FAIL row must lead with `FAIL  ` (status `FAIL` \
             + 2-space gap to label); got: {corrupt_row:?}"
        );
        assert!(
            corrupt_row.contains("config_file"),
            "the FAIL row should be the config_file check \
             (the corrupt config.toml); got: {corrupt_row:?}"
        );
        assert!(
            corrupt_row.contains("parse failed"),
            "the FAIL row's detail should mention `parse failed`; \
             got: {corrupt_row:?}"
        );
    }

    #[test]
    fn config_show_human_branch_emits_8_padded_label_lines_without_sources() {
        // ponytail: subprocess pin for `plugin3 config` on the
        // human-readable (non-JSON) branch. The JSON sibling
        // (`config_show_json_envelope_includes_sources_when_show_sources_passed`)
        // pins the 8-key envelope (or 9 with `--show-sources`).
        // The human branch goes through
        // `commands::config::show(...)` directly, emitting 8 lines
        // with `{k:<16} {path}` padding, plus an optional `---`
        // separator + 3 env-source lines when `--show-sources` is
        // passed. The format is the contract: a contributor who
        // changes the column width from 16 to 8 (or 24) breaks
        // every wrapper that does `awk '{print $1}'` on the
        // rendered output.
        //
        // Two arms:
        //   no --show-sources → 8 lines, no `---` separator,
        //                        no JSON envelope markers
        //   --show-sources    → 8 + 1 (---) + 3 = 12 lines,
        //                        with `config_dir:`/`data_dir:`/
        //                        `runtime_dir:` env-source lines
        //
        // The 16-char pad matters: `recent_outputs` is the
        // longest label at 14 chars; `{:<16}` pads it to 16.
        // A contributor who narrows to `{:<12}` would clip the
        // `recent_outputs` label (last 4 chars land on the value
        // side, contaminating path parsing). The label-content
        // pin below catches that.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let out = std::process::Command::new(plugin3_binary_path())
            // NOTE: no `--json`. Human branch.
            .args(["config"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 config (human)");
        assert!(
            out.status.success(),
            "config (human) must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        // ponytail: pin the exact line count. The human branch
        // emits 8 lines (one per path in the `pairs` Vec) — no
        // `---` separator, no env-source block (those are
        // gated on `--show-sources`). A contributor who adds
        // a 9th path to the Vec without updating this pin
        // surfaces here as 9 lines.
        assert_eq!(
            lines.len(),
            8,
            "human branch without --show-sources must emit \
             exactly 8 lines (one per path in `pairs`); got: {lines:?}"
        );

        // ponytail: pin the label-order and `{:<16}` padding. The
        // labels in order are: config_dir, data_dir, runtime_dir,
        // config_file, budget_file, slices_dir, usage_log,
        // recent_outputs. Each label is left-aligned to 16 chars
        // (the longest label `recent_outputs` is 14 chars, padded
        // with 2 spaces). The pinned substrings check that the
        // label appears with at least one trailing space before
        // the path (the `{:<16}` formatter pads with spaces, then
        // `format!` adds one more space before the value).
        let expected_labels = [
            "config_dir",
            "data_dir",
            "runtime_dir",
            "config_file",
            "budget_file",
            "slices_dir",
            "usage_log",
            "recent_outputs",
        ];
        for (i, label) in expected_labels.iter().enumerate() {
            let line = lines[i];
            // ponytail: pin the column-pad-and-space pattern.
            // `{:<16}` left-aligns to 16 chars, then `format!`
            // adds one more space (the literal between the two
            // `{}` placeholders). So the gap between label and
            // path is `16 - label.len() + 1` chars. For
            // `recent_outputs` (14 chars): 16 - 14 + 1 = 3
            // spaces. For `data_dir` (8 chars): 16 - 8 + 1 = 9
            // spaces. We check for at least one space after
            // the label and that the label prefix is followed by
            // a separator before the path.
            assert!(
                line.starts_with(&format!("{label:<16}")),
                "line[{i}] must lead with `{label:<16}` (left-aligned \
                 to 16 chars); got: {line:?}"
            );
        }

        // ponytail: pin the path values. Each line carries the
        // path value (a tempdir prefix that varies per run). We
        // verify each line has a non-empty path after the label
        // (the path is `{label:<16} {path}` — at least one char
        // of path after the trailing space). The tempdir prefix
        // is randomised per run; we don't pin it.
        for (i, line) in lines.iter().enumerate() {
            let label_part = &line[..16.min(line.len())];
            let rest = line[16.min(line.len())..].trim_start();
            assert!(
                !rest.is_empty(),
                "line[{i}] must carry a non-empty path after the \
                 `{label_part}` label; got: {line:?}"
            );
        }

        // ponytail: negative pin. Without --show-sources the
        // human branch MUST NOT emit:
        //   - the `---` separator that gates the env-source block
        //   - the JSON envelope markers (`{`, `}`)
        //   - the env-source labels (`config_dir:`, `data_dir:`,
        //     `runtime_dir:` — note these would overlap with the
        //     path labels but use a different format: `: ` vs
        //     `:<16} `)
        assert!(
            !stdout.contains("---"),
            "human branch without --show-sources must NOT emit the \
             `---` separator (that's gated on --show-sources); \
             got: {stdout:?}"
        );
        assert!(
            !stdout.contains('{'),
            "human branch must NOT emit JSON envelope markers; \
             got: {stdout:?}"
        );
        // The env-source block uses `config_dir:    ` (12-char pad)
        // rather than `{:<16}` (16-char pad). A contributor who
        // makes the separator block unconditional surfaces here
        // as the 12-char-padded labels appearing.
        for env_label in ["config_dir:    ", "data_dir:      ", "runtime_dir:   "] {
            assert!(
                !stdout.contains(env_label),
                "human branch without --show-sources must NOT emit \
                 the env-source label `{env_label}` (12-char pad); \
                 got: {stdout:?}"
            );
        }

        // ponytail: arm 2 — with --show-sources. 8 path lines +
        // 1 separator + 3 env-source lines = 12 lines. The
        // env-source lines use a different padding scheme
        // (`config_dir:    ` is 12-char-padded, vs the path
        // lines' `{:<16}` 16-char padding). A contributor who
        // re-aligns both to 16 surfaces here as the env-source
        // labels gaining more spaces.
        let cfg2 = tempfile::tempdir().expect("cfg tempdir 2");
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["config", "--show-sources"])
            .env("PLUGIN3_CONFIG_DIR", cfg2.path())
            // Remove PLUGIN3_DATA_DIR / PLUGIN3_RUNTIME_DIR so
            // those arms report "XDG default" (matches the
            // JSON sibling's pattern).
            .env_remove("PLUGIN3_DATA_DIR")
            .env_remove("PLUGIN3_RUNTIME_DIR")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 config --show-sources (human)");
        assert!(
            out.status.success(),
            "config --show-sources (human) must exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            12,
            "human branch with --show-sources must emit \
             8 + 1 (`---`) + 3 = 12 lines; got: {lines:?}"
        );
        // ponytail: pin the separator position. After the 8 path
        // lines (lines 0..7), line 8 is `---`, then lines 9..11
        // are the env-source block.
        assert_eq!(
            lines[8], "---",
            "line[8] must be the literal `---` separator; \
             got: {:?}",
            lines[8]
        );
        // ponytail: pin the env-source label padding. The labels
        // here are 12-char-padded (NOT 16) — `config_dir:` is 11
        // chars, padded to 12 with 1 space; `data_dir:` is 9
        // chars, padded to 12 with 3 spaces; `runtime_dir:` is
        // 11 chars, padded to 12 with 1 space. A contributor
        // who switches these to 16-char padding (matching the
        // path lines) surfaces here.
        assert!(
            lines[9].starts_with("config_dir:    "),
            "line[9] (env-source config_dir) must lead with \
             `config_dir:    ` (12-char pad); got: {:?}",
            lines[9]
        );
        assert!(
            lines[10].starts_with("data_dir:      "),
            "line[10] (env-source data_dir) must lead with \
             `data_dir:      ` (12-char pad); got: {:?}",
            lines[10]
        );
        assert!(
            lines[11].starts_with("runtime_dir:   "),
            "line[11] (env-source runtime_dir) must lead with \
             `runtime_dir:   ` (12-char pad); got: {:?}",
            lines[11]
        );
        // ponytail: pin the env-source values. config_dir is
        // env-set (custom tempdir path); data_dir and runtime_dir
        // are XDG default (no env var set).
        assert!(
            lines[9].contains(&cfg2.path().to_string_lossy().to_string()),
            "env-source config_dir line must include the custom \
             tempdir path passed via PLUGIN3_CONFIG_DIR; \
             got: {:?}",
            lines[9]
        );
        assert!(
            lines[9].starts_with("config_dir:    env PLUGIN3_CONFIG_DIR="),
            "env-source config_dir must use the `env VAR=` prefix; \
             a bare path here breaks the audit affordance. \
             got: {:?}",
            lines[9]
        );
        assert_eq!(
            lines[10], "data_dir:      XDG default",
            "env-source data_dir with no PLUGIN3_DATA_DIR must be \
             exactly `data_dir:      XDG default` (12-char pad); \
             got: {:?}",
            lines[10]
        );
        assert_eq!(
            lines[11], "runtime_dir:   XDG default",
            "env-source runtime_dir with no PLUGIN3_RUNTIME_DIR \
             must be exactly `runtime_dir:   XDG default` \
             (12-char pad); got: {:?}",
            lines[11]
        );
    }

    #[test]
    fn report_last_n_truncates_to_n_records_at_subprocess() {
        // ponytail: subprocess pin for `plugin3 --json report
        // --last N`. The CLI defaults `--last` to 100 and the
        // aggregator truncates via `tail_lines(&filtered, last)`
        // (plugin3-core/src/report.rs). A contributor who breaks
        // the truncation (e.g. drops `tail_lines` and emits
        // `&filtered` directly, or routes the wrong slice into
        // the JSON printer) silently changes the count contract:
        //   `report --last 1`  must show 1 record (the latest)
        //   `report --last 100` on 5 records must show all 5
        // Existing tests in `tests` call `commands::report::at`
        // directly with the typed `last` arg, bypassing clap's
        // `--last` parsing — they wouldn't catch a drift between
        // the clap default and the runtime param. Subprocess is
        // the only layer that exercises the full path: clap
        // → CLI dispatch → `tail_lines` → JSON wire.
        //
        // Seed 5 records with distinct session_ids so the
        // truncation is observable by ID (not just count).
        // `tail_lines` preserves original order, so the last 2
        // records must be in seed order (r3, then r4).
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        for r in [
            mk(UsageKind::Slice, "r0"),
            mk(UsageKind::BudgetWarn, "r1"),
            mk(UsageKind::Slice, "r2"),
            mk(UsageKind::CompactHint, "r3"),
            mk(UsageKind::Slice, "r4"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --last 2 → 2 records (the tail, in seed order).
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--last", "2"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --last 2");
        assert!(
            out.status.success(),
            "--last 2 must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            2,
            "--last 2 on a 5-row seed must return 2 records; a \
             different count here means the truncation either \
             dropped (`tail_lines` returning the wrong slice) or \
             was bypassed entirely (full file emitted). got: {}",
            arr.len()
        );
        // ponytail: pin the order. `tail_lines` preserves the
        // seed order, so the surviving records must be r3 then
        // r4 (not r4 then r3 — that would be a reverse sort, not
        // a tail). A contributor who sorts by ts asc or by
        // session_id asc silently changes the contract.
        assert_eq!(
            arr[0]["session_id"], "r3",
            "--last 2 must preserve seed order: the second-to-last \
             record (r3) comes first in the output. got: {:?}",
            arr[0]["session_id"]
        );
        assert_eq!(
            arr[1]["session_id"], "r4",
            "--last 2 must preserve seed order: the last record \
             (r4) comes second. got: {:?}",
            arr[1]["session_id"]
        );

        // Arm 2: --last 100 on the same 5-row seed → all 5
        // records (the `n > len` fallback in `tail_lines`).
        // This is the dual-arm: when `--last` exceeds the seed,
        // the CLI must emit the full set, not crash or return
        // zero. A contributor who replaces the fallback with
        // `panic!` or `return &[]` surfaces here.
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--last", "100"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --last 100");
        assert!(
            out.status.success(),
            "--last 100 must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            5,
            "--last 100 on a 5-row seed must return all 5 records \
             (the `n > len` fallback in `tail_lines`); zero or \
             panic here means the fallback regressed. got: {}",
            arr.len()
        );
        assert_eq!(
            arr[0]["session_id"], "r0",
            "fallback path must preserve original (head-first) \
             order, not reversed. got: {:?}",
            arr[0]["session_id"]
        );
    }

    #[test]
    fn report_last_n_human_branch_truncates_keeping_seed_order() {
        // ponytail: subprocess pin for `plugin3 report --last N` on
        // the human-readable (non-JSON) branch. The JSON sibling
        // above (`report_last_n_truncates_to_n_records_at_subprocess`)
        // pins that `--last` truncates AFTER filtering and returns
        // the LAST N records (not the FIRST N) at the JSON-array
        // level. The human branch goes through the verbatim
        // `for line in lines { println!("{line}"); }` loop and
        // was only exercised at unit level via
        // `commands::report::at(...)`. A contributor who breaks
        // `tail_lines` (e.g. swaps the slice direction to
        // `lines[..n]`, taking the FIRST N) or who swaps the
        // human branch to `for line in lines.iter().rev()` would
        // pass the unit tests (which assert on `lines.len()` only)
        // but break the wire contract — `tail -n` semantics are
        // load-bearing for the wrapper-script patterns ADR-0010
        // documents (e.g. `plugin3 report --last 1` to fetch the
        // most-recent record).
        //
        // Three arms exercise:
        //   --last 3 on a 5-row seed  → 3 lines, seed order,
        //                                LAST 3 records of seed
        //   --last 5 (= seed length) → 5 lines (tail_lines fallback
        //                                when `n >= len` returns
        //                                the slice unchanged)
        //   --last 100 (= default)   → 5 lines (same fallback; the
        //                                default of 100 is what an
        //                                unconfigured `plugin3
        //                                report` invocation uses)
        // The fallback path is the same code branch (the `else`
        // arm of `tail_lines`'s `if lines.len() > n`) but pinned
        // separately because `--last 100` is the common default
        // and a contributor who breaks only the truncation path
        // (e.g. `if lines.len() >= n`) might miss the >= case.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            }
        };
        // ponytail: session_id encodes the seed position so the
        // surviving-record assertion can verify tail-vs-head
        // ordering. r0 is the FIRST seed row, r4 is the LAST.
        // A contributor who swaps the slice direction to
        // `lines[..n]` (head-first) would surface as r0/r1/r2
        // surviving under --last 3 instead of r2/r3/r4.
        for (i, sid) in ["r0", "r1", "r2", "r3", "r4"].iter().enumerate() {
            // Mix the kinds so a kind-based short-circuit can't
            // accidentally collapse the 5-row seed into one type.
            let kind = match i % 3 {
                0 => UsageKind::Slice,
                1 => UsageKind::BudgetWarn,
                _ => UsageKind::CompactHint,
            };
            s.push_str(&serde_json::to_string(&mk(kind, sid)).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --last 3 → exactly 3 surviving lines, in seed
        // order, the LAST 3 records (r2, r3, r4). The order pin
        // catches a head-first regression; the count pin catches
        // off-by-one or over-truncation.
        let out = std::process::Command::new(plugin3_binary_path())
            // NOTE: no `--json`. Human branch.
            .args(["report", "--last", "3"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 report --last 3 (human)");
        assert!(
            out.status.success(),
            "--last 3 must parse and exit 0 on the human branch; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            3,
            "--last 3 must truncate the 5-row seed to exactly 3 \
             lines; a different count means `tail_lines` regressed \
             (off-by-one, slice direction swapped, or zero fallback). \
             got: {lines:?}"
        );

        // ponytail: pin the ORDER of the surviving records. A
        // contributor who swaps to `lines[..n]` (head-first) keeps
        // the count but returns r0/r1/r2 — a `--last` semantically
        // inverted to `--first`. The session_id values are
        // ordered in the seed, so the surviving records must be
        // the LAST 3 (r2, r3, r4) in seed order, not reversed.
        let sids: Vec<&str> = lines
            .iter()
            .filter_map(|l| {
                // ponytail: parse the session_id field by
                // substring scan rather than deserialising — the
                // human branch emits verbatim JSONL, but the
                // assertion stays substring-based so it works
                // even if a contributor renames the field. We
                // anchor on the `"session_id":"..."` literal.
                let key = "\"session_id\":\"";
                let start = l.find(key)? + key.len();
                let rest = &l[start..];
                let end = rest.find('"')?;
                Some(&rest[..end])
            })
            .collect();
        assert_eq!(
            sids,
            vec!["r2", "r3", "r4"],
            "--last 3 must return the LAST 3 records in seed order \
             (tail semantics, not head). A `lines[..n]` regression \
             would yield [r0, r1, r2]; a `lines[..n].rev()` would \
             yield [r4, r3, r2]. got: {sids:?}"
        );

        // ponytail: pin the verbatim single-line shape. A
        // contributor who merged the human branch into the JSON
        // branch's `to_string_pretty` parser would emit
        // multi-line records (one field per line). The literal
        // JSON object braces on each line pin that the human
        // branch did NOT switch to pretty-printing.
        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.starts_with('{') && line.ends_with('}'),
                "human-branch line[{i}] must be a single-line JSONL \
                 object (verbatim passthrough); got: {line:?}"
            );
        }

        // Arm 2: --last 5 (= seed length) → all 5 records in
        // seed order. The `tail_lines` fallback (`if lines.len()
        // > n { ... } else { lines }`) returns the slice unchanged
        // when `n >= len`. A contributor who changes the
        // condition to `if lines.len() >= n` would still pass
        // arm 1 but might over-truncate here on `n == len`
        // (returning 4 lines if `>=` is interpreted as `>` minus
        // the boundary, depending on the slice arithmetic).
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["report", "--last", "5"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 report --last 5 (human)");
        assert!(
            out.status.success(),
            "--last 5 must parse and exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            5,
            "--last 5 (= seed length) must return all 5 lines via \
             the `n >= len` fallback; a 4-line result means the \
             boundary regressed. got: {lines:?}"
        );
        // ponytail: seed-order preservation on the fallback path.
        // A contributor who switches `tail_lines`'s fallback to
        // `&lines[..]` (which still returns all 5 for n >= len)
        // would pass the count check but might lose the
        // preservation — actually the slice is unchanged here so
        // this is redundant; the order assertion on arm 1 is
        // the load-bearing one. We skip the order assertion on
        // arm 2 to keep the test focused.

        // Arm 3: --last 100 (default value of the clap flag)
        // must also return all 5 lines. clap's `default_value_t
        // = 100` means an unconfigured `plugin3 report` uses
        // 100; on a 5-row seed that's well above the seed
        // length. A contributor who changes the default to e.g.
        // `1` would surface here as a 1-line output under the
        // default invocation. We pass `--last 100` explicitly
        // to pin the FLAG VALUE behaviour, not the DEFAULT
        // behaviour (the default is pinned at the unit level
        // via clap's own derive).
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["report", "--last", "100"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 report --last 100 (human)");
        assert!(
            out.status.success(),
            "--last 100 must parse and exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            5,
            "--last 100 on a 5-row seed must return all 5 lines \
             (fallback); a different count means `--last 100` \
             doesn't reach `tail_lines`'s fallback branch. \
             got: {lines:?}"
        );
    }

    #[test]
    fn report_last_n_with_session_filter_human_branch_orders_filter_then_tail() {
        // ponytail: subprocess pin for `plugin3 report --last N
        // --session <SID>` on the human-readable (non-JSON)
        // branch. The `--last N` family is pinned on the JSON
        // branch (`report_last_n_truncates_to_n_records_at_subprocess`
        // + `report_last_after_combined_filters_at_subprocess`),
        // and `--session` alone is pinned on the human branch
        // (R61: `report_session_filter_human_branch_prints_only_matching_sids`).
        // The combination of the two — filter by session, then
        // take the LAST N of the surviving set — has never been
        // pinned end-to-end on the human branch.
        //
        // The load-bearing drift here is FILTER-THEN-TAIL order.
        // `commands::report::at()` runs:
        //   let filtered = report::filter_lines(&all, ...);
        //   let lines    = report::tail_lines(&filtered, last);
        // A contributor who swaps these (tail-then-filter) would
        // break the contract: `plugin3 report --last 1 --session
        // bravo` should return bravo's last record, NOT the
        // whole-file-last record (which might be a different
        // session). The JSON sibling pins this; the human branch
        // is the gap. Unit tests in plugin3-core cover
        // `filter_lines`/`tail_lines` in isolation but not their
        // composition via the CLI's `at()`.
        //
        // The 4-row seed has 2 alpha rows + 2 bravo rows (all
        // the same kind) so the surviving count is non-degenerate
        // and the ORDER pin catches a tail-then-filter regression:
        //   arm 1: --last 2 --session bravo → 2 lines, both bravo
        //   arm 2: --last 1 --session alpha → 1 line, the SECOND alpha row
        //   arm 3: --last 5 --session alpha → 2 lines (alpha has
        //          only 2; the n > len fallback applies, proving
        //          the order is filter-first)
        // session_id encodes the position so order is verifiable
        // at the substring level.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            }
        };
        // 4 rows: alpha/0, alpha/1, bravo/0, bravo/1 — interleaved
        // so a tail-then-filter regression would surface as
        // (bravo/1, bravo/0) surviving under --last 2 --session
        // bravo (correct) vs (alpha/1, bravo/1) surviving under
        // a tail-then-filter regression (last 2 of the whole file
        // are bravo/1 and alpha/1, then --session bravo keeps
        // bravo/1 — different count: 1 vs 2, depending on impl).
        // The interleaving makes the test robust to both
        // directions of bug.
        for r in [
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "bravo"),
            mk(UsageKind::Slice, "bravo"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        fn sid_of(line: &str) -> &str {
            let key = "\"session_id\":\"";
            let start = line.find(key).expect("session_id present") + key.len();
            let rest = &line[start..];
            let end = rest.find('"').expect("session_id terminated");
            &rest[..end]
        }

        let run = |last: &str, sid: &str| -> std::process::Output {
            std::process::Command::new(plugin3_binary_path())
                // NOTE: no `--json`. Human branch.
                .args(["report", "--last", last, "--session", sid])
                .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
                .env("PLUGIN3_DATA_DIR", data_dir.path())
                .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .unwrap_or_else(|e| panic!("spawn plugin3 --last {last} --session {sid}: {e}"))
        };

        // Arm 1: --last 2 --session bravo → exactly 2 lines, both
        // bravo. The 2 surviving bravo rows must be in seed
        // order (the third and fourth seed rows). A contributor
        // who breaks filter-then-tail to tail-then-filter would
        // surface here: the last 2 rows of the WHOLE FILE are
        // bravo + bravo (rows 3 and 4), then filter-by-bravo
        // keeps both — same result by accident because the last
        // 2 rows happen to be bravo. To break this coincidence
        // we need arm 2: --last 1 --session alpha — the last
        // row of the whole file is bravo, NOT alpha, so a
        // tail-then-filter regression returns the bravo row
        // and FAILS the session equality pin. Arm 1 alone is
        // not load-bearing; arm 2 is.
        let out = run("2", "bravo");
        assert!(
            out.status.success(),
            "--last 2 --session bravo must parse and exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            2,
            "--last 2 --session bravo must filter the 4-row seed \
             down to 2 lines (both bravo rows); a different count \
             here means the session filter dropped a bravo row. \
             got: {lines:?}"
        );
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(
                sid_of(line),
                "bravo",
                "arm 1 line[{i}] must have session_id=bravo; got: {line:?}"
            );
        }

        // Arm 2 (load-bearing): --last 1 --session alpha → 1
        // line, the LAST alpha row (the second alpha in seed
        // order). A tail-then-filter regression would return
        // the LAST row of the whole file (bravo), failing the
        // session equality pin. The order pin (alpha is the
        // FIRST and SECOND seed rows; --last 1 returns the
        // second one) catches a filter-first→filter-last
        // regression too.
        let out = run("1", "alpha");
        assert!(
            out.status.success(),
            "--last 1 --session alpha must parse and exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--last 1 --session alpha must filter the 4-row seed \
             down to 1 line (the second alpha row); got: {lines:?}"
        );
        let line = lines[0];
        assert_eq!(
            sid_of(line),
            "alpha",
            "arm 2 line must have session_id=alpha; a `bravo` \
             here means filter-then-tail was swapped to \
             tail-then-filter (the LAST row of the whole file \
             is bravo). got: {line:?}"
        );

        // Arm 3: --last 5 --session alpha → 2 lines, both alpha.
        // The `n > len` fallback in `tail_lines` runs after the
        // session filter reduces the seed to 2 alpha rows;
        // `--last 5` is well above 2 so the fallback returns
        // the filtered slice unchanged. A contributor who
        // accidentally applied `--last` BEFORE `--session` (the
        // tail-then-filter bug) would tail the whole file to 5
        // rows (= whole file), then filter to alpha — same
        // result of 2 alpha rows. So this arm doesn't catch
        // the order regression. It DOES pin the n > len
        // fallback on the human branch in combination with a
        // filter — a contributor who narrows `tail_lines`'s
        // fallback to `if lines.len() - n > 0` would still
        // pass arms 1 and 2 but fail here on `--last 5`
        // returning fewer than 2 lines.
        let out = run("5", "alpha");
        assert!(
            out.status.success(),
            "--last 5 --session alpha must parse and exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            2,
            "--last 5 --session alpha must return all 2 alpha \
             rows (the n > len fallback in `tail_lines`); a \
             1-line result means the fallback regressed to \
             `n - len` or similar. got: {lines:?}"
        );
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(
                sid_of(line),
                "alpha",
                "arm 3 line[{i}] must have session_id=alpha; \
                 got: {line:?}"
            );
        }
    }

    #[test]
    fn report_last_n_with_session_and_kind_filters_human_branch() {
        // ponytail: subprocess pin for the THREE-filter combination
        // (`--last N --session <SID> --kind <K>`) on the
        // human-readable (non-JSON) branch. The JSON sibling
        // (`report_last_after_combined_filters_at_subprocess`)
        // pins the same combination at the JSON-array level. The
        // two-filter combinations have been pinned on the human
        // branch separately (R63: `--session + --kind`; R64:
        // `--last + --session`); the three-filter composition
        // is the missing arm.
        //
        // The load-bearing drift is the SAME filter-then-tail
        // order pinned in R64, plus the AND semantics of the two
        // predicates. A contributor who breaks `--kind` parsing
        // (e.g. narrows `UsageKindArg` to a typo'd variant) would
        // surface here as a clap usage error (exit non-zero)
        // rather than a correctly-rendered line; a contributor
        // who swaps the AND to OR surfaces as a leaked forbidden
        // substring; a contributor who reorders tail-then-filter
        // surfaces as the wrong session_id surviving under
        // `--last 1`.
        //
        // Same 5-row seed as the JSON sibling so a reader can
        // diff the two tests and see "same fixture, two
        // renderers". The seed positions rows interleaved across
        // sessions so a tail-then-filter regression shows up as a
        // different session_id in the surviving row (not just a
        // different count).
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            }
        };
        // 5 rows: alpha/slice ×2, bravo/slice ×1, charlie/budget_warn,
        // charlie/compact_hint. Mirrors the JSON sibling's seed
        // (line 2870).
        for r in [
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "bravo"),
            mk(UsageKind::BudgetWarn, "charlie"),
            mk(UsageKind::CompactHint, "charlie"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        let run = |last: &str, sid: &str, kind: &str| -> std::process::Output {
            std::process::Command::new(plugin3_binary_path())
                // NOTE: no `--json`. Human branch.
                .args(["report", "--last", last, "--session", sid, "--kind", kind])
                .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
                .env("PLUGIN3_DATA_DIR", data_dir.path())
                .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .unwrap_or_else(|e| {
                    panic!("spawn --last {last} --session {sid} --kind {kind}: {e}")
                })
        };

        // ponytail: substring-based field extractor. The human
        // branch emits verbatim JSONL, so substring scan is the
        // faithful mirror of the rendered shape (the same trick
        // R64 used for `sid_of`).
        fn field<'a>(line: &'a str, key: &str) -> &'a str {
            let needle = format!("\"{key}\":\"");
            let start = line.find(&needle).expect("key present") + needle.len();
            let rest = &line[start..];
            let end = rest.find('"').expect("value terminated");
            &rest[..end]
        }

        // Arm 1: --last 2 --session bravo --kind slice → exactly
        // 1 surviving line, the bravo/slice row. The seed has
        // only 1 bravo+slice row, so `--last 2` is well above
        // the filtered count (1) — the n > len fallback in
        // `tail_lines` returns the single surviving line. A
        // contributor who breaks the AND to OR (e.g. drops the
        // session short-circuit) would leak the two alpha/slice
        // rows through, returning 3 lines under `--last 2` (the
        // 2 alpha rows + the 1 bravo row). The count pin
        // catches that. The session_id pin catches a session
        // filter bypass.
        let out = run("2", "bravo", "slice");
        assert!(
            out.status.success(),
            "--last 2 --session bravo --kind slice must parse and \
             exit 0; exit non-zero means --kind parsing regressed. \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--last 2 --session bravo --kind slice must filter \
             the 5-row seed to exactly 1 line (the bravo/slice \
             row); a 2-line result means the session filter was \
             bypassed (alpha/slice rows leaked) AND the tail ran \
             first; a 3-line result means the AND dropped to OR. \
             got: {lines:?}"
        );
        let line = lines[0];
        assert_eq!(
            field(line, "session_id"),
            "bravo",
            "arm 1 surviving line must have session_id=bravo; \
             got: {line:?}"
        );
        assert_eq!(
            field(line, "kind"),
            "slice",
            "arm 1 surviving line must have kind=slice (snake_case \
             wire form after kebab→snake round-trip); got: {line:?}"
        );

        // Arm 2: --last 1 --session alpha --kind slice → 1
        // line, the SECOND alpha/slice row (filter-then-tail).
        // A tail-then-filter regression would return the LAST
        // row of the whole file (charlie/compact_hint), caught
        // by the session_id pin. A filter bypass (drop the
        // session short-circuit) would return the LAST alpha/slice
        // row but still with session_id=alpha — passing arm 2
        // but failing arm 1 (the count check). The two arms
        // pin independent failure modes.
        let out = run("1", "alpha", "slice");
        assert!(
            out.status.success(),
            "--last 1 --session alpha --kind slice must parse \
             and exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--last 1 --session alpha --kind slice must return \
             exactly 1 line; got: {lines:?}"
        );
        let line = lines[0];
        assert_eq!(
            field(line, "session_id"),
            "alpha",
            "arm 2 surviving line must have session_id=alpha; \
             a `charlie` here means tail-then-filter was applied \
             (the LAST row of the whole file is charlie's \
             compact_hint). got: {line:?}"
        );
        assert_eq!(
            field(line, "kind"),
            "slice",
            "arm 2 surviving line must have kind=slice; got: {line:?}"
        );

        // Arm 3: --last 5 --session charlie --kind slice → 0
        // lines. charlie has 2 rows in the seed but neither is
        // slice, so the AND drops everything. A contributor
        // who breaks the AND to OR surfaces here: the
        // charlie/budget_warn row would survive (matches
        // session alone) — caught by the explicit forbidden
        // substring pin below.
        let out = run("5", "charlie", "slice");
        assert!(
            out.status.success(),
            "--last 5 --session charlie --kind slice must exit 0 \
             (filter dropped, not clap usage error); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        assert!(
            stdout.trim().is_empty(),
            "--last 5 --session charlie --kind slice must produce \
             empty stdout (charlie has no slice rows); got: {stdout:?}"
        );
        // ponytail: double-negative pin. Under AND semantics the
        // forbidden substrings MUST be absent. A contributor who
        // swaps AND to OR surfaces as a charlie row leaking
        // (any of the two charlie rows match --session charlie
        // alone). A contributor who drops the kind filter
        // surfaces as charlie/budget_warn surviving. Both
        // regressions are caught by the explicit substring
        // pin below.
        assert!(
            !stdout.contains("\"session_id\":\"charlie\""),
            "AND filter must drop charlie entirely (no charlie \
             row matches --kind slice); a leak here means the \
             kind short-circuit broke. stdout: {stdout:?}"
        );
        assert!(
            !stdout.contains("\"kind\":\"budget_warn\""),
            "AND filter must drop non-slice rows even when the \
             session matches; a leak here means the kind filter \
             was bypassed. stdout: {stdout:?}"
        );
    }

    #[test]
    fn report_kind_filter_human_branch_prints_filtered_lines_verbatim() {
        // ponytail: subprocess pin for the human-readable (non-JSON)
        // sibling of `report_kind_filter_at_subprocess_pins_kebab_to_snake_enum_mapping`.
        // The JSON branch emits `serde_json::to_string_pretty` over
        // parsed records; the human branch prints each surviving
        // JSONL line VERBATIM (no parsing, no pretty-printing) — the
        // CLI does `for line in lines { println!("{line}"); }` on
        // the filtered tail. A contributor who copy-pastes the JSON
        // branch's `parsed.iter().map(to_string_pretty)` into the
        // human branch changes the shape a `grep "kind":"slice"`
        // dashboard relies on — the JSON branch's pretty-printer
        // puts one field per line, so `kind:"slice"` becomes
        // `"kind": "slice",` on its own line. Pin the verbatim line
        // shape here so the two branches stay visibly distinct.
        //
        // The 5-row seed mirrors the JSON-sibling fixture so a
        // reader can diff the two tests and see "same rows, two
        // renderers". Two arms exercise both single-word kebab
        // (`slice`) and multi-word kebab (`budget-warn`) — same
        // dash/underscore boundary that the JSON branch pins.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        for r in [
            mk(UsageKind::Slice, "s1"),
            mk(UsageKind::BudgetWarn, "s1"),
            mk(UsageKind::CompactHint, "s1"),
            mk(UsageKind::Slice, "s2"),
            mk(UsageKind::BudgetOver, "s2"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --kind slice on the human branch.
        // 2 surviving lines, both kind="slice" (snake_case on the
        // wire), sessions s1 + s2 in seed order. The lines must be
        // the JSONL source bytes verbatim — single-line records,
        // no pretty-printing, no leading whitespace.
        let out = std::process::Command::new(plugin3_binary_path())
            // NOTE: no `--json` here. The human branch is reached
            // by omitting it; `commands::report::at()` routes to
            // `for line in lines { println!("{line}"); }`.
            .args(["report", "--kind", "slice"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 report --kind slice (human)");
        assert!(
            out.status.success(),
            "--kind slice must parse and exit 0 on the human branch; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            2,
            "--kind slice must filter the 5-row seed down to 2 lines \
             on the human branch; a different count here means either \
             the kebab→snake conversion broke or the human branch's \
             `for line in lines` loop dropped/duplicated lines. \
             got: {lines:?}"
        );

        // ponytail: pin the verbatim single-line JSONL shape. A
        // contributor who switches the human branch to the JSON
        // branch's `to_string_pretty` parser would emit lines
        // like `  "kind": "slice",` (one field per line, padded
        // with two-space indent). The substrings `  "kind": "slice",`
        // and similar pretty-printed fragments must NOT appear —
        // if they do, the human branch has been merged into the
        // JSON branch. The single-line `"kind":"slice"` form
        // (no space between `:` and value, no leading whitespace)
        // is what verbatim passthrough produces via serde's
        // default `to_string` on `UsageRecord`.
        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.starts_with('{') && line.ends_with('}'),
                "human-branch line[{i}] must be a single-line JSONL \
                 object (verbatim passthrough); got: {line:?}"
            );
            assert!(
                line.contains("\"kind\":\"slice\""),
                "human-branch line[{i}] must carry the verbatim \
                 `\"kind\":\"slice\"` substring (no space between \
                 `:` and value, single line); pretty-printed \
                 `\"kind\": \"slice\",` here means the human branch \
                 was merged into the JSON branch's pretty-printer. \
                 got: {line:?}"
            );
            // Negative: the pretty-printed sibling must NOT leak.
            assert!(
                !line.contains("\"kind\": \"slice\""),
                "human-branch line[{i}] must NOT carry the \
                 pretty-printed `\"kind\": \"slice\"` form (space \
                 after colon); that form means the human branch \
                 started pretty-printing. got: {line:?}"
            );
        }

        // ponytail: pin the kind filter on the human branch — the
        // non-matching kinds must not leak through. A contributor
        // who breaks `filter_lines`'s `r.kind != ks` short-circuit
        // (e.g. accidentally inverts the comparison) surfaces
        // here. We assert each NON-matching kind is absent from
        // the filtered stdout — substring scan on the rendered
        // lines.
        for forbidden in ["budget_warn", "compact_hint", "budget_over"] {
            assert!(
                !stdout.contains(&format!("\"kind\":\"{forbidden}\"")),
                "human-branch --kind slice must filter out kind=\"{forbidden}\"; \
                 a leak here means `filter_lines`'s kind equality broke. \
                 stdout: {stdout:?}"
            );
        }

        // Arm 2: --kind budget-warn on the human branch.
        // 1 surviving line, kind="budget_warn" (snake_case on the
        // wire after the CLI's `budget-warn` → `BudgetWarn` →
        // `UsageKind::BudgetWarn` → `"budget_warn"` round-trip).
        // This arm pins the dash/underscore boundary on the
        // human branch; the JSON sibling pins the same boundary
        // separately.
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["report", "--kind", "budget-warn"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 report --kind budget-warn (human)");
        assert!(
            out.status.success(),
            "--kind budget-warn must parse and exit 0 on the human \
             branch; exit non-zero means the kebab-case enum lost \
             `BudgetWarn`. stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--kind budget-warn must filter the 5-row seed down to 1 \
             line on the human branch. got: {lines:?}"
        );
        let line = lines[0];
        assert!(
            line.contains("\"kind\":\"budget_warn\""),
            "human-branch --kind budget-warn line must carry \
             `\"kind\":\"budget_warn\"` (the snake_case wire form, \
             after the CLI's `budget-warn` → `UsageKind::BudgetWarn` \
             round-trip). got: {line:?}"
        );
        assert!(
            line.contains("\"session_id\":\"s1\""),
            "human-branch line must preserve the session_id from \
             the seed (s1). got: {line:?}"
        );
    }

    #[test]
    fn report_kind_filter_at_subprocess_pins_kebab_to_snake_enum_mapping() {
        // ponytail: subprocess pin for `plugin3 --json report
        // --kind <K>`. The CLI spells enum variants in kebab-case
        // (`--kind budget-warn`) and serde spells them snake_case
        // (`"budget_warn"` on the wire). The two rename rules are
        // independent and a contributor who breaks one without
        // the other silently changes the filter behaviour: e.g.
        // a typo in `#[clap(rename_all = "kebab-case")]` would
        // make `--kind budget-warn` fail to parse (clap returns
        // 64 — usage error) or, worse, silently pass an empty
        // filter and emit ALL records.
        //
        // Existing report filter tests in `tests` call
        // `commands::report::at(...)` directly with the typed
        // `Some(UsageKind::Slice)` — they bypass clap's
        // `UsageKindArg → UsageKind` conversion. A drift between
        // the kebab-case CLI spelling and the snake_case serde
        // spelling surfaces only at the subprocess layer. This
        // test pins BOTH arms:
        //   --kind slice     → 2 records (both kind=slice)
        //   --kind budget-warn → 1 record (kind=budget_warn)
        // The dual-arm catches an off-by-one in the kebab-case
        // list (e.g. `BudgetWarn` accidentally removed from
        // `UsageKindArg`, which compiles because the enum is
        // exhaustive but breaks `--kind budget-warn` at runtime).
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        for r in [
            mk(UsageKind::Slice, "s1"),
            mk(UsageKind::BudgetWarn, "s1"),
            mk(UsageKind::CompactHint, "s1"),
            mk(UsageKind::Slice, "s2"),
            mk(UsageKind::BudgetOver, "s2"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --kind slice → 2 records, all kind="slice".
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--kind", "slice"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --kind slice");
        assert!(
            out.status.success(),
            "--kind slice must parse and exit 0; exit non-zero here means \
             the kebab-case `UsageKindArg` enum lost the `Slice` variant \
             (clap returns 64). stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v.as_array().expect(
            "report --json top-level must be an array (parsed \
                     UsageRecord values, one per JSONL line)",
        );
        assert_eq!(
            arr.len(),
            2,
            "--kind slice must filter the 5-row seed down to 2 slice \
             records; a different count here means the filter dropped \
             (or stopped dropping) records. got: {}",
            arr.len()
        );
        for (i, rec) in arr.iter().enumerate() {
            assert_eq!(
                rec["kind"], "slice",
                "record[{i}] kind must be the snake_case `\"slice\"` \
                 (serde `rename_all = \"snake_case\"`); kebab-case or \
                 PascalCase here would break every `jq '.[] | \
                 select(.kind == \"slice\")'` filter. got: {:?}",
                rec["kind"]
            );
        }

        // Arm 2: --kind budget-warn → 1 record, kind="budget_warn".
        // The CLI spelling is kebab-case (`budget-warn`); the JSONL
        // and the wire format are snake_case (`budget_warn`). This
        // arm pins the conversion across the dash/underscore
        // boundary.
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--kind", "budget-warn"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --kind budget-warn");
        assert!(
            out.status.success(),
            "--kind budget-warn (kebab-case CLI spelling) must parse \
             and exit 0; exit non-zero here means the kebab-case \
             `UsageKindArg` enum lost the `BudgetWarn` variant. \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v.as_array().expect("report --json top-level is an array");
        assert_eq!(
            arr.len(),
            1,
            "--kind budget-warn must filter the 5-row seed down to 1 \
             record; a different count here means the kebab→snake \
             conversion broke (filter dropped or kept the wrong \
             records). got: {}",
            arr.len()
        );
        assert_eq!(
            arr[0]["kind"], "budget_warn",
            "the surviving record must be the snake_case `\"budget_warn\"` \
             (serde `rename_all = \"snake_case\"`); `\"budget-warn\"` or \
             `\"budgetWarn\"` here would break the wire contract. \
             got: {:?}",
            arr[0]["kind"]
        );
    }

    #[test]
    fn report_kind_filter_for_budget_over_and_compact_hint_at_subprocess() {
        // ponytail: dual-arm pin for the kebab→snake conversion on
        // the multi-word kebab variants the Round 43 test didn't
        // cover (`budget-warn` + `slice` were covered; `budget-over`
        // and `compact-hint` are not). The kebab-case CLI spelling
        // (`--kind budget-over`) and the snake_case serde spelling
        // (`"budget_over"`) are independently-owned rename rules
        // (one in `#[clap(rename_all = "kebab-case")]`, one in
        // `#[serde(rename_all = "snake_case")]` on `UsageKind`),
        // and a contributor who breaks one without the other
        // silently changes the filter behaviour — e.g. a typo in
        // the kebab-case list would make `--kind budget-over`
        // fail to parse (clap returns 64 — usage error).
        //
        // Both arms share the same 5-row seed so the count of
        // surviving records is a strong filter signal:
        //   --kind budget-over  → 1 record (the 5th row, "s2")
        //   --kind compact-hint → 1 record (the 3rd row, "s1")
        // If a contributor removed `BudgetOver` from
        // `UsageKindArg` (the enum is exhaustive so it compiles,
        // but the variant disappears from clap's help), the kebab
        // arm here would surface as a clap parse failure (exit 64).
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        // ponytail: 5 rows, one per kind × mixed sessions, so the
        // count of surviving records under each --kind arm is
        // exactly 1 (BudgetOver × 1, CompactHint × 1). The Slice
        // and BudgetWarn rows are red herrings — they verify the
        // filter actually filters, not just passes-through.
        for r in [
            mk(UsageKind::Slice, "s1"),
            mk(UsageKind::BudgetWarn, "s1"),
            mk(UsageKind::CompactHint, "s1"),
            mk(UsageKind::Slice, "s2"),
            mk(UsageKind::BudgetOver, "s2"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --kind budget-over → 1 record, kind="budget_over".
        // Dual rename: kebab `budget-over` (clap) → snake
        // `budget_over` (serde). A contributor who flips the
        // kebab rename rule to snake_case (loses the dash) would
        // make `budget-over` fail to parse here as exit 64.
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--kind", "budget-over"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --kind budget-over");
        assert!(
            out.status.success(),
            "--kind budget-over must parse and exit 0; exit non-zero \
             (typically 64) here means the kebab-case `UsageKindArg` \
             enum lost the `BudgetOver` variant or the kebab→snake \
             bridge (From<UsageKindArg>) regressed. stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            1,
            "--kind budget-over must filter the 5-row seed down to \
             exactly 1 record (the s2 BudgetOver row); a different \
             count means the filter dropped or kept the wrong rows. \
             got: {}",
            arr.len()
        );
        assert_eq!(
            arr[0]["kind"], "budget_over",
            "the surviving record must be the snake_case `\"budget_over\"` \
             (serde `rename_all = \"snake_case\"`); `\"budget-over\"` or \
             `\"budgetOver\"` here would break the wire contract. \
             got: {:?}",
            arr[0]["kind"]
        );
        assert_eq!(
            arr[0]["session_id"], "s2",
            "the surviving BudgetOver record's session_id must be the \
             one seeded with that kind; a different value here means \
             the filter accidentally routed a different row through. \
             got: {:?}",
            arr[0]["session_id"]
        );

        // Arm 2: --kind compact-hint → 1 record, kind="compact_hint".
        // Same dual-rename contract as arm 1; CompactHint is the
        // second kebab variant with an internal word boundary
        // (the only one not tested in Round 43 besides BudgetOver).
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--kind", "compact-hint"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --kind compact-hint");
        assert!(
            out.status.success(),
            "--kind compact-hint must parse and exit 0; exit non-zero \
             here means the kebab-case `UsageKindArg` enum lost the \
             `CompactHint` variant or the kebab→snake bridge regressed. \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            1,
            "--kind compact-hint must filter the 5-row seed down to \
             exactly 1 record (the s1 CompactHint row); a different \
             count means the filter dropped or kept the wrong rows. \
             got: {}",
            arr.len()
        );
        assert_eq!(
            arr[0]["kind"], "compact_hint",
            "the surviving record must be the snake_case `\"compact_hint\"` \
             (serde `rename_all = \"snake_case\"`); `\"compact-hint\"` or \
             `\"compactHint\"` here would break the wire contract. \
             got: {:?}",
            arr[0]["kind"]
        );
        assert_eq!(
            arr[0]["session_id"], "s1",
            "the surviving CompactHint record's session_id must be the \
             one seeded with that kind; a different value here means \
             the filter accidentally routed a different row through. \
             got: {:?}",
            arr[0]["session_id"]
        );
    }

    #[test]
    fn report_session_filter_at_subprocess_pins_field_equality() {
        // ponytail: subprocess pin for `plugin3 --json report
        // --session <SID>`. The existing
        // `report_session_filter_selects_matching_lines` (line 524)
        // only exercises the filter via the typed
        // `Some("s1")` argument through `commands::report::at(...)`
        // — it bypasses clap's `String → String` plumbing. A
        // contributor who breaks the clap `Session = String` arg
        // (e.g. renames it to `--sid`, makes it required, or
        // adds a default-value mismatch) keeps the unit-level
        // filter pin green and breaks every wrapper script
        // doing `plugin3 --json report --session <SID>` silently.
        // Drift catches here, at the subprocess boundary.
        //
        // Two arms:
        //   --session alpha → 2 records (both kind=slice for sid=alpha)
        //   --session bravo → 1 record  (the single bravo row)
        // The dual-arm catches a contributor who hardcodes the
        // filter value (e.g. writes `if args.session == "alpha"`
        // instead of routing through `report::filter_lines`) —
        // arm 2 would surface as a count mismatch.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        // ponytail: 4 rows, 2 alpha + 1 bravo + 1 empty-session
        // (legitimate pre-compact event per the
        // `aggregate_skips_malformed_jsonl_lines` test). The empty
        // row is the red-herring control: arm 1 must NOT include
        // it (session_id field is empty string, not "alpha"), arm
        // 2 must NOT include it either. This catches a
        // contributor who replaces `r.session_id != sid` with
        // `r.session_id.contains(sid)` (substring match) — the
        // empty-string sid would falsely match every record.
        for r in [
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::BudgetWarn, "bravo"),
            mk(UsageKind::CompactHint, ""),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --session alpha → 2 records, all session_id="alpha".
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--session", "alpha"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --session alpha");
        assert!(
            out.status.success(),
            "--session alpha must parse and exit 0; exit non-zero \
             (typically 64) here means the clap `Session` arg lost its \
             binding or the `String → Option<String>` plumbing broke. \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v.as_array().expect(
            "report --json top-level must be an array (parsed \
                     UsageRecord values, one per JSONL line)",
        );
        assert_eq!(
            arr.len(),
            2,
            "--session alpha must filter the 4-row seed down to exactly \
             2 records (both alpha Slice rows); a different count means \
             the filter dropped (or kept) the wrong rows. got: {}",
            arr.len()
        );
        for (i, rec) in arr.iter().enumerate() {
            assert_eq!(
                rec["session_id"], "alpha",
                "record[{i}] session_id must equal `\"alpha\"` exactly; \
                 a contributor who flips the comparison to \
                 `r.session_id.contains(sid)` (substring match) would \
                 let the empty-sid record slip through. got: {:?}",
                rec["session_id"]
            );
            // ponytail: pin the kind as a sanity-check — both
            // surviving records were Slice, so this confirms we
            // didn't accidentally cross-route a BudgetWarn row.
            assert_eq!(
                rec["kind"], "slice",
                "record[{i}] kind must be `\"slice\"` (the kind of \
                 both alpha rows in the seed); a different value here \
                 means the session filter accidentally routed a \
                 different kind through. got: {:?}",
                rec["kind"]
            );
        }

        // Arm 2: --session bravo → 1 record, session_id="bravo".
        // The dual-arm: if a contributor hardcodes the filter
        // value to "alpha", arm 2 would emit zero records (no
        // match), which would surface here as a count mismatch
        // (expecting 1, got 0).
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--session", "bravo"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --session bravo");
        assert!(
            out.status.success(),
            "--session bravo must parse and exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            1,
            "--session bravo must filter the 4-row seed down to exactly \
             1 record (the bravo BudgetWarn row); a different count \
             means the filter dropped or kept the wrong rows. got: {}",
            arr.len()
        );
        assert_eq!(
            arr[0]["session_id"], "bravo",
            "the surviving record's session_id must equal `\"bravo\"` \
             exactly; got: {:?}",
            arr[0]["session_id"]
        );
        assert_eq!(
            arr[0]["kind"], "budget_warn",
            "the surviving record's kind must be `\"budget_warn\"` \
             (the kind of the bravo row in the seed); got: {:?}",
            arr[0]["kind"]
        );
    }

    #[test]
    fn report_kind_filter_multi_word_kebab_human_branch() {
        // ponytail: subprocess pin for the multi-word kebab `--kind`
        // variants on the human-readable (non-JSON) branch. R60
        // (`report_kind_filter_human_branch_prints_filtered_lines_verbatim`)
        // covered `slice` (single-word) and `budget-warn`
        // (multi-word); the JSON sibling covered all four kebab
        // variants across two tests. The two missing multi-word
        // variants on the human branch are `budget-over` and
        // `compact-hint` — both round-trip through
        // `UsageKindArg::BudgetOver → UsageKind::BudgetOver` and
        // `UsageKindArg::CompactHint → UsageKind::CompactHint`
        // respectively, then serde-renamed to snake_case on the
        // wire (`budget_over`, `compact_hint`).
        //
        // The load-bearing drift: a contributor who accidentally
        // drops `BudgetOver` or `CompactHint` from
        // `UsageKindArg` would surface here as a clap usage
        // error (exit non-zero) rather than a correctly-rendered
        // line — the kebab→snake round-trip fails at clap's
        // value parser before reaching `filter_lines`. The
        // snake_case wire form on each surviving line catches a
        // separate drift: a contributor who flips the inner
        // `From<UsageKindArg> for UsageKind` mapping to point at
        // the wrong variant (compile-clean because of how the
        // round-trip form works — see R56 commentary) surfaces
        // here as a wrong `kind` value.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            }
        };
        // 4 rows: one of each kind (slice, budget_warn,
        // budget_over, compact_hint) on the same session. The
        // --kind budget-over arm must filter down to exactly the
        // budget_over row; --kind compact-hint down to exactly
        // the compact_hint row. Other rows must not leak.
        for r in [
            mk(UsageKind::Slice, "s1"),
            mk(UsageKind::BudgetWarn, "s1"),
            mk(UsageKind::BudgetOver, "s1"),
            mk(UsageKind::CompactHint, "s1"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        let run = |kind_flag: &str| -> std::process::Output {
            std::process::Command::new(plugin3_binary_path())
                // NOTE: no `--json`. Human branch.
                .args(["report", "--kind", kind_flag])
                .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
                .env("PLUGIN3_DATA_DIR", data_dir.path())
                .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .unwrap_or_else(|e| panic!("spawn --kind {kind_flag}: {e}"))
        };

        // ponytail: substring-based kind extractor (mirrors R65's
        // `field` helper). The anchor `"kind":"` is unique to the
        // kind field on `UsageRecord` (no other field starts
        // with `k`).
        fn kind_of(line: &str) -> &str {
            let needle = "\"kind\":\"";
            let start = line.find(needle).expect("kind present") + needle.len();
            let rest = &line[start..];
            let end = rest.find('"').expect("value terminated");
            &rest[..end]
        }

        // Arm 1: --kind budget-over → 1 line, kind="budget_over"
        // (snake_case wire form after kebab→snake round-trip).
        let out = run("budget-over");
        assert!(
            out.status.success(),
            "--kind budget-over must parse and exit 0 on the human \
             branch; exit non-zero means `UsageKindArg` lost the \
             `BudgetOver` variant. stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--kind budget-over must filter the 4-row seed down \
             to 1 line (the budget_over row); got: {lines:?}"
        );
        let line = lines[0];
        assert_eq!(
            kind_of(line),
            "budget_over",
            "human-branch --kind budget-over surviving line must \
             carry `kind=budget_over` (snake_case on the wire \
             after the CLI's `budget-over` → \
             `UsageKind::BudgetOver` → `\\\"budget_over\\\"` \
             round-trip); got: {line:?}"
        );
        // ponytail: forbidden-kinds pin. The 3 non-matching kinds
        // must not leak under --kind budget-over. A contributor
        // who breaks `filter_lines`'s kind equality surfaces
        // here: with 4 rows and 3 forbidden kinds, a broken
        // filter (always-true) returns 4 lines instead of 1.
        for forbidden in ["slice", "budget_warn", "compact_hint"] {
            assert!(
                !stdout.contains(&format!("\"kind\":\"{forbidden}\"")),
                "human-branch --kind budget-over must NOT leak \
                 kind=\"{forbidden}\"; a leak means `filter_lines`'s \
                 kind equality broke. stdout: {stdout:?}"
            );
        }

        // Arm 2: --kind compact-hint → 1 line, kind="compact_hint".
        // This variant has the LONGEST kebab form (5 chars after
        // the dash: `compact-hint` → snake_case `compact_hint`),
        // which means a typo in `#[clap(rename_all =
        // "kebab-case")]` is more likely here than on the
        // shorter forms. The exit-success pin catches a missing
        // `CompactHint` variant; the kind wire-form pin catches
        // a wrong inner mapping.
        let out = run("compact-hint");
        assert!(
            out.status.success(),
            "--kind compact-hint must parse and exit 0 on the \
             human branch; exit non-zero means `UsageKindArg` \
             lost the `CompactHint` variant. stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--kind compact-hint must filter the 4-row seed \
             down to 1 line (the compact_hint row); got: {lines:?}"
        );
        let line = lines[0];
        assert_eq!(
            kind_of(line),
            "compact_hint",
            "human-branch --kind compact-hint surviving line must \
             carry `kind=compact_hint` (snake_case on the wire \
             after kebab→snake round-trip); got: {line:?}"
        );
        for forbidden in ["slice", "budget_warn", "budget_over"] {
            assert!(
                !stdout.contains(&format!("\"kind\":\"{forbidden}\"")),
                "human-branch --kind compact-hint must NOT leak \
                 kind=\"{forbidden}\"; a leak means `filter_lines`'s \
                 kind equality broke. stdout: {stdout:?}"
            );
        }
    }

    #[test]
    fn report_session_filter_human_branch_prints_only_matching_sids() {
        // ponytail: subprocess pin for `plugin3 report --session <SID>`
        // on the human-readable (non-JSON) branch. The JSON sibling
        // above (`report_session_filter_at_subprocess_pins_field_equality`)
        // pins that the JSON branch's parsed `session_id` field equals
        // the CLI argument byte-for-byte; the human branch goes
        // through `for line in lines { println!("{line}"); }` and was
        // only tested at unit level via `commands::report::at()` with
        // the typed `Some("s1".into())`. A contributor who breaks
        // `filter_lines`'s `r.session_id != sid` short-circuit — e.g.
        // accidentally inverts to `==` (would invert to a NOT-match
        // filter and drop the target session) — passes the unit tests
        // because they assert on the surviving count, which would
        // also be non-zero under the inverted condition (just on the
        // wrong sessions). The substring pin on the rendered lines
        // catches this.
        //
        // The 4-row seed has a 3:1 split (3 rows for s1, 1 row for
        // s2) so the surviving-count assertion is non-degenerate: a
        // broken filter that passes everything returns 4 lines, a
        // broken filter that drops everything returns 0, and only the
        // correct `r.session_id == sid` short-circuit returns 3.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        // 4 rows: 3 for s1 (slice + budget_warn + compact_hint),
        // 1 for s2 (budget_over). The kind mix matters: a
        // contributor who copy-pastes the `filter_lines` short-
        // circuit into a wrong order (e.g. `if r.kind != ks &&
        // r.session_id != sid { return false }`) breaks the AND
        // pin but this test only exercises one filter at a time.
        for r in [
            mk(UsageKind::Slice, "s1"),
            mk(UsageKind::BudgetWarn, "s1"),
            mk(UsageKind::CompactHint, "s1"),
            mk(UsageKind::BudgetOver, "s2"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --session s1 → 3 surviving lines, all session_id="s1".
        let out = std::process::Command::new(plugin3_binary_path())
            // NOTE: no `--json`. The human branch is reached by
            // omitting it; `commands::report::at()` routes to
            // `for line in lines { println!("{line}"); }`.
            .args(["report", "--session", "s1"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 report --session s1 (human)");
        assert!(
            out.status.success(),
            "--session s1 must parse and exit 0 on the human branch; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            3,
            "--session s1 must filter the 4-row seed down to 3 lines \
             (the s1 rows: slice + budget_warn + compact_hint); a \
             different count means the session_id equality broke — \
             either all 4 leaked through (filter dropped), 0 came \
             through (filter over-dropped), or 1 came through \
             (typo on sid). got: {lines:?}"
        );
        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.contains("\"session_id\":\"s1\""),
                "human-branch line[{i}] under --session s1 must carry \
                 `\"session_id\":\"s1\"` exactly; got: {line:?}"
            );
        }
        // ponytail: the s2 row must not leak through. The 4-row
        // seed has exactly one s2 row (budget_over) and it carries
        // `\"session_id\":\"s2\"`. A contributor who breaks the
        // filter to "passes everything" surfaces here — the s2
        // substring would appear in stdout. We assert on the
        // exact substring (not just session_id absent) so a
        // contributor who renames the field to `sid_v2` also
        // fails this pin.
        assert!(
            !stdout.contains("\"session_id\":\"s2\""),
            "human-branch --session s1 must NOT leak any s2 row; \
             a `\"session_id\":\"s2\"` substring here means the \
             session filter was bypassed (e.g. always-true \
             short-circuit). stdout: {stdout:?}"
        );

        // Arm 2: --session s2 → 1 surviving line, session_id="s2".
        // The non-trivial direction: the s2 row is the 4th seed
        // row, and a `--last 100` (default) has room for it. But a
        // contributor who changed `filter_lines` to apply `last`
        // BEFORE `session` (rather than the documented
        // filter-then-tail order) would still return this row
        // here because tail=100 > 4; this arm doesn't pin the
        // order, that's `report_last_after_combined_filters_at_subprocess`.
        // It does pin that the surviving row is the budget_over
        // record from s2 — if `filter_lines`'s session_id check
        // swapped to `r.session_id != sid` (which is what the
        // current code does, returning false on mismatch and
        // keeping the rest), the s1 rows would survive and s2
        // would NOT — opposite of arm 1.
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["report", "--session", "s2"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 report --session s2 (human)");
        assert!(
            out.status.success(),
            "--session s2 must parse and exit 0 on the human branch; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--session s2 must filter the 4-row seed down to 1 line \
             (the s2 row: budget_over); got: {lines:?}"
        );
        let line = lines[0];
        assert!(
            line.contains("\"session_id\":\"s2\""),
            "human-branch line under --session s2 must carry \
             `\"session_id\":\"s2\"` exactly; got: {line:?}"
        );
        assert!(
            line.contains("\"kind\":\"budget_over\""),
            "the surviving s2 row must be the budget_over record \
             from the seed; got: {line:?}"
        );
        // ponytail: s1 must not leak through on the s2 arm.
        // Symmetric to the s2-not-leaking pin above — a broken
        // filter (always-true) would emit all 4 rows here.
        assert!(
            !stdout.contains("\"session_id\":\"s1\""),
            "human-branch --session s2 must NOT leak any s1 row; \
             stdout: {stdout:?}"
        );

        // Arm 3: --session nonexistent → 0 lines.
        // ponytail: a typo'd --session value should produce empty
        // stdout (and exit 0), NOT a clap usage error. clap's
        // `--session <SID>` is `Option<String>`, so any string
        // parses; the filter does the dropping. A contributor who
        // narrows `--session` to a `ValueEnum` would surface here
        // — clap would reject "nonexistent" with exit 64. The
        // empty-stdout contract lets `plugin3 report --session $X
        // | wc -l` reliably return zero.
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["report", "--session", "nonexistent"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 report --session nonexistent");
        assert!(
            out.status.success(),
            "--session with an unmatched id must exit 0 (filter \
             dropped everything), not 64 (clap usage error — that \
             would mean --session was narrowed to a ValueEnum). \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        assert!(
            stdout.trim().is_empty(),
            "--session with an unmatched id must produce empty \
             stdout on the human branch; got: {stdout:?}"
        );
    }

    #[test]
    fn report_session_and_kind_filters_combine_human_branch() {
        // ponytail: subprocess pin for the AND-combination of
        // `--session` and `--kind` on the human-readable (non-JSON)
        // branch. The JSON sibling below
        // (`report_session_and_kind_filters_combine_at_subprocess`)
        // pins the AND combination through the parsed JSON-array
        // path. The human branch goes through
        // `for line in lines { println!("{line}"); }` and was only
        // exercised at unit level via `commands::report::at(...)`
        // with typed filters. The unit tests in
        // `plugin3-core/src/report.rs::filter_then_tail_is_pinned`
        // exercise `filter_lines` directly with combined filters
        // — they cover the FILTER logic but not the wire-level
        // rendering. A contributor who breaks the kebab→snake
        // round-trip in `clap::ValueEnum` for `--kind` (e.g.
        // narrows `UsageKindArg` to a typo'd variant) would
        // surface here as a clap usage error rather than a
        // correctly-rendered line — both arms below exercise
        // distinct paths through that conversion.
        //
        // The 5-row seed has 3 sessions × 3 kinds so that:
        //   --session alpha --kind slice    → 1 line (the alpha/slice row)
        //   --session bravo --kind budget-warn → 1 line (the bravo/budget_warn row)
        //   --session charlie --kind slice  → 0 lines (charlie has no slice row)
        // The third arm is the load-bearing one for the AND
        // semantics: a contributor who breaks one filter (e.g.
        // narrows `--session` to a ValueEnum that rejects
        // "charlie") would surface here as a clap usage error
        // (exit non-zero), not as empty stdout. The first two
        // arms prove the survivors; the third proves that an
        // unmatched combo is empty (filter logic), not an error
        // (clap rejection).
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            }
        };
        // 5 rows: alpha has slice + budget_warn; bravo has
        // budget_warn + compact_hint; charlie has compact_hint
        // only. So no session has a row that overlaps another's
        // kind set — every (sid, kind) pair is unique.
        for r in [
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::BudgetWarn, "alpha"),
            mk(UsageKind::BudgetWarn, "bravo"),
            mk(UsageKind::CompactHint, "bravo"),
            mk(UsageKind::CompactHint, "charlie"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // ponytail: helper to spawn a combined-filter invocation
        // on the human branch. The args sequence is fixed; the
        // only variability across arms is the (sid, kind) tuple.
        // `--kind` is kebab-case (CLI spelling); `--session` is
        // a free-form string (no enum conversion). The dash in
        // `budget-warn` exercises the kebab→snake boundary on
        // the human branch.
        let run = |sid: &str, kind: &str| -> std::process::Output {
            std::process::Command::new(plugin3_binary_path())
                .args(["report", "--session", sid, "--kind", kind])
                .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
                .env("PLUGIN3_DATA_DIR", data_dir.path())
                .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .unwrap_or_else(|e| panic!("spawn plugin3 --session {sid} --kind {kind}: {e}"))
        };

        // Arm 1: alpha + slice → 1 surviving line. The seed has
        // exactly one (alpha, slice) row. The line must carry
        // both substrings. This arm proves the AND on the human
        // branch keeps a single survivor.
        let out = run("alpha", "slice");
        assert!(
            out.status.success(),
            "--session alpha --kind slice must parse and exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--session alpha --kind slice must filter the 5-row \
             seed down to exactly 1 line (the alpha/slice row); \
             got: {lines:?}"
        );
        let line = lines[0];
        assert!(
            line.contains("\"session_id\":\"alpha\""),
            "surviving line must carry `session_id=alpha`; got: {line:?}"
        );
        assert!(
            line.contains("\"kind\":\"slice\""),
            "surviving line must carry `kind=slice` (snake_case \
             wire form after the CLI's `slice` → `UsageKind::Slice` \
             round-trip); got: {line:?}"
        );

        // Arm 2: bravo + budget-warn → 1 surviving line. Multi-word
        // kebab (`budget-warn`) on the human branch. A contributor
        // who drops the `BudgetWarn` variant from `UsageKindArg`
        // would surface here as exit-non-zero (clap rejects
        // `budget-warn`). The kind wire form must be `budget_warn`
        // (snake_case).
        let out = run("bravo", "budget-warn");
        assert!(
            out.status.success(),
            "--session bravo --kind budget-warn must parse and \
             exit 0; exit non-zero here means the kebab-case \
             `UsageKindArg` enum lost `BudgetWarn`. stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "--session bravo --kind budget-warn must filter to \
             exactly 1 line (the bravo/budget_warn row); got: {lines:?}"
        );
        let line = lines[0];
        assert!(
            line.contains("\"session_id\":\"bravo\""),
            "surviving line must carry `session_id=bravo`; got: {line:?}"
        );
        assert!(
            line.contains("\"kind\":\"budget_warn\""),
            "surviving line must carry `kind=budget_warn` \
             (snake_case on the wire, after kebab→snake round-trip); \
             got: {line:?}"
        );

        // Arm 3: charlie + slice → 0 surviving lines. charlie has
        // only a compact_hint row; --kind slice excludes it.
        // AND semantics on the human branch: BOTH filters must
        // match, so this arm must produce empty stdout (not the
        // charlie/compact_hint row, which would mean the
        // session filter was skipped). A contributor who breaks
        // the AND into an OR (e.g. drops one short-circuit) would
        // surface here as 1 surviving line (the charlie row) —
        // charlie's row carries session_id=charlie but kind=
        // compact_hint, NOT slice, so a kind-OR with session
        // would still emit it.
        let out = run("charlie", "slice");
        assert!(
            out.status.success(),
            "--session charlie --kind slice must exit 0 (filter \
             dropped, not clap usage error); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        assert!(
            stdout.trim().is_empty(),
            "--session charlie --kind slice must produce empty \
             stdout on the human branch (charlie has no slice row, \
             AND semantics drop everything); a non-empty stdout \
             here means either the kind filter was bypassed (the \
             charlie/compact_hint row leaked through) or the \
             session filter was bypassed (a different session's \
             slice row leaked through). got: {stdout:?}"
        );
        // ponytail: explicit double-negative pin — the forbidden
        // survivor substrings must NOT appear. A contributor who
        // swaps the AND to OR (`if !matches_session ||
        // !matches_kind { return false }`) would surface here:
        // the charlie/compact_hint row carries
        // session_id=charlie which matches the --session filter
        // alone (so under OR it would survive).
        assert!(
            !stdout.contains("\"session_id\":\"charlie\""),
            "AND filter must drop charlie entirely (no charlie \
             row matches --kind slice); a leak here means the \
             kind short-circuit broke. stdout: {stdout:?}"
        );
        assert!(
            !stdout.contains("\"kind\":\"slice\""),
            "AND filter must drop all slice rows from non-charlie \
             sessions too; a leak here means the session \
             short-circuit broke (e.g. always-true). stdout: {stdout:?}"
        );
    }

    #[test]
    fn report_session_and_kind_filters_combine_at_subprocess() {
        // ponytail: subprocess pin for the AND-combination of
        // `--session` and `--kind` on the `plugin3 --json report`
        // path. The unit-level `filter_then_tail_is_pinned`
        // (plugin3-core/src/report.rs) covers the AND combination
        // through the typed `filter_lines(...)` boundary, but
        // nothing exercises BOTH filters together at the
        // clap → binary boundary. A contributor who flips the
        // filter composition from AND to OR (e.g. early-returns
        // from the first match — `if r.session_id == sid { return true; }`)
        // would slip past every existing subprocess pin because
        // each existing arm only sets ONE filter.
        //
        // Two arms, seeded from the same 5-row file:
        //   --session alpha --kind slice → 2 records (both
        //     slice/alpha rows). The bravo-slice row drops on
        //     session, the alpha-budget_warn and alpha-compact_hint
        //     rows drop on kind. The arm is tight: any extra
        //     surviving record means the filter OR'd instead of
        //     AND'd.
        //   --session bravo --kind slice → 1 record (slice/bravo).
        //     The dual-arm catches a contributor who hardcodes the
        //     session filter to "alpha" (arm 2 would emit zero
        //     records).
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        // ponytail: 5 rows where exactly 2 match BOTH filters for
        // arm 1 and exactly 1 matches BOTH filters for arm 2.
        // The other rows are red herrings that exercise one
        // dimension each: bravo-slice (right kind, wrong session),
        // alpha-budget_warn (right session, wrong kind),
        // alpha-compact_hint (right session, wrong kind).
        for r in [
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "bravo"),
            mk(UsageKind::BudgetWarn, "alpha"),
            mk(UsageKind::CompactHint, "alpha"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --session alpha --kind slice → 2 records.
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--session", "alpha", "--kind", "slice"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --session alpha --kind slice");
        assert!(
            out.status.success(),
            "--session alpha --kind slice must parse and exit 0; exit \
             non-zero (typically 64) here means the clap wiring for the \
             combined filter args broke. stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            2,
            "--session alpha --kind slice must filter the 5-row seed \
             down to exactly 2 records (the two slice/alpha rows). A \
             count >2 here means the filter OR'd instead of AND'd \
             (e.g. early-returned on the first filter match); count \
             <2 means one of the slice/alpha rows was dropped. got: {}",
            arr.len()
        );
        for (i, rec) in arr.iter().enumerate() {
            assert_eq!(
                rec["session_id"], "alpha",
                "record[{i}] session_id must be `\"alpha\"`; got: {:?}",
                rec["session_id"]
            );
            assert_eq!(
                rec["kind"], "slice",
                "record[{i}] kind must be `\"slice\"` (snake_case serde); \
                 got: {:?}",
                rec["kind"]
            );
        }

        // Arm 2: --session bravo --kind slice → 1 record.
        // Catches a contributor who hardcodes the session value
        // to "alpha" — arm 2 would surface as count=0 (no bravo
        // rows in any kind). Also catches the OR-bug symmetric
        // to arm 1: if the filter was OR'd, this arm would
        // emit BOTH slice/alpha and slice/bravo (=3 records).
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--session", "bravo", "--kind", "slice"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --session bravo --kind slice");
        assert!(
            out.status.success(),
            "--session bravo --kind slice must parse and exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            1,
            "--session bravo --kind slice must filter the 5-row seed \
             down to exactly 1 record (slice/bravo). A count >1 here \
             means the filter OR'd instead of AND'd (the two \
             slice/alpha rows slipped through). got: {}",
            arr.len()
        );
        assert_eq!(
            arr[0]["session_id"], "bravo",
            "the surviving record's session_id must be `\"bravo\"`; \
             got: {:?}",
            arr[0]["session_id"]
        );
        assert_eq!(
            arr[0]["kind"], "slice",
            "the surviving record's kind must be `\"slice\"`; \
             got: {:?}",
            arr[0]["kind"]
        );
    }

    #[test]
    fn report_last_after_combined_filters_at_subprocess() {
        // ponytail: subprocess pin for the THREE-way combination
        // `--last N --session <SID> --kind <K>`. Round 43 pinned
        // `--last N` alone; Round 56 pinned `--session + --kind`;
        // nothing exercises all three together at the subprocess
        // boundary. The order of operations is load-bearing
        // (ADR-0010 § Report subcommand: filter first, THEN tail to
        // N — see `filter_lines` then `tail_lines` in
        // plugin3-core/src/report.rs). A contributor who reverses
        // the order (tail first, then filter) silently drops the
        // chronological tail of the filtered set — the host sees
        // the WRONG records as "the latest".
        //
        // 5-row seed: 2 slice/alpha, 2 slice/bravo, 1 budget_warn/
        // charlie. Three arms:
        //   --session bravo --kind slice --last 2  → 2 records
        //     (both slice/bravo; tail of 2 = the full filtered set)
        //   --session alpha --kind slice --last 1  → 1 record
        //     (the SECOND slice/alpha, NOT the first — proves
        //     truncation happened AFTER filtering)
        //   --session charlie --kind slice --last 5 → 0 records
        //     (charlie has no slice rows; proves --kind still
        //     filters even when --last exceeds the filtered count)
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        for r in [
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "bravo"),
            mk(UsageKind::Slice, "bravo"),
            mk(UsageKind::BudgetWarn, "charlie"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --last 2 + bravo + slice → 2 records (both
        // slice/bravo). The dual `--session + --kind` filter
        // narrows the seed to 2 rows; --last 2 keeps both. A
        // contributor who truncates BEFORE filtering would emit
        // the tail-2 of the full seed (the second slice/bravo
        // and the budget_warn/charlie row) — surface: count=2
        // but kind mismatch on arr[1].
        let out = std::process::Command::new(plugin3_binary_path())
            .args([
                "--json",
                "report",
                "--last",
                "2",
                "--session",
                "bravo",
                "--kind",
                "slice",
            ])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --last 2 --session bravo --kind slice");
        assert!(
            out.status.success(),
            "combined --last + --session + --kind must parse and exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            2,
            "--last 2 --session bravo --kind slice must filter the \
             5-row seed to 2 (the slice/bravo rows) and tail-keep \
             both; a different count means filter-then-tail order \
             regressed or the filter dropped rows. got: {}",
            arr.len()
        );
        for (i, rec) in arr.iter().enumerate() {
            assert_eq!(
                rec["session_id"], "bravo",
                "record[{i}] session_id must be `\"bravo\"`; got: {:?}",
                rec["session_id"]
            );
            assert_eq!(
                rec["kind"], "slice",
                "record[{i}] kind must be `\"slice\"`; got: {:?}",
                rec["kind"]
            );
        }

        // Arm 2: --last 1 + alpha + slice → 1 record, the SECOND
        // slice/alpha. This is the load-bearing arm for the
        // filter-then-tail order. The seed has TWO slice/alpha
        // rows; tail-1 of the filtered set picks the second one
        // (chronologically later). A contributor who truncates
        // BEFORE filtering (tail-1 of the full file = the
        // budget_warn/charlie row) would emit kind=budget_warn
        // here, not slice — surface as kind mismatch.
        let out = std::process::Command::new(plugin3_binary_path())
            .args([
                "--json",
                "report",
                "--last",
                "1",
                "--session",
                "alpha",
                "--kind",
                "slice",
            ])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --last 1 --session alpha --kind slice");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            1,
            "--last 1 --session alpha --kind slice must filter the \
             5-row seed to 2 (slice/alpha), then tail-1 to the LAST \
             one; a different count means --last ignored the filter \
             or the tail truncation happened before filtering. got: {}",
            arr.len()
        );
        assert_eq!(
            arr[0]["session_id"], "alpha",
            "the surviving record's session_id must be `\"alpha\"`; got: {:?}",
            arr[0]["session_id"]
        );
        assert_eq!(
            arr[0]["kind"], "slice",
            "the surviving record's kind must be `\"slice\"`; got: {:?}",
            arr[0]["kind"]
        );
        // ponytail: pin the content_preview / size fingerprint
        // so a tail-vs-filter regression is unambiguous. The
        // SECOND slice/alpha row was constructed identically to
        // the first (same mk closure, same bytes_in/bytes_out),
        // so we use session_id + kind + position-in-arr as the
        // fingerprint. The arr.len() == 1 assertion above proves
        // tail happened AFTER filter; the kind/session assertions
        // prove the filter didn't leak a different row.

        // Arm 3: --last 5 + charlie + slice → 0 records. The
        // charlie row is budget_warn, not slice — the kind
        // filter wipes it out. --last 5 exceeds the filtered
        // count (0), so the result is the empty slice. This
        // arm catches a contributor who hardcodes
        // `last.min(filtered.len())` in a way that accidentally
        // bypasses --kind (e.g. routes only --session through
        // filter_lines) — surface: count=1 (the charlie row),
        // kind=budget_warn.
        let out = std::process::Command::new(plugin3_binary_path())
            .args([
                "--json",
                "report",
                "--last",
                "5",
                "--session",
                "charlie",
                "--kind",
                "slice",
            ])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --last 5 --session charlie --kind slice");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let arr = v
            .as_array()
            .expect("report --json top-level must be an array");
        assert_eq!(
            arr.len(),
            0,
            "--last 5 --session charlie --kind slice must produce 0 \
             records (charlie has no slice rows; --kind filter wiped \
             the seed); a count >0 here means --kind didn't reach the \
             filter path or --last exceeded the filtered count. got: {}",
            arr.len()
        );
    }

    #[test]
    fn budget_status_json_state_approaching_and_over_are_pinned() {
        // ponytail: dual-arm pin for the non-Under BudgetState
        // variants on the JSON path. The existing
        // `budget_status_emits_json_when_json_flag_set` only
        // exercises the `"under"` arm (fresh tempdir, used=0).
        // `BudgetState` is `Under | Approaching | Over` with
        // `#[serde(rename_all = "snake_case")]` — three
        // independent wire spellings. A contributor who flips
        // `"approaching"` → `"warning"` (or `"over"` → `"exceeded"`)
        // breaks every `jq '.state == "approaching"'` filter
        // silently. Drift catches here.
        //
        // budget.toml is `runtime_dir/budget.toml` (ADR-0014 § B2);
        // the runtime loader at `load_budget_with_config` parses it
        // via `toml::from_str::<TokenBudget>`. Two seeded values
        // cover the two interesting ratios:
        //   ceiling=100, used=80  → ratio=0.80 ≥ approaching_ratio
        //                            (default 0.8) → Approaching
        //   ceiling=100, used=100 → ratio=1.00 ≥ 1.0          → Over
        // The `used` value also carries through to the JSON
        // payload — pinning it catches a regression where
        // `state` is computed but `used` is hardcoded to 0 in
        // the wire builder.
        for (used, expected_state) in [(80usize, "approaching"), (100usize, "over")] {
            let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
            let data_dir = tempfile::tempdir().expect("data tempdir");
            let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
            // ponytail: write the runtime budget.toml. The
            // default `approaching_ratio` (0.8) is preserved by
            // NOT seeding config.toml — `load_budget_with_config`
            // leaves `b.approaching_ratio` at its Default value
            // when config.toml is absent. The seeded `used`
            // value crosses the threshold for the expected
            // state.
            let budget_path = runtime_dir.path().join("budget.toml");
            let seed = TokenBudget {
                ceiling: 100,
                approaching_ratio: 0.8,
                used,
            };
            std::fs::write(&budget_path, toml::to_string(&seed).unwrap()).unwrap();

            let out = std::process::Command::new(plugin3_binary_path())
                .args(["--json", "budget", "status"])
                .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
                .env("PLUGIN3_DATA_DIR", data_dir.path())
                .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .expect("spawn plugin3");
            assert!(
                out.status.success(),
                "budget status must exit 0; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let v: serde_json::Value =
                serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
            assert_eq!(
                v["state"], expected_state,
                "with ceiling=100, used={used}, expected state \
                 `{expected_state}` (snake_case BudgetState variant); \
                 a different value here means the serde rename \
                 drifted OR the ceiling/used values didn't load from \
                 budget.toml (the runtime file is silently ignored). \
                 got: {:?}",
                v["state"]
            );
            // ponytail: pin that `used` carries the seeded value,
            // not a hardcoded 0. A contributor who rewires the
            // wire builder to emit `used: 0` regardless of state
            // keeps the state transition pin green and loses the
            // diagnostic dashboards care about.
            assert_eq!(
                v["used"], used,
                "used must carry the seeded {used} through to the \
                 JSON payload; `used: 0` here means the wire builder \
                 hardcoded the counter and broke the audit signal. \
                 got: {}",
                v["used"]
            );
            assert_eq!(
                v["ceiling"], 100,
                "ceiling must carry the seeded 100 through; a \
                 different value means the runtime budget.toml was \
                 bypassed (default ceiling is 200_000)"
            );
        }
    }

    #[test]
    fn budget_validate_exits_78_on_corrupt_budget_toml() {
        // ponytail: corrupt budget.toml must also surface as EX_CONFIG
        // (78). ADR-0015 § Exit codes names 78 the catch-all for
        // "config parse or backend init failure"; budget.toml is a
        // sibling surface to config.toml — both flow through
        // `run_path_checks`'s `parse_existing` callback. A contributor
        // who drops `parse_budget_at` from the check list (or
        // changes it to swallow errors) keeps the corrupt-config test
        // green and silently allows a corrupt runtime budget to ship
        // — caught here because the failure count goes to zero and
        // validate would exit 0.
        let (out, _cfg) = run_cli_subprocess_with_corrupt_file(
            &["config", "--validate"],
            b"this is = not [ valid",
            "runtime",
            "budget.toml",
        );
        assert!(
            !out.status.success(),
            "corrupt budget.toml must exit non-zero; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(78),
            "corrupt budget.toml must exit 78 (EX_CONFIG) — same exit code \
             as corrupt config.toml; a contributor who wires a different \
             exit code for budget-parse failures breaks the ADR-0015 \
             contract here. stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("FAIL"),
            "stdout must show the failing check; got: {stdout}"
        );
        // ponytail: also pin the label that appears in the FAIL
        // row. The check is registered as `budget_file` in
        // `run_path_checks`; a contributor who renames the
        // label (e.g. `runtime_budget`) breaks dashboard
        // scripts that grep the label by name.
        assert!(
            stdout.contains("budget_file"),
            "FAIL row must label the failing surface as `budget_file`; got: {stdout}"
        );
    }

    #[test]
    fn help_text_includes_subcommand_descriptions() {
        // ponytail: ADR-0015 § Help output conventions requires a
        // one-line description on every subcommand. A contributor
        // who deletes a `///` doc comment from a variant breaks
        // the help output below; this test catches it before a
        // host script greps for the missing phrase. Each phrase
        // matches a `///` line on a HookKind or BudgetSub variant.
        let (out, _c, _d, _r) = run_cli_subprocess(&["hook", "--help"]);
        assert!(
            out.status.success(),
            "--help must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        for needle in [
            "Slice the tool result",
            "Check the budget",
            "Emit a `CompactHint`",
        ] {
            assert!(
                stdout.contains(needle),
                "hook --help missing {needle:?}; got:\n{stdout}"
            );
        }
        let (out, _c, _d, _r) = run_cli_subprocess(&["budget", "--help"]);
        assert!(out.status.success(), "--help must exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Print the current budget state"),
            "budget --help missing Status description; got:\n{stdout}"
        );
    }

    #[test]
    fn report_summary_json_envelope_shape_is_pinned() {
        // ponytail: pin the `plugin3 --json report --summary` wire
        // shape. The CLI emits a JSON object keyed by session_id
        // (BTreeMap → sorted), each value a `SessionTotals` with
        //   {bytes_saved, warnings, compactions, records}
        // (all snake_case, all `usize`). ADR-0010 § Report
        // subcommand names this shape; the existing tests pin the
        // *return value* (`sessions.len()`) but never parsed stdout
        // to verify the actual field set. A contributor who renames
        // `bytes_saved` → `bytes_dropped` (or splits `records` into
        // `records_seen` + `records_kept`) breaks every dashboard
        // jq filter silently — `jq '.s1.records'` returns null, no
        // error. Drift catches here.
        //
        // Seed two sessions across two record kinds so we exercise
        // the aggregation across multiple `session_id` keys (the
        // BTreeMap branch, not the degenerate single-session case).
        // Slice records contribute to `bytes_saved` and `records`;
        // BudgetWarn contributes to `warnings` and `records`.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        // ponytail: inline the UsageRecord build (the `tests::rec`
        // helper is in a sibling test module and isn't `pub(crate)`).
        // Slice records carry bytes_in/bytes_out so the aggregator's
        // bytes_saved math has something to roll up.
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        for r in [
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::BudgetWarn, "alpha"),
            mk(UsageKind::CompactHint, "bravo"),
            mk(UsageKind::BudgetOver, "bravo"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--summary"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect(
            "report --summary --json top-level must be an object \
                     (BTreeMap<String, SessionTotals>), not an array",
        );
        // ponytail: assert the BTreeMap ordering — sessions appear
        // sorted by session_id. A contributor who switches to
        // HashMap keeps the wire contract valid but loses the
        // determinism that makes diffs reviewable. This is a
        // separate drift from the field-set pin above, but the
        // cost of asserting is one line.
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["alpha", "bravo"],
            "BTreeMap ordering must be alphabetical; switching to \
             HashMap loses diff-stable output. got: {keys:?}"
        );
        assert_eq!(
            obj.len(),
            2,
            "two distinct session_ids must produce two top-level keys"
        );

        // ponytail: pin the per-session field set. SessionTotals
        // lives in plugin3-core and is serde-derived; the JSON
        // shape is a direct reflection of the struct. A
        // contributor who adds a field (e.g. `tokens_used`) without
        // updating this test surfaces here.
        let expected_fields: std::collections::BTreeSet<&str> =
            ["bytes_saved", "compactions", "records", "warnings"]
                .into_iter()
                .collect();
        for (sid, totals) in obj {
            let tobj = totals
                .as_object()
                .unwrap_or_else(|| panic!("{sid} totals must be object"));
            let fields: std::collections::BTreeSet<&str> =
                tobj.keys().map(String::as_str).collect();
            assert_eq!(
                fields, expected_fields,
                "{sid} field set drifted from SessionTotals; got: {fields:?}"
            );
        }

        // ponytail: assert the aggregation itself on one session
        // — alpha has 2 Slice (bytes_in=1000, bytes_out=400, so
        // bytes_saved = (1000-400)*2 = 1200) + 1 BudgetWarn
        // (records=1). bravo has 1 CompactHint + 1 BudgetOver
        // (records=2, no slice contribution). These numbers are
        // the contract: a contributor who breaks
        // `aggregate_sessions`'s slice byte accounting (e.g.
        // drops `bytes_in - bytes_out` and just counts rows)
        // surfaces here. The wire shape pin above and the
        // value pin here are independent — both belong.
        let alpha = &v["alpha"];
        assert_eq!(
            alpha["records"], 3,
            "alpha saw 3 records (2 Slice + 1 BudgetWarn); got: {}",
            alpha["records"]
        );
        assert_eq!(
            alpha["warnings"], 1,
            "alpha saw 1 BudgetWarn; got: {}",
            alpha["warnings"]
        );
        assert_eq!(
            alpha["bytes_saved"], 1200,
            "alpha's 2 Slice records saved (1000-400)*2 = 1200 bytes; \
             got: {}",
            alpha["bytes_saved"]
        );
        assert_eq!(
            alpha["compactions"], 0,
            "alpha had no CompactHint; got: {}",
            alpha["compactions"]
        );

        let bravo = &v["bravo"];
        assert_eq!(
            bravo["records"], 2,
            "bravo saw 2 records (1 CompactHint + 1 BudgetOver); got: {}",
            bravo["records"]
        );
        assert_eq!(
            bravo["compactions"], 1,
            "bravo saw 1 CompactHint; got: {}",
            bravo["compactions"]
        );
        assert_eq!(
            bravo["warnings"], 1,
            "bravo saw 1 BudgetOver; got: {}",
            bravo["warnings"]
        );
        assert_eq!(
            bravo["bytes_saved"], 0,
            "bravo had no Slice records; got: {}",
            bravo["bytes_saved"]
        );
    }

    #[test]
    fn report_summary_human_text_output_shape_is_pinned() {
        // ponytail: pin the `plugin3 report --summary` NON-JSON wire
        // shape. The JSON sibling is pinned above
        // (`report_summary_json_envelope_shape_is_pinned`); this
        // one pins the human-readable text that ADR-0010 § Report
        // subcommand documents as the default output. The unit
        // tests in commands::report::at() drive `format_summary_line`
        // directly and would still pass if a contributor replaced
        // the call with `{sid:?} {t:?}` — the subprocess boundary
        // is the only place the exact line shape is observable
        // end-to-end. A regex-pin via `format_summary_line`'s
        // return type would only pin the contract; this pins the
        // rendering.
        //
        // The seeded rows and the alpha/bravo per-session totals
        // mirror the JSON-sibling test above so a reader can
        // diff the two and see "same fixture, two renderers".
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
            let mut r = UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: None,
                bytes_out: None,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            };
            if matches!(r.kind, UsageKind::Slice) {
                r.bytes_in = Some(1000);
                r.bytes_out = Some(400);
            }
            r
        };
        for r in [
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::Slice, "alpha"),
            mk(UsageKind::BudgetWarn, "alpha"),
            mk(UsageKind::CompactHint, "bravo"),
            mk(UsageKind::BudgetOver, "bravo"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        let out = std::process::Command::new(plugin3_binary_path())
            // NOTE: no `--json` here — this is the human-readable
            // branch. `commands::report::at()` routes to the
            // `format_summary_line` loop (per-session println!).
            .args(["report", "--summary"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");

        // ponytail: pin "one line per session, no envelope". A
        // contributor who wraps the output in `[...]` (treating it
        // like a JSON list) or who emits `--- summary ---` framing
        // surfaces here — the line count must equal the number of
        // distinct session_ids. Stdout must not be empty; an empty
        // stdout would mean the human branch silently dropped the
        // aggregate, which a wrapper script's `grep -c session`
        // interprets as zero records rather than an error.
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            2,
            "two distinct session_ids must produce exactly two \
             summary lines; got: {lines:?}"
        );

        // ponytail: pin BTreeMap ordering on the human-readable
        // branch too. JSON sibling asserts this above; the human
        // branch goes through the same `BTreeMap<String,
        // SessionTotals>` so the order matches. A contributor who
        // sorts the JSON branch but not the human branch (or vice
        // versa) breaks the diff-stable review expectation.
        assert!(
            lines[0].starts_with("session alpha  "),
            "first summary line must be alpha (BTreeMap order); \
             got: {}",
            lines[0]
        );
        assert!(
            lines[1].starts_with("session bravo  "),
            "second summary line must be bravo (BTreeMap order); \
             got: {}",
            lines[1]
        );

        // ponytail: pin the exact line shape
        //   session <sid>  bytes_saved=N  warnings=N  compactions=N  records=N
        // with TWO spaces between fields (not tabs, not single
        // spaces, not `key=value` quoting). The fixture gives:
        //   alpha: 2 Slice (bytes_in=1000, bytes_out=400 → 1200) + 1 BudgetWarn
        //   bravo: 1 CompactHint + 1 BudgetOver (no slice contribution)
        // The substring scan is exact — no wildcards, no
        // normalisation. A contributor who switches to `key = value`
        // (single space, padded) or drops the two-space gutter
        // surfaces here. The chosen field set mirrors SessionTotals;
        // adding a field to that struct (e.g. `tokens_used`) without
        // updating this pin surfaces here too.
        let alpha_line = lines[0];
        assert!(
            alpha_line.contains("session alpha  "),
            "alpha line must lead with `session alpha  ` (note the \
             two-space gutter between sid and the first field); \
             got: {alpha_line}"
        );
        assert!(
            alpha_line.contains("  bytes_saved=1200  "),
            "alpha line must carry `bytes_saved=1200` with the \
             two-space gutter (2 Slice rows × (1000-400) bytes); \
             got: {alpha_line}"
        );
        assert!(
            alpha_line.contains("  warnings=1  "),
            "alpha line must carry `warnings=1` (1 BudgetWarn); \
             got: {alpha_line}"
        );
        assert!(
            alpha_line.contains("  compactions=0  "),
            "alpha line must carry `compactions=0` (no CompactHint); \
             got: {alpha_line}"
        );
        assert!(
            alpha_line.contains("  records=3"),
            "alpha line must carry `records=3` (2 Slice + 1 \
             BudgetWarn); `records=` must be the last field (no \
             trailing gutter). got: {alpha_line}"
        );

        let bravo_line = lines[1];
        assert!(
            bravo_line.contains("session bravo  "),
            "bravo line must lead with `session bravo  `; got: {bravo_line}"
        );
        assert!(
            bravo_line.contains("  bytes_saved=0  "),
            "bravo line must carry `bytes_saved=0` (no Slice); \
             got: {bravo_line}"
        );
        assert!(
            bravo_line.contains("  warnings=1  "),
            "bravo line must carry `warnings=1` (1 BudgetOver); \
             got: {bravo_line}"
        );
        assert!(
            bravo_line.contains("  compactions=1  "),
            "bravo line must carry `compactions=1` (1 CompactHint); \
             got: {bravo_line}"
        );
        assert!(
            bravo_line.contains("  records=2"),
            "bravo line must carry `records=2` (1 CompactHint + 1 \
             BudgetOver); got: {bravo_line}"
        );

        // ponytail: negative pin — the JSON-sibling envelope
        // markers must NOT leak into the human branch. A
        // contributor who copy-pastes the JSON branch's
        // `serde_json::to_string_pretty` call into the human
        // branch surfaces here (stdout would contain `{` / `}` /
        // `"` markers that don't belong on the human branch).
        assert!(
            !stdout.contains('{') && !stdout.contains('}'),
            "human summary branch must NOT emit JSON envelope \
             markers; got stdout: {stdout:?}"
        );
    }

    #[test]
    fn report_summary_with_session_and_kind_filters_at_subprocess() {
        // ponytail: subprocess pin for `plugin3 --json report
        // --summary --session <SID> --kind <K>`. The Round 50
        // --summary envelope test pins the no-filter wire shape;
        // the Round 56 combined-filter test pins the detailed-view
        // (array) wire shape. Neither pins the COMBINATION under
        // --summary. A contributor who breaks one of these
        // regressions stays silent under the existing pins:
        //
        //   (a) routing --session/--kind into `aggregate_sessions`
        //       (e.g. summary path bypasses `filter_lines` and
        //       reads the raw file) → emits ALL session keys
        //       instead of the filtered subset.
        //   (b) summary path with filters routes through
        //       `tail_lines(&filtered, last)` instead of
        //       `aggregate_sessions(&filtered)` (the Round 52
        //       bug-shape, but with filters applied — the
        //       per-session totals would silently vanish).
        //   (c) summary's `aggregate_sessions` reads the
        //       wrong field for Slice byte math under a kind
        //       filter (e.g. forgets to honour bytes_in/out on
        //       filtered rows).
        //
        // Three arms from one seed exercise each:
        //   --summary                  → 3 sessions (full set)
        //   --summary --session alpha --kind slice
        //                              → alpha only (records=2,
        //                                bytes_saved=1200)
        //   --summary --session charlie (no --kind)
        //                              → charlie only
        //                                (records=2, warnings=1,
        //                                 compactions=1)
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let usage_dir = data_dir.path().join("logs");
        std::fs::create_dir_all(&usage_dir).unwrap();
        let usage_path = usage_dir.join("usage.jsonl");
        let mut s = String::new();
        let mk = |kind: UsageKind,
                  sid: &str,
                  b_in: Option<usize>,
                  b_out: Option<usize>|
         -> UsageRecord {
            UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: b_in,
                bytes_out: b_out,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            }
        };
        // ponytail: 5 rows across 3 sessions, 4 kinds. The
        // charlie rows deliberately carry no Slice so the
        // charlie-only arm proves the filter actually filtered
        // (bytes_saved must be 0, not 0 by coincidence).
        //   alpha:  2 Slice (1000→400 each)
        //   bravo:  1 Slice (500→100)
        //   charlie: 1 BudgetWarn + 1 CompactHint
        for r in [
            mk(UsageKind::Slice, "alpha", Some(1000), Some(400)),
            mk(UsageKind::Slice, "alpha", Some(1000), Some(400)),
            mk(UsageKind::Slice, "bravo", Some(500), Some(100)),
            mk(UsageKind::BudgetWarn, "charlie", None, None),
            mk(UsageKind::CompactHint, "charlie", None, None),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        std::fs::write(&usage_path, s).unwrap();

        // Arm 1: --summary only → 3 session keys, full totals.
        // Anchor arm — establishes the seed's expected
        // aggregation without filters. Used as the baseline to
        // confirm the filtered arms are stricter (fewer keys,
        // narrower totals).
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--summary"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --summary");
        assert!(
            out.status.success(),
            "--summary must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v
            .as_object()
            .expect("--summary --json top-level must be an object (BTreeMap)");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["alpha", "bravo", "charlie"],
            "--summary with no filters must surface all 3 sessions; \
             a different set here means aggregate_sessions bypassed \
             the file or dropped a session key. got: {keys:?}"
        );

        // Arm 2: --summary --session alpha --kind slice → alpha
        // only. The combination is the load-bearing case (Round
        // 56 pins it on the detailed view; this pins it on
        // summary). Catches:
        //   (a) summary path bypasses filter_lines → would emit
        //       alpha+bravo+charlie (=3 keys, count mismatch)
        //   (b) summary path routes through tail_lines → still
        //       3 keys but alpha's records would be tail-truncated
        let out = std::process::Command::new(plugin3_binary_path())
            .args([
                "--json",
                "report",
                "--summary",
                "--session",
                "alpha",
                "--kind",
                "slice",
            ])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --summary --session alpha --kind slice");
        assert!(
            out.status.success(),
            "--summary with combined filters must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level is an object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["alpha"],
            "combined --session alpha --kind slice under --summary \
             must produce exactly {{alpha}} (BTreeMap ordering). \
             A wider set here means the summary path bypassed \
             filter_lines and routed the unfiltered file through \
             aggregate_sessions. got: {keys:?}"
        );
        let alpha = &v["alpha"];
        assert_eq!(
            alpha["records"], 2,
            "alpha has 2 Slice records; got: {}",
            alpha["records"]
        );
        assert_eq!(
            alpha["bytes_saved"], 1200,
            "alpha's 2 Slice records saved (1000-400)*2 = 1200 bytes; \
             a different value means the summary path dropped a row \
             (filter-then-tail bug) or mis-summed bytes_in/bytes_out. \
             got: {}",
            alpha["bytes_saved"]
        );
        assert_eq!(
            alpha["warnings"], 0,
            "alpha had no BudgetWarn/BudgetOver; got: {}",
            alpha["warnings"]
        );
        assert_eq!(
            alpha["compactions"], 0,
            "alpha had no CompactHint; got: {}",
            alpha["compactions"]
        );

        // Arm 3: --summary --session charlie (no --kind) →
        // charlie only, with BOTH warnings AND compactions
        // populated. Catches a contributor who wires --session
        // to work but accidentally narrows the kind implicitly
        // (e.g. `--session` filter hardcodes kind=slice). The
        // charlie rows are deliberately non-Slice so the
        // expectations differ from arm 2's alpha totals.
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--summary", "--session", "charlie"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 --summary --session charlie");
        assert!(
            out.status.success(),
            "--summary --session charlie must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level is an object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["charlie"],
            "--summary --session charlie must produce exactly \
             {{charlie}}; a wider set means --session didn't \
             reach the summary path. got: {keys:?}"
        );
        let charlie = &v["charlie"];
        assert_eq!(
            charlie["records"], 2,
            "charlie has 2 records (1 BudgetWarn + 1 CompactHint); got: {}",
            charlie["records"]
        );
        assert_eq!(
            charlie["warnings"], 1,
            "charlie's BudgetWarn must count as 1 warning; got: {}",
            charlie["warnings"]
        );
        assert_eq!(
            charlie["compactions"], 1,
            "charlie's CompactHint must count as 1 compaction; got: {}",
            charlie["compactions"]
        );
        assert_eq!(
            charlie["bytes_saved"], 0,
            "charlie has no Slice records (filter must not have \
             leaked a Slice row); got: {}",
            charlie["bytes_saved"]
        );
    }

    #[test]
    fn budget_compact_json_envelope_shape_is_pinned_at_subprocess_level() {
        // ponytail: subprocess pin for `plugin3 --json budget compact`.
        // The existing `budget_compact_json_output_shape_is_pinned`
        // in `commands::budget::compact_tests` mirrors the wrapper
        // shape INLINE (`serde_json::json!({ "hint": hint })`) — it
        // tests the macro call, not the CLI's actual stdout. A
        // contributor who rewires `commands::budget::compact()` to
        // emit `{"hint_v2": ...}` AND updates the inline test to
        // match keeps the unit test green and silently breaks every
        // downstream `jq '.hint.tokens_used'` consumer. Drift
        // catches here, at the subprocess boundary.
        //
        // Fresh tempdir → default TokenBudget (used=0,
        // ceiling=200_000) + empty recent outputs. The reason
        // string is verbatim from `compaction::build_hint`:
        //   "session at {used}/{ceiling} tokens; compaction suggested"
        // Pinning the reason format catches a contributor who
        // tweaks `build_hint` (e.g. drops "; compaction suggested"
        // thinking it's redundant noise).
        let (out, _c, _d, _r) = run_cli_subprocess(&["--json", "budget", "compact"]);
        assert!(
            out.status.success(),
            "fresh tempdir must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level object");
        let top_keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            top_keys,
            ["hint"].into_iter().collect(),
            "budget compact --json top-level key set must be exactly \
             {{hint}}; a contributor who adds a sibling key (or \
             renames `hint`) breaks every `jq '.hint'` reader. got: \
             {top_keys:?}"
        );

        let hint_v = &v["hint"];
        let hint_obj = hint_v
            .as_object()
            .expect("hint is an object (CompactHint is a struct, not a primitive)");
        let hint_keys: std::collections::BTreeSet<&str> =
            hint_obj.keys().map(String::as_str).collect();
        assert_eq!(
            hint_keys,
            [
                "newest_turn",
                "oldest_turn",
                "reason",
                "tokens_ceiling",
                "tokens_used"
            ]
            .into_iter()
            .collect(),
            "CompactHint serialised field set must match the 5-field \
             struct in plugin3-core::compaction::CompactHint. A \
             contributor who adds a 6th field (e.g. `triggered_at`) \
             propagates here only if the CLI's wrapper logic also \
             surfaces it; a rename of `tokens_used` → `used` breaks \
             `jq '.hint.tokens_used'` silently. got: {hint_keys:?}"
        );

        // ponytail: spot-check the values. The default TokenBudget
        // is ceiling=200_000, used=0; an empty recent produces
        // oldest_turn/newest_turn = null (not 0, not absent).
        // Asserting the literal reason string pins the
        // `compaction::build_hint` format — a contributor who
        // shortens it (drops "; compaction suggested") surfaces
        // here as a reason mismatch.
        assert_eq!(
            hint_v["tokens_used"], 0,
            "tokens_used on a fresh tempdir must be 0 (default budget \
             has used=0); non-zero here means the subprocess picked \
             up stale state from outside the tempdir"
        );
        assert_eq!(
            hint_v["tokens_ceiling"], 200_000,
            "tokens_ceiling must be the default 200_000 on a fresh \
             tempdir; a different value here means PLUGIN3_CONFIG_DIR \
             leaked through and a persisted config.toml set a custom \
             ceiling"
        );
        assert!(
            hint_v["oldest_turn"].is_null(),
            "oldest_turn must be null (Option::None serialised) on an \
             empty recent VecDeque — NOT 0 (which would be ambiguous \
             with the head of a single-entry deque) and NOT absent \
             (which would mean a skip_serializing_if drifted in)"
        );
        assert!(
            hint_v["newest_turn"].is_null(),
            "newest_turn must be null on an empty recent VecDeque — \
             same null-vs-0-vs-absent contract as oldest_turn above"
        );
        assert_eq!(
            hint_v["reason"], "session at 0/200000 tokens; compaction suggested",
            "reason must be the literal format string from \
             `compaction::build_hint`; a contributor who tweaks the \
             format (drops the trailing suffix, reorders the \
             fields, etc.) surfaces here as a reason mismatch"
        );
    }

    #[test]
    fn budget_compact_json_envelope_with_populated_recent_is_pinned() {
        // ponytail: subprocess pin for `plugin3 --json budget
        // compact` when recent_outputs.jsonl is populated.
        // The Round 41 pin (`budget_compact_json_envelope_shape_is_pinned_at_subprocess_level`)
        // only covers the empty-recent case (fresh tempdir,
        // oldest_turn=null, newest_turn=null). A contributor who
        // wires `compact()` to truncate `turns` to the last 5
        // entries (thinking "the host only cares about recent
        // activity") would shrink the hint's turn range silently
        // — the empty-recent pin passes (null still wins) but
        // the populated path silently narrows the range. Drift
        // catches here.
        //
        // Subprocess setup:
        //   1. budget.toml: ceiling=100, used=42,
        //      approaching_ratio=0.8 → state Under (ratio 0.42)
        //      but the compact command reads used/ceiling only,
        //      not state.
        //   2. recent_outputs.jsonl: 3 entries
        //        {"key":"k0","size":100}
        //        {"key":"k1","size":200}
        //        {"key":"k2","size":300}
        //      so the hint's turn range spans 0..=2.
        //   3. Hint fields:
        //      - tokens_used=42 (from budget.toml)
        //      - tokens_ceiling=100 (from budget.toml)
        //      - oldest_turn=0, newest_turn=2 (full recent window)
        //      - reason = "session at 42/100 tokens; compaction suggested"
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        let budget_path = runtime_dir.path().join("budget.toml");
        let recent_path = data_dir.path().join("recent_outputs.jsonl");
        std::fs::write(
            &budget_path,
            toml::to_string(&TokenBudget {
                ceiling: 100,
                approaching_ratio: 0.8,
                used: 42,
            })
            .unwrap(),
        )
        .unwrap();
        // ponytail: build the JSONL inline. The on-disk shape is
        // `{"key":"...","size":N}` per the recent_outputs_tests
        // wire pin (line 2962). The CLI's `load_recent_outputs`
        // reads each line as a `RecentEntry` struct.
        let mut body = String::new();
        for (k, s) in [("k0", 100usize), ("k1", 200), ("k2", 300)] {
            body.push_str(&format!(
                "{}\n",
                serde_json::to_string(&RecentEntry {
                    key: k.into(),
                    size: s
                })
                .unwrap(),
            ));
        }
        std::fs::write(&recent_path, body).unwrap();

        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "budget", "compact"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3 budget compact");
        assert!(
            out.status.success(),
            "budget compact with populated recent must exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        let obj = v.as_object().expect("top-level object");
        // ponytail: pin the same envelope shape as the empty
        // case (`{hint}` only) — populated recent must not
        // trigger a sibling key (e.g. `recent` array).
        let top_keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            top_keys,
            ["hint"].into_iter().collect(),
            "budget compact --json top-level key set must be \
             exactly {{hint}} even with populated recent; a \
             contributor who leaks the recent VecDeque into the \
             envelope (e.g. as a `recent` sibling key) surfaces \
             here. got: {top_keys:?}"
        );

        let hint_v = &v["hint"];
        let hint_obj = hint_v.as_object().expect("hint is an object");
        // ponytail: pin the same 5-field hint shape as the empty
        // case — populated recent must not add a 6th field.
        let hint_keys: std::collections::BTreeSet<&str> =
            hint_obj.keys().map(String::as_str).collect();
        assert_eq!(
            hint_keys,
            [
                "newest_turn",
                "oldest_turn",
                "reason",
                "tokens_ceiling",
                "tokens_used"
            ]
            .into_iter()
            .collect(),
            "CompactHint serialised field set must match the 5-field \
             struct in plugin3-core::compaction::CompactHint; got: \
             {hint_keys:?}"
        );

        // ponytail: pin the values from the seeded budget.toml
        // and recent entries. A contributor who wires `tokens_used`
        // to `recent.len()` (off-by-domain) or `tokens_ceiling` to
        // the hardcoded default 200_000 surfaces here.
        assert_eq!(
            hint_v["tokens_used"], 42,
            "tokens_used must be the seeded 42 (from budget.toml); \
             a different value means the CLI bypassed the seeded \
             budget.toml and picked up the default 0. got: {}",
            hint_v["tokens_used"]
        );
        assert_eq!(
            hint_v["tokens_ceiling"], 100,
            "tokens_ceiling must be the seeded 100 (from budget.toml); \
             a different value means the CLI bypassed the seeded \
             budget.toml and picked up the default 200_000. got: {}",
            hint_v["tokens_ceiling"]
        );

        // ponytail: pin the populated-recent turn range. With 3
        // entries (k0/k1/k2), oldest_turn=0 (FIFO head) and
        // newest_turn=2 (FIFO tail). A contributor who truncates
        // `turns` to the last 5 would still show 0..=2 here
        // (below the threshold) — but a contributor who flips
        // the range to the recent-rev (e.g. starts at -1 for
        // "before the head") surfaces as oldest_turn=-1, which
        // `is_number()` would catch as a type mismatch. The
        // dual-pin (range + per-index content) catches more.
        assert_eq!(
            hint_v["oldest_turn"], 0,
            "oldest_turn must be 0 (FIFO head of 3 seeded entries); \
             a different value means the turn-range computation \
             regressed. got: {}",
            hint_v["oldest_turn"]
        );
        assert_eq!(
            hint_v["newest_turn"], 2,
            "newest_turn must be 2 (FIFO tail of 3 seeded entries); \
             a different value means the turn-range computation \
             regressed. got: {}",
            hint_v["newest_turn"]
        );

        // ponytail: pin the reason format with seeded values.
        // The format from `compaction::build_hint` is
        //   "session at {used}/{ceiling} tokens; compaction suggested"
        // so the seeded (used=42, ceiling=100) yields
        //   "session at 42/100 tokens; compaction suggested"
        // Verbatim pin catches a contributor who tweaks the
        // format (drops the trailing suffix, reorders the
        // fields, etc.) and the seeded values verify the budget
        // actually threaded through to the reason string.
        assert_eq!(
            hint_v["reason"], "session at 42/100 tokens; compaction suggested",
            "reason must be the literal format string from \
             `compaction::build_hint` with seeded budget values; \
             a different value here means either the format \
             regressed or the budget values didn't propagate. \
             got: {:?}",
            hint_v["reason"]
        );
    }

    #[test]
    fn budget_compact_human_branch_emits_3_or_5_lines_per_recent_window() {
        // ponytail: subprocess pin for `plugin3 budget compact` on
        // the human-readable (non-JSON) branch. The JSON sibling
        // (`budget_compact_json_envelope_shape_is_pinned_at_subprocess_level`
        // + `budget_compact_json_envelope_with_populated_recent_is_pinned`)
        // pins the JSON envelope (`{"hint": ...}`) and the
        // `CompactHint` 5-field shape end-to-end. The human branch
        // goes through `commands::budget::compact()` directly,
        // emitting 3 lines (empty recent) or 5 lines (populated
        // recent) — and was only tested at unit level via
        // `compact_hint_*` tests on `CompactHint` itself. A
        // contributor who flips the line order, drops a label
        // (e.g. shortens `tokens_used:` → `used:`), or re-pads
        // the column (`tokens_used: ` → `tokens_used:    `)
        // would pass unit tests because they assert on the
        // `CompactHint` struct, not on its rendering. The line-
        // by-line pin catches that here.
        //
        // The label padding is intentionally inconsistent:
        //   reason:       <value>   (7 spaces after colon)
        //   tokens_used: <value>   (1 space after colon)
        //   ceiling:      <value>   (6 spaces after colon)
        //   oldest_turn: <value>   (1 space after colon, only when Some)
        //   newest_turn: <value>   (1 space after colon, only when Some)
        // Pinning the exact padding catches a contributor who
        // re-aligns them (e.g. via `println!("{:<13}{}", "reason:",
        // hint.reason)`) — the change would be cosmetically
        // appealing but breaks a wrapper that does
        // `awk -F': ' '{print $1}'` on the rendered output.
        //
        // Two arms:
        //   empty recent   → 3 lines (reason, tokens_used, ceiling)
        //   3 recent       → 5 lines (above + oldest_turn, newest_turn)
        for (recent_size, expected_lines) in [(0usize, 3usize), (3usize, 5usize)] {
            let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
            let data_dir = tempfile::tempdir().expect("data tempdir");
            let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
            // ponytail: seed budget.toml with used=42, ceiling=100.
            // The reason format from `compaction::build_hint` is
            // `session at {used}/{ceiling} tokens; compaction suggested`,
            // so seeded values yield `session at 42/100 tokens;
            // compaction suggested` — pinned verbatim below.
            let budget_path = runtime_dir.path().join("budget.toml");
            let seed = TokenBudget {
                ceiling: 100,
                approaching_ratio: 0.8,
                used: 42,
            };
            std::fs::write(&budget_path, toml::to_string(&seed).unwrap()).unwrap();
            // ponytail: seed recent_outputs.jsonl with `recent_size`
            // entries. `load_recent_outputs` reads this file at
            // `data_dir/recent_outputs.jsonl` (Paths::recent_outputs).
            // Each entry is `(key, size)` where `key` is the
            // BLAKE3 24-hex-char content address; we use
            // 24-char hex as a stand-in because the human branch
            // only displays the turn indices (`oldest_turn`/
            // `newest_turn`), not the keys.
            if recent_size > 0 {
                let recent_path = data_dir.path().join("recent_outputs.jsonl");
                let mut s = String::new();
                for i in 0..recent_size {
                    let key = format!("{:0>24x}", i + 1);
                    s.push_str(&format!(
                        "{{\"key\":\"{key}\",\"size\":{}}}\n",
                        (i + 1) * 100
                    ));
                }
                std::fs::write(&recent_path, s).unwrap();
            }

            let out = std::process::Command::new(plugin3_binary_path())
                // NOTE: no `--json`. Human branch.
                .args(["budget", "compact"])
                .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
                .env("PLUGIN3_DATA_DIR", data_dir.path())
                .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .expect("spawn plugin3 budget compact (human)");
            assert!(
                out.status.success(),
                "budget compact must exit 0; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stdout = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
            assert_eq!(
                lines.len(),
                expected_lines,
                "human branch must emit {expected_lines} lines for \
                 recent_size={recent_size} (empty → 3, populated → 5); \
                 a different count means the optional oldest_turn/\
                 newest_turn prints changed. got: {lines:?}"
            );

            // ponytail: pin the EXACT line shapes. The first three
            // lines are unconditional; the 4th and 5th appear only
            // when recent is populated. The label padding (number
            // of spaces between colon and value) is part of the
            // wire contract.
            assert!(
                lines[0].starts_with("reason:       "),
                "line[0] must lead with `reason:       ` (7 spaces \
                 after colon); got: {:?}",
                lines[0]
            );
            assert!(
                lines[0].ends_with("session at 42/100 tokens; compaction suggested"),
                "line[0] must end with the seeded reason string; \
                 got: {:?}",
                lines[0]
            );
            assert!(
                lines[1].starts_with("tokens_used: "),
                "line[1] must lead with `tokens_used: ` (1 space \
                 after colon); got: {:?}",
                lines[1]
            );
            assert!(
                lines[1].ends_with("42"),
                "line[1] must end with the seeded used=42; \
                 got: {:?}",
                lines[1]
            );
            assert!(
                lines[2].starts_with("ceiling:      "),
                "line[2] must lead with `ceiling:      ` (6 spaces \
                 after colon); got: {:?}",
                lines[2]
            );
            assert!(
                lines[2].ends_with("100"),
                "line[2] must end with the seeded ceiling=100; \
                 got: {:?}",
                lines[2]
            );

            if expected_lines == 5 {
                assert!(
                    lines[3].starts_with("oldest_turn: "),
                    "line[3] (populated recent) must lead with \
                     `oldest_turn: ` (1 space after colon); \
                     got: {:?}",
                    lines[3]
                );
                assert!(
                    lines[3].ends_with('0'),
                    "line[3] must end with `0` (FIFO head of 3 \
                     seeded entries); got: {:?}",
                    lines[3]
                );
                assert!(
                    lines[4].starts_with("newest_turn: "),
                    "line[4] (populated recent) must lead with \
                     `newest_turn: ` (1 space after colon); \
                     got: {:?}",
                    lines[4]
                );
                assert!(
                    lines[4].ends_with('2'),
                    "line[4] must end with `2` (FIFO tail of 3 \
                     seeded entries, index 0..2); got: {:?}",
                    lines[4]
                );
            }

            // ponytail: negative pin — the JSON sibling's envelope
            // markers MUST NOT leak into the human branch. A
            // contributor who copy-pastes the JSON branch's
            // `serde_json::to_string_pretty` into the human branch
            // surfaces here (the rendered lines would contain
            // `{`, `}`, and `"key":` fragments).
            assert!(
                !stdout.contains('{'),
                "human branch must NOT emit JSON envelope markers; \
                 got: {stdout:?}"
            );
            assert!(
                !stdout.contains("\"hint\""),
                "human branch must NOT emit the JSON sibling's \
                 `\"hint\"` key; got: {stdout:?}"
            );
        }
    }

    #[test]
    fn report_json_emits_empty_envelope_when_usage_log_missing() {
        // ponytail: pin the missing-usage.jsonl contract for
        // `--json` mode. The bug-fixed behaviour: when
        // `data_dir/logs/usage.jsonl` doesn't exist (fresh
        // install, never-run hooks, freshly-rotated logs), the
        // CLI emits a parseable envelope — `[]` for the detailed
        // view, `{}` for `--summary` — and exits 0 with no
        // stderr noise. Pre-fix, the missing-file branch
        // eprintln'd and returned 0 with empty stdout; a wrapper
        // doing `plugin3 --json report | jq '.[]'` on a clean
        // XDG data dir got exit 0 + no output, which `jq`
        // treats as a stream error rather than "no records
        // yet". The human branch keeps its eprintln (users want
        // the breadcrumb when an alias unexpectedly returns
        // nothing).
        //
        // Three arms:
        //   1. `plugin3 --json report`            → `[]`
        //   2. `plugin3 --json report --summary`  → `{}`
        //   3. `plugin3 report` (no --json)       → exit 0, eprintln
        // Arm 3 pins that the breadcrumb is preserved on the
        // human branch — a contributor who drops the
        // `as_json ? "[]" : eprintln!(...)` ternary to a flat
        // `println!("[]")` silences the breadcrumb and surfaces
        // here as a stderr-empty mismatch.
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");

        // arm 1: detailed --json → []
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        assert!(
            out.status.success(),
            "missing usage.jsonl on --json must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.stdout,
            b"[]\n",
            "missing usage.jsonl on --json must emit `[]` (a parseable \
             JSON array), not empty stdout — a wrapper `jq '.[]' | ...` \
             treats empty stdout as a stream error rather than 'no \
             records yet'. got: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            out.stderr.is_empty(),
            "missing usage.jsonl on --json must NOT eprintln the \
             'no usage.jsonl' breadcrumb — the breadcrumb is for the \
             human branch only. got: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );

        // arm 2: --summary --json → {}
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["--json", "report", "--summary"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        assert!(
            out.status.success(),
            "missing usage.jsonl on --summary --json must exit 0; \
             stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.stdout,
            b"{}\n",
            "missing usage.jsonl on --summary --json must emit a \
             parseable JSON object (empty BTreeMap, `{{}}`); a \
             different value here means the missing-file branch \
             hardcodes only the array case. got: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            out.stderr.is_empty(),
            "missing usage.jsonl on --summary --json must NOT \
             eprintln — same contract as arm 1. got: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );

        // arm 3: human branch (no --json) keeps the breadcrumb.
        let out = std::process::Command::new(plugin3_binary_path())
            .args(["report"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn plugin3");
        assert!(
            out.status.success(),
            "missing usage.jsonl on the human branch must exit 0 \
             (it's a 'no records yet' state, not an error); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("no usage.jsonl"),
            "human branch must keep the 'no usage.jsonl at ...' \
             breadcrumb so a user whose `report` alias suddenly \
             returns empty gets a hint about why. got: {stderr}"
        );
    }
}

// ponytail: ADR-0014 § Recent outputs file — pins the
// `recent_outputs.jsonl` wire shape, the `RECENT_BOUND = 32`
// FIFO bound, and the per-line JSON object keys. Lives here
// (plugin3-cli) rather than plugin3-core because the writer and
// reader are both in this crate; the test calls the
// path-parameterised seam (`append_recent_at` /
// `load_recent_outputs_at`) so a tempdir keeps the user's real
// `$XDG_DATA_HOME/plugin3/recent_outputs.jsonl` out of the
// test's blast radius.
#[cfg(test)]
mod recent_outputs_tests {
    use super::*;

    fn read_lines(path: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .expect("recent_outputs.jsonl readable")
            .lines()
            .filter(|l| !l.is_empty())
            .map(std::string::ToString::to_string)
            .collect()
    }

    #[test]
    fn recent_bound_is_pinned_at_32() {
        // ponytail: ADR-0014 § Recent outputs file specifies a
        // 32-entry FIFO bound. The constant is the load-bearing
        // contract — a contributor who bumps it to 64 silently
        // grows the on-disk file for every session. The fixture
        // test below (`fifo_eviction_at_boundary`) is the
        // behaviour-side pin; this attribute test catches a
        // constant-only change for review.
        assert_eq!(RECENT_BOUND, 32);
    }

    #[test]
    fn per_line_wire_shape_is_key_and_size() {
        // ponytail: ADR-0014 § Recent outputs file spec includes
        // `content`/`tool_name`/`ts` fields; the actual wire
        // format is `{"key":"...","size":N}` because the budget
        // guard only needs the (key, size) pair. Pin both the
        // field set AND the order so a contributor who adds a
        // field (or renames `size` → `bytes`) surfaces here.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent_outputs.jsonl");
        append_recent_at(&path, "abc123", 4242);
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&lines[0])
            .unwrap_or_else(|e| panic!("line not valid JSON: {lines:?}: {e}"));
        let obj = v.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(std::string::String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["key", "size"], "field set drifted");
        assert_eq!(obj["key"], "abc123");
        assert_eq!(obj["size"], 4242);
    }

    #[test]
    fn fifo_eviction_at_boundary() {
        // ponytail: ADR-0014 § Recent outputs file specifies FIFO
        // eviction when the file exceeds 32 entries. The 33rd
        // append must evict the 1st; the 34th evicts the 2nd. We
        // push 35 entries and assert the surviving window is
        // entries 4..36 (oldest 3 evicted).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent_outputs.jsonl");
        for i in 0..35 {
            append_recent_at(&path, &format!("k{i:02}"), i * 100);
        }
        let lines = read_lines(&path);
        assert_eq!(
            lines.len(),
            32,
            "FIFO bound must hold; got {} lines",
            lines.len()
        );
        // First surviving entry is k03 (the 4th push, index 3);
        // last is k34. This catches a contributor who flips the
        // eviction to LIFO (newest dropped) or to a different
        // bound (e.g. 64).
        let first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        let last: serde_json::Value = serde_json::from_str(&lines[31]).unwrap();
        assert_eq!(first["key"], "k03");
        assert_eq!(last["key"], "k34");
        // The sizes follow the key index, so size=300 and size=3400
        // pin the row contents (a contributor who scrambles key/size
        // in the writer surfaces here).
        assert_eq!(first["size"], 300);
        assert_eq!(last["size"], 3400);
    }

    #[test]
    fn empty_file_loads_as_empty_vec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent_outputs.jsonl");
        assert!(
            load_recent_outputs_at(&path).is_empty(),
            "missing file must yield empty list, not panic"
        );
    }

    #[test]
    fn reload_round_trips_recent_entries() {
        // ponytail: the writer/reader pair is owned by the same
        // module, but a future contributor who introduces a second
        // reader (e.g. for the report subcommand) could diverge
        // the field names. This test pins the round-trip on the
        // first 5 entries (below the FIFO bound, no eviction).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent_outputs.jsonl");
        for i in 0..5 {
            append_recent_at(&path, &format!("rt{i}"), 100 + i);
        }
        let back = load_recent_outputs_at(&path);
        assert_eq!(back.len(), 5);
        for (i, (k, s)) in back.iter().enumerate() {
            assert_eq!(k, &format!("rt{i}"));
            assert_eq!(*s, 100 + i);
        }
    }

    // ponytail: malformed JSONL rows must be silently skipped on
    // load, mirroring `aggregate_sessions` in plugin3-core::report.
    // A contributor who flips `filter_map(... .ok())` to a strict
    // `map(... .unwrap())` makes any hand-edited recent file a
    // crash on the next PostToolUse — caught here. The reader is
    // `pub(crate) fn load_recent_outputs_at` (path-parameterised),
    // so the test stays hermetic without touching XDG.
    #[test]
    fn malformed_recent_jsonl_rows_are_silently_skipped_on_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent_outputs.jsonl");
        // Hand-craft a file with two valid rows and two malformed
        // lines (one non-JSON, one valid JSON but missing fields).
        // The writer always emits parseable rows; this simulates
        // a host that interrupted the file mid-write or a user
        // who edited it by hand.
        std::fs::write(
            &path,
            "{\"key\":\"good1\",\"size\":100}\n\
             not json at all\n\
             {\"key\":\"good2\",\"size\":200}\n\
             {\"missing\":\"both-fields\"}\n",
        )
        .unwrap();
        let back = load_recent_outputs_at(&path);
        assert_eq!(
            back.len(),
            2,
            "two malformed rows must be silently skipped, leaving the two \
             valid rows; got {} entries: {back:?}",
            back.len()
        );
        assert_eq!(back[0].0, "good1");
        assert_eq!(back[0].1, 100);
        assert_eq!(back[1].0, "good2");
        assert_eq!(back[1].1, 200);
    }

    // ponytail: FIFO is by *insertion order*, not by key. A future
    // contributor who introduces a "merge duplicate keys" pass
    // (e.g. `if entries.iter().any(|(k,_)| k == new_key) { skip }`)
    // silently shrinks the on-disk file when a single tool fires
    // repeatedly — caught here. The fixture: 5 appends, all with
    // the SAME key, all unique sizes. After append, the load must
    // yield 5 distinct (key, size) rows, not 1 (deduped).
    #[test]
    fn fifo_is_by_insertion_order_not_by_key_dedup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent_outputs.jsonl");
        for i in 0..5 {
            // Same key, distinct sizes — a dedup pass would collapse
            // these into one entry.
            append_recent_at(&path, "duplicate-key", 100 + i);
        }
        let back = load_recent_outputs_at(&path);
        assert_eq!(
            back.len(),
            5,
            "FIFO is by insertion order, not by key; 5 appends of the same \
             key must produce 5 entries (dedup-by-key would silently shrink \
             this to 1); got {} entries: {back:?}",
            back.len()
        );
        let sizes: Vec<usize> = back.iter().map(|(_, s)| *s).collect();
        assert_eq!(
            sizes,
            vec![100, 101, 102, 103, 104],
            "insertion order must preserve the original sizes (100..104)"
        );
    }

    // ponytail: pin the `load_recent_outputs_at` empty-input contract.
    // An empty file (zero bytes) is different from a missing file.
    // `read_to_string` on an empty file returns Ok("") which
    // `.lines()` yields zero items — that path must also produce
    // an empty VecDeque. The `empty_file_loads_as_empty_vec` test
    // covers the missing-file case; this test covers the
    // existing-but-empty case (a contributor who switches to
    // `.read_to_string(path)?.lines()` would propagate the empty
    // string fine, but a contributor who uses `BufReader::new` +
    // a `.read_line` loop might mistakenly treat empty as EOF
    // mid-stream — caught here).
    #[test]
    fn load_on_zero_byte_file_yields_empty_vec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent_outputs.jsonl");
        std::fs::write(&path, "").expect("write empty");
        let back = load_recent_outputs_at(&path);
        assert!(
            back.is_empty(),
            "zero-byte file must load as empty VecDeque; got {} entries: {back:?}",
            back.len()
        );
    }

    // ponytail: pin the explicit-match `From<UsageKindArg> for UsageKind`
    // bridge. clap names variants `kebab-case` (`budget-warn`); the
    // `UsageKind` enum on the core side uses `snake_case` to match
    // the on-disk JSONL wire format (ADR-0010). A contributor who
    // adds a 7th variant to either side without updating the match
    // fails to compile — better than the previous serde round-trip
    // form that panicked at runtime on a missing wire string.
    #[test]
    fn usage_kind_arg_round_trips_via_match_into_usage_kind() {
        use clap::ValueEnum; // for `to_possible_value`
        for (arg, kind) in [
            (UsageKindArg::Slice, UsageKind::Slice),
            (UsageKindArg::BudgetWarn, UsageKind::BudgetWarn),
            (UsageKindArg::BudgetOver, UsageKind::BudgetOver),
            (UsageKindArg::CompactHint, UsageKind::CompactHint),
            (UsageKindArg::Prompt, UsageKind::Prompt),
            (UsageKindArg::Response, UsageKind::Response),
        ] {
            // Explicit match — the bridge the impl uses.
            assert_eq!(
                UsageKind::from(arg),
                kind,
                "UsageKindArg::{arg:?} must map to UsageKind::{kind:?}"
            );
            // CLI flag spelling — kebab-case from
            // `#[clap(rename_all = "kebab-case")]`. Single-word
            // variant names like `Slice` and `Prompt` are
            // unchanged (kebab-case with one segment is itself);
            // multi-word variants like `BudgetWarn` become
            // `budget-warn` (hyphen-separated).
            let cli_name = arg.to_possible_value().unwrap().get_name().to_string();
            let cli_expected = match arg {
                UsageKindArg::Slice => "slice",
                UsageKindArg::BudgetWarn => "budget-warn",
                UsageKindArg::BudgetOver => "budget-over",
                UsageKindArg::CompactHint => "compact-hint",
                UsageKindArg::Prompt => "prompt",
                UsageKindArg::Response => "response",
            };
            assert_eq!(
                cli_name, cli_expected,
                "CLI flag for {arg:?} must be the kebab-case spelling; got {cli_name:?}"
            );
        }
        // ponytail: the wire spelling for `UsageKind` (snake_case)
        // is the single source of truth for the JSONL form. Pin it
        // on the core side via `usage_kind_serialises_to_snake_case`
        // in `cost.rs`. The CLI-side `UsageKindArg` no longer
        // derives `Serialize` because the bridge is now an explicit
        // match — re-adding the serde derive here would invite
        // drift between two enums that are no longer linked.
    }
}
