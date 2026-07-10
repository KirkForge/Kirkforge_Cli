# ADR-0015: CLI design — clap-derive, env override precedence

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

The binary is the user-facing surface. Plugin3's CLI must:

- Be discoverable: `--help` lists every flag.
- Be consistent: flag names match across subcommands.
- Be scriptable: every subcommand accepts `--json`.
- Respect precedence: CLI > env > file > default.
- Use sysexits codes for ops-tool branching.

Mirrors Stratum ADR-0016. Plugin3 reuses the same shape;
the differences are subcommand-specific (`budget`,
`report`) and the missing subcommands that Stratum has but
Plugin3 does not (`init`, `mode`, `rules`, `debt`).

## Decision

### Top-level structure

```rust
// crates/plugin3-cli/src/main.rs

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "plugin3", version, about = "Output slicing + token budget for AI agent context.")]
pub struct Cli {
    /// Emit machine-readable JSON to stdout (ADR-0015).
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
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
        /// Write-test every path; exit 78 (EX_CONFIG) on failure.
        /// ADR-0015 § Validate.
        #[arg(long)]
        validate: bool,
    },
}
```

ponytail: the earlier draft declared five extra clap global
flags (`verbose`, `quiet`, `config_dir`, `data_dir`,
`runtime_dir`, `config`) plus a `Version` subcommand variant.
The MVP ships only `json: bool` as a global clap flag —
verbose/quiet are unused and the three `PLUGIN3_*_DIR` env
vars are read directly by `Paths::resolve()` rather than
threaded through clap. `Version` is clap's built-in
`--version` flag. A contributor who re-adds the path flags
without re-wiring `Paths::resolve()` documents a clap shape
the impl does not have.

### Subcommand shapes

```rust
#[derive(ValueEnum, Clone, Copy, Debug)]
#[clap(rename_all = "kebab-case")]
pub enum HookKind {
    /// Slice the tool result before the host reads it.
    PostToolUse,
    /// Check the budget before the host sends the prompt to the model.
    UserPromptSubmit,
    /// Emit a CompactHint so the host's compactor has a head-start.
    PreCompact,
}

#[derive(Parser, Debug)]
#[command(about = "Inspect or set the token budget.")]
pub struct BudgetCmd {
    #[command(subcommand)]
    sub: BudgetSub,
}

#[derive(Subcommand, Debug)]
pub enum BudgetSub {
    /// Print the current budget state (used, ceiling, state).
    Status,
    /// Set the budget ceiling for this session.
    Set {
        ceiling: usize,
        /// Persist as the default in config.toml (ADR-0015).
        #[arg(long)]
        default: bool,
    },
    /// Emit a CompactHint for the host's compactor (ADR-0008).
    Compact {
        /// Print the hint as JSON (default: human-readable).
        #[arg(long)]
        json: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
#[clap(rename_all = "kebab-case")]
pub enum UsageKindArg {
    Slice,
    BudgetWarn,
    BudgetOver,
    CompactHint,
    Prompt,
    Response,
}
```

ponytail: the earlier draft declared phantom wrapper
structs (`HookArgs`, `BudgetArgs`, `ReportArgs`,
`ConfigArgs`) that the impl never names. The MVP inlines
`Report { ... }` and `Config { ... }` directly into the
`Command` enum as struct variants; `Hook { kind: HookKind }`
takes the kind directly rather than wrapping it in
`HookArgs { subcommand: HookSubcommand }`. `ConfigArgs` is
flag-based (`--show-sources`, `--validate`) rather than
subcommand-based (`Show`, `ShowSources`, `Validate`) — the
two booleans read cleaner than three unit variants in a
single-line help table.

### Precedence chain (CLI > env > file > default)

```rust
// crates/plugin3-cli/src/precedence.rs

pub(crate) fn resolve_config_path(
    cli_config: Option<&std::path::Path>,
    env: &dyn EnvSource,
    xdg: &std::path::Path,
) -> std::path::PathBuf {
    if let Some(p) = cli_config { return p.to_path_buf(); }       // CLI flag
    if let Some(p) = env.get("PLUGIN3_CONFIG") {                   // env
        return std::path::PathBuf::from(p);
    }
    xdg.join("config.toml")                                        // XDG default
}
```

The chain is identical to Stratum ADR-0016, with `PLUGIN3_*`
env var names instead of `STRATUM_*`. A contributor who
memorises one chain has memorised both.

### `--json` for every subcommand

```rust
// crates/plugin3-cli/src/commands/budget.rs

pub(crate) fn status(as_json: bool) {
    let b = load_budget();
    if as_json {
        let resp = serde_json::json!({
            "used": b.used,
            "ceiling": b.ceiling,
            "state": b.state(),
        });
        println!("{}", serde_json::to_string_pretty(&resp).unwrap());
    } else {
        println!("used: {} / {} ({:?})", b.used, b.ceiling, b.state());
    }
}
```

ponytail: the earlier draft specified a shared
`fn emit<T: serde::Serialize>(cli: &Cli, human, machine)`
helper. The MVP inlines the `if as_json` branch inside each
subcommand handler instead — the human/machine shapes differ
per subcommand (Budget::Status has `(used, ceiling, state)`,
Report has `(records[])`, Compact has `(hint)`) and an inline
conditional reads more clearly than a closure over a
heterogeneous `(human: &str, machine: &T)` pair. Each
subcommand's `--help` shows the global `--json` flag because
clap renders `global = true` flags in every subcommand's help.

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Generic failure |
| 64 | Usage error (clap parses this for us) |
| 78 | `EX_CONFIG` — config parse or backend init failure |
| 130 | `SIGINT` (default) |

```rust
// crates/plugin3-cli/src/exit.rs

pub fn exit_config_err(msg: &str) -> ! {
    eprintln!("plugin3: {}", msg);
    std::process::exit(78);
}

pub fn exit_usage_err(msg: &str) -> ! {
    eprintln!("plugin3: {}", msg);
    std::process::exit(64);
}
```

### Help output conventions

Every subcommand's `--help` lists:

1. A one-line description.
2. Usage line with all positional and required args.
3. Options section with every flag, default, and env var.
4. Examples section with 2–4 common invocations.
5. See also: link to the relevant ADR.

```
$ plugin3 budget --help
Inspect or set the token budget.

Usage: plugin3 budget <SUBCOMMAND>

Options:
  -h, --help     Print help

Subcommands:
  status         Print the current budget state
  compact        Suggest a compaction to the host
  set            Set the budget ceiling for this session

Examples:
  # Print the current budget state
  $ plugin3 budget status

  # Set the ceiling to 300000 tokens for this invocation
  $ plugin3 budget set 300000

  # Persist 300000 as the new default
  $ plugin3 budget set 300000 --default

See also: ADR-0005 (token budget), ADR-0014 (state management)
```

### Shell completion

`clap_complete` generates shell completion at build time. The
scripts are written to `target/plugin3-completion.{bash,zsh,fish}`
and installed via the `--install-completion` subcommand
(deferred; the build step is enough for MVP).

## Consequences

Negative first:

- The CLI surface is smaller than Stratum's (no `mode`,
  `rules`, `debt`). A user migrating from Stratum finds
  some subcommands missing — they were Stratum-specific.
- `--json` adds a flag to every subcommand. The trade is
  uniform scriptability.

Positive:

- `--help` is comprehensive, consistent, includes ADR
  cross-references.
- `--json` makes every subcommand scriptable.
- Exit codes follow sysexits; ops tools branch on them.
- CLI > env > file > default precedence is documented and
  asserted by tests.

## Implementation notes

The CLI lives at `crates/plugin3-cli/src/`. Subcommand
implementations live in submodules:

```
crates/plugin3-cli/src/
├── main.rs
├── precedence.rs
├── exit.rs
├── hooks/
│   └── mod.rs    # all three handlers (PostToolUse / UserPromptSubmit / PreCompact)
└── commands/
    ├── mod.rs
    ├── budget.rs
    ├── report.rs
    └── config.rs
```

ponytail: the `hooks/` split (one file per hook) is deferred —
all three handlers live in `hooks/mod.rs`. Likewise `args.rs`
and `config_loader.rs` are folded into `main.rs` until a
contributor finds a reason to split. The drift test
`adr_0015_impl_directory_layout_matches_adr` (in
`tests/cli_design_spec_drift.rs`) pins this layout so a
contributor who re-splits files without updating the ADR
surfaces here.

The `clap` features enabled in `Cargo.toml`:

```toml
[dependencies.clap]
version = "4"
features = ["derive", "env"]
```

Tests:

1. `resolve_config_path_cli_wins_over_xdg`
2. `resolve_config_path_env_wins_over_cli`
3. `budget_status_emits_human_by_default`
4. `budget_status_emits_json_when_json_flag_set`
5. `unknown_subcommand_exits_64`
6. `config_validate_exits_78_on_unknown_field`
7. `report_summary_aggregates_per_session`

Test 2 is the regression for the precedence chain.