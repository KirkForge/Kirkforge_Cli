# ADR-0010: Cost reporting — usage.jsonl + report subcommand

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

A user who installs Plugin3 wants to see *how much* it saved.
Without cost reporting, the plugin is a black box: the user
sees lower token bills but cannot attribute the savings to a
specific prompt or tool output.

Cost reporting is the audit trail. The plugin emits a
`usage.jsonl` file with one record per significant event;
the user queries it via `plugin3 report`.

## Decision

### usage.jsonl format

Each line is a JSON object:

```json
{"ts":"2026-06-24T10:23:45.123Z","kind":"slice","bytes_in":52428,"bytes_out":8192,"tool":"cargo test","session_id":"abc-123"}
{"ts":"2026-06-24T10:23:51.456Z","kind":"budget_warn","tokens_used":160000,"tokens_ceiling":200000,"session_id":"abc-123"}
{"ts":"2026-06-24T10:24:02.789Z","kind":"compact_hint","tokens_used":195000,"tokens_ceiling":200000,"session_id":"abc-123"}
```

### UsageKind enum

```rust
// crates/plugin3-core/src/cost.rs

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    /// A tool output was sliced.
    Slice,
    /// The budget guard emitted a warning (Approaching).
    BudgetWarn,
    /// The budget guard refused and suggested compaction (Over).
    BudgetOver,
    /// A compaction hint was emitted.
    CompactHint,
    /// A prompt was sent to the model. Reserved — no emission site yet
    /// (the MVP's budget guard decides per turn, not per prompt token;
    /// adding `tokens_in`/`tokens_out`/`model` here requires a real
    /// token counter, which is a future ADR).
    Prompt,
    /// A model response was received. Reserved — same as `Prompt`.
    Response,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UsageRecord {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub kind: UsageKind,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_in: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_out: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_ceiling: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

/// Backing type for the `[usage]` TOML section (ADR-0005
/// § Default ceiling). One field today (`enabled`); a
/// future ADR adds per-kind filters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageConfig {
    pub enabled: bool,
}
```

The record deliberately has no `tokens_in`/`tokens_out`/`model`
fields: those would require a per-prompt token counter, which
the MVP defers (ADR-0005 § Token estimation notes the swap to
a real tokeniser is a future ADR). The drift test in
`tests/cost_record_drift.rs` pins the field set so a future
contributor adding `tokens_in` cannot do so without updating
both the struct and this ADR. The `Prompt`/`Response` variants
are reserved — they exist in the enum so `report --kind prompt`
filters gracefully when an empty record of that kind appears,
but the MVP emits nothing.

### Intervention → UsageKind mapping

The `classify_kind` function translates `Intervention` (ADR-0005
§ Auto-intervention) to `Option<UsageKind>`:

```rust
// crates/plugin3-core/src/cost.rs

pub fn classify_kind(intervention: &Intervention) -> Option<UsageKind> {
    match intervention {
        Intervention::Allow => None,
        Intervention::Warn { .. } => Some(UsageKind::BudgetWarn),
        Intervention::Slice { .. } => Some(UsageKind::Slice),
        Intervention::Compact { .. } => Some(UsageKind::BudgetOver),
    }
}
```

ponytail: `Allow → None`. A healthy turn at `Under` state is
not a "significant event" and must not inflate the warnings
count in `plugin3 report --summary`. Without `Option`, every
healthy turn would emit a `BudgetWarn` and the user's summary
view would lie. The drift test `classify_kind_allow_returns_none`
pins this regression guard. The drift test
`cost_reporting_spec_drift::adr_0010_classify_kind_section_lists_four_arms`
pins that the ADR documents all four arms — a contributor who
adds a fifth `Intervention` variant but forgets to update this
match surfaces here.

### Emission site

The `emit_usage` function is called from every hook handler:

```rust
// crates/plugin3-core/src/cost.rs

pub fn emit_usage(record: &UsageRecord) {
    emit_usage_at(record, &usage_path());
}

// ponytail: path-parameterised core of emit_usage. The
// public `emit_usage` is a thin wrapper that targets the
// user's XDG usage.jsonl; tests point this at a tempdir so
// they exercise the real file-append code path without
// touching the user's data dir.
pub(crate) fn emit_usage_at(record: &UsageRecord, path: &std::path::Path) {
    // ponytail: short-circuit before any I/O. The check is
    // two syscalls (read + close) and we save an append +
    // fsync.
    if !is_usage_enabled_at(&Paths::resolve().config_file()) {
        return;
    }
    let line = match serde_json::to_string(&record) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("plugin3: failed to serialise usage record: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // ponytail: this exists — losing usage records is not
    // load-bearing for the MVP. Replace with a fatal init
    // when reporting becomes required.
    let mut file = match std::fs::OpenOptions::new()
        .create(true).append(true).open(path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("plugin3: usage.jsonl open failed ({e}); dropping record");
            return;
        }
    };
    use std::io::Write;
    let _ = writeln!(file, "{line}");
}
```

ponytail: the earlier draft specified `tracing::error!` on
serialise failure and `tracing::warn!` on file-open failure.
The MVP does **not** depend on `tracing` (ADR-0017 § Workspace
Cargo.toml) — both error paths emit one `eprintln!` line tagged
`plugin3:` to stderr and return early (losing one record is
not load-bearing). The drift test
`cost_reporting_spec_drift::adr_0010_emission_site_uses_eprintln_not_tracing`
pins the no-`tracing` shape so a contributor who re-pastes the
older tracing-heavy example surfaces here.

The `// ponytail: this exists` comment is deliberate — losing
usage records is not load-bearing for the MVP. A future
contributor who needs strict audit replaces this with a fatal
init.

### File location

```rust
fn usage_path() -> std::path::PathBuf {
    Paths::resolve().usage_log()
}
```

The path comes from `Paths::resolve()` (ADR-0014 § Path
resolver). On Linux without `PLUGIN3_DATA_DIR`, the resolved
path is `~/.local/share/plugin3/logs/usage.jsonl` — the `logs`
subdir is auto-created on first emit (see
`create_dir_all(parent)` in `emit_usage_at`). A user who sets
`PLUGIN3_DATA_DIR=/tmp/p3` gets `/tmp/p3/logs/usage.jsonl`.

ponytail: the earlier draft specified an inline
`std::env::var("PLUGIN3_DATA_DIR")` +
`directories::ProjectDirs` fallback chain inside `cost.rs`.
The MVP delegates to `Paths::resolve()` (ADR-0014) so the
env-var handling lives in exactly one place — `cost.rs`
does not duplicate the `PLUGIN3_DATA_DIR` / XDG precedence
chain. The `directories` crate is wired (ADR-0017 § Workspace
Cargo.toml: `directories = "5"`) but only consumed by
`Paths::resolve()`; the file-IO modules in `cost.rs` see a
resolved `PathBuf` and don't reach for the crate directly.

### Report subcommand

```rust
// crates/plugin3-cli/src/commands/report.rs

#[derive(Parser, Debug)]
pub struct ReportArgs {
    /// Show summary only (one line per session).
    #[arg(long)]
    summary: bool,
    /// Filter to a single session.
    #[arg(long)]
    session: Option<String>,
    /// Filter to a single kind.
    #[arg(long, value_enum)]
    kind: Option<UsageKind>,
    /// Show last N records.
    #[arg(long, default_value = "100")]
    last: usize,
    /// Output JSON instead of human text.
    #[arg(long)]
    json: bool,
}
```

`plugin3 report` reads `usage.jsonl` and emits:

- Summary view: total bytes saved, total warnings, total
  compactions, per-session totals.
- Detailed view: last N records, one per line.
- Filtered view: by session or kind.

```bash
$ plugin3 report --summary
session abc-123  bytes_saved=2345678  warnings=3  compactions=1
session def-456  bytes_saved=12345    warnings=0  compactions=0
```

### Retention

The plugin does not garbage-collect `usage.jsonl` in the MVP.
The file grows linearly with usage; at ~200 bytes per record
and 100 records/day, the file is ~7 MB/year. A future ADR
adds rotation (e.g. `usage-YYYY-MM.jsonl` files with monthly
pruning).

### Privacy

The `usage.jsonl` file lives in the user's data directory
(`~/.local/share/plugin3/logs/usage.jsonl` on Linux). It is
not uploaded, not shared, not sent to a remote service. The
README documents the path explicitly.

A user who wants to disable usage reporting sets:

```toml
# ~/.config/plugin3/config.toml
[usage]
enabled = false
```

The `emit_usage_at` function reads `ConfigFile.usage.enabled`
on every emit and returns early when disabled. Missing *and*
malformed config both default to enabled — a typo must not
silently disable reporting. The drift test
`is_usage_enabled_tolerates_malformed_config` (in `cost.rs`)
pins the malformed-defaults-to-enabled behaviour.

## Consequences

Negative first:

- Writing to `usage.jsonl` on every event is I/O. At 100
  records/day, the overhead is negligible. At 10 000/day
  (a heavy session), the overhead is still <1 ms/record. A
  future contributor who needs buffered writes adds a
  `BufWriter`.
- The MVP does not rotate the file. A long-running box
  accumulates a single growing file. The README warns about
  this.

Positive:

- The user sees exactly what Plugin3 did. `plugin3 report
  --summary` is a one-liner audit.
- The JSONL format is greppable: `grep '"kind":"slice"' ~/.local/share/plugin3/logs/usage.jsonl`.
- Disabling reporting is a config flag, not a code change.

## Implementation notes

The `cost` module lives at
`crates/plugin3-core/src/cost.rs`. The `emit_usage` function
is the only public API; the rest of the module is private
helpers.

Tests:

1. `emit_usage_writes_jsonl_line` — emit one record, read the
   file, assert one valid JSON line.
2. `usage_record_round_trips` — serialise, deserialise, assert
   equality.
3. `usage_disabled_flag_skips_writes` — set `enabled = false`,
   call `emit_usage`, assert the file is empty.
4. `report_summary_aggregates_per_session` — write 10 records
   across 2 sessions, run `plugin3 report --summary`, assert
   the per-session totals.

The drift test (ADR-0016) pins the JSON shape so a contributor
who renames a field surfaces the change for review.