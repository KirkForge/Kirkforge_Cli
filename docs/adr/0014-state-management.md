# ADR-0014: State management — XDG dirs, atomic flag file

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

Plugin3 needs to persist:

- The budget state (used tokens, ceiling).
- The recent tool outputs list (for budget auto-slicing).
- The host detection result.
- The offload store (slice markers).

The state lives in three locations (mirrors Stratum
ADR-0015):

- **Config** — `$XDG_CONFIG_HOME/plugin3/` (default
  `~/.config/plugin3/`).
- **Data** — `$XDG_DATA_HOME/plugin3/` (default
  `~/.local/share/plugin3/`).
- **Runtime** — `$XDG_RUNTIME_DIR/plugin3/` (default
  `/run/user/$UID/plugin3/`).

## Decision

### Directory layout

```
$XDG_CONFIG_HOME/plugin3/
└── config.toml              # user-editable config

$XDG_DATA_HOME/plugin3/
├── slices/                  # OffloadStore (FileOffloadStore, per ADR-0004)
│   └── <24-hex-key>         # one file per slice
├── recent_outputs.jsonl     # recent tool outputs (bounded)
└── logs/
    └── usage.jsonl          # cost reporting (ADR-0010)

$XDG_RUNTIME_DIR/plugin3/
└── budget.toml              # session-local budget state (used counter)
```

ponytail: the earlier draft listed `slices.sqlite` (a
SQLite-backed OffloadStore), `anchors/<workspace_hash>` (per
ADR-0011's deferred knowledge system), and a `lock` file in
`$XDG_RUNTIME_DIR/plugin3/`. None of those are in the impl
today:
- `slices.sqlite` was renamed to `slices/` when the
  `FileOffloadStore` (ADR-0004) became the persistent
  backend. Adding a SQLite OffloadStore is a future ADR with
  a `rusqlite` dep; until then the directory layout does not
  carry a SQLite file.
- `anchors/<workspace_hash>` is reserved for ADR-0011, which
  is **deferred**. Adding the anchors layout is part of
  un-deferring ADR-0011, not a standalone change to this
  ADR.
- The `lock` file in runtime_dir is unused — see § Runtime
  lock below for the Ponytail rationale.

B2 update: `budget.toml` moved from `$XDG_DATA_HOME/plugin3/`
to `$XDG_RUNTIME_DIR/plugin3/` so the `used` counter is
session-local. `config.toml` continues to persist the user's
`ceiling`/`approaching_ratio` defaults (ADR-0005).

The drift test
`tests/state_spec_drift.rs::adr_0014_directory_layout_block`
pins the absence of `slices.sqlite`, `anchors/`, and `lock`
in the § Directory layout block and the presence of
`$XDG_RUNTIME_DIR` for `budget.toml`.

### Path resolution

```rust
// crates/plugin3-core/src/paths.rs

pub struct Paths {
    pub config_dir: std::path::PathBuf,
    pub data_dir: std::path::PathBuf,
    pub runtime_dir: std::path::PathBuf,
}

impl Paths {
    pub fn resolve() -> Self {
        let proj = directories::ProjectDirs::from("dev", "kirkforge", "plugin3");
        let (cfg_default, data_default, run_default) = match proj {
            Some(p) => (
                p.config_dir().to_path_buf(),
                p.data_dir().to_path_buf(),
                p.runtime_dir().map_or_else(
                    || p.data_dir().to_path_buf(),
                    std::path::Path::to_path_buf,
                ),
            ),
            None => (
                std::path::PathBuf::from("."),
                std::path::PathBuf::from("."),
                std::path::PathBuf::from("."),
            ),
        };
        Self {
            config_dir: std::env::var("PLUGIN3_CONFIG_DIR")
                .map_or(cfg_default, std::path::PathBuf::from),
            data_dir: std::env::var("PLUGIN3_DATA_DIR")
                .map_or(data_default, std::path::PathBuf::from),
            runtime_dir: std::env::var("PLUGIN3_RUNTIME_DIR")
                .map_or(run_default, std::path::PathBuf::from),
        }
    }
}
```

The `PLUGIN3_*_DIR` env vars override the XDG defaults
returned by `directories::ProjectDirs`. There are no
path-override clap flags — the env vars are the only
override surface (an earlier draft specified three clap
flags that were never wired through; ADR-0015 § Top-level
structure ships only `json: bool` as a global clap flag).

ponytail: the earlier draft used
`ProjectDirs::from(...).expect("home directory is required")`
and would have panicked on a host without `HOME`/`XDG_*_HOME`
set (a headless CI runner, a Docker container without env).
The MVP's `match proj { Some/None }` falls back to `"."`
in that case — `Plugin3` then writes its budget file to the
working directory instead of panicking, which is the safer
default for an MVP that may run on minimal environments.
The in-file test `resolve_does_not_panic` pins the
no-panic contract.

### Atomic flag file for budget

The budget state file is written atomically via
`tempfile::NamedTempFile` + `persist`. The atomic helper
lives at `crates/plugin3-core/src/atomic_write.rs` (ponytail:
the earlier draft located it in a `state.rs` module; the
helper is small enough — one function — that it earns its own
file alongside `paths.rs`). The save/load entry points
themselves live in the CLI's `main.rs` because they handle
the precedence chain (config vs runtime overlay per
ADR-0015), which is CLI-side.

```rust
// crates/plugin3-core/src/atomic_write.rs
// crates/plugin3-cli/src/main.rs (save_budget / load_budget entry points)

use std::io::Write;

pub fn atomic_write_text(path: &Path, label: &str, body: &str) {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if let Err(e) = std::fs::create_dir_all(parent) {
        eprintln!("plugin3: {label} dir create failed: {e}");
        return;
    }
    let mut tmp = match tempfile::NamedTempFile::new_in(parent) {
        Ok(t) => t,
        Err(e) => { eprintln!("plugin3: {label} tmpfile create failed: {e}"); return; }
    };
    if let Err(e) = tmp.write_all(body.as_bytes()) {
        eprintln!("plugin3: {label} tmpfile write failed: {e}");
        return;
    }
    if let Err(e) = tmp.flush() {
        eprintln!("plugin3: {label} tmpfile flush failed: {e}");
        return;
    }
    if let Err(e) = tmp.persist(path).map_err(|e| e.error) {
        eprintln!("plugin3: {label} persist failed: {e}");
    }
}
```

ponytail: the helper does not return a `Result` — failure
prints a tagged warning and returns. The earlier draft's
`save_budget` returned `std::io::Result<()>` and would have
propagated an `Err` to the caller, which in turn would have
needed to log + carry on. The CLI caller already does that
in the surrounding code, so the helper folds both into a
single log + continue path. The `label` parameter exists so
the failure log reads the same whether it came from a budget
file or a config file — a future contributor chasing a
corrupt-budget report sees a single label style across both
writers.

The atomic write means a crash mid-write leaves the previous
budget state intact. The next hook invocation reads the
previous state — slightly stale but consistent.

### Recent outputs file

The `recent_outputs.jsonl` is appended-only. A new entry is
written on every PostToolUse; old entries are evicted when
the file exceeds `RECENT_BOUND` entries (FIFO). The bound
is pinned to 32 in the CLI's `main.rs` and tested by
`recent_bound_is_pinned_at_32`.

```rust
// crates/plugin3-cli/src/main.rs

const RECENT_BOUND: usize = 32;

#[derive(serde::Deserialize, serde::Serialize)]
struct RecentEntry {
    key: String,
    size: usize,
}

fn append_recent(key: &str, size: usize) {
    // ponytail: the impl takes (key, size) — the content itself
    // is not stored, only the size, so the file stays small
    // (~64 bytes per entry × 32 = 2 KB on disk). A future ADR
    // stores content too if retrieval is needed.
    append_recent_at(&Paths::resolve().recent_outputs(), key, size)
}

// ponytail: path-parameterised so the in-file tests
// (`recent_bound_is_pinned_at_32`, `fifo_eviction_at_boundary`,
// `per_line_wire_shape_is_key_and_size`) can point at a
// tempdir without mutating the process-wide `PLUGIN3_*_DIR`
// env vars — the public `append_recent` is the production
// entry point.
fn append_recent_at(path: &std::path::Path, key: &str, size: usize) {
    let mut entries = load_recent_outputs_at(path);
    entries.push_back((key.to_string(), size));
    while entries.len() > RECENT_BOUND { entries.pop_front(); }
    let mut body = String::new();
    for (k, s) in &entries {
        body.push_str(&serde_json::to_string(&RecentEntry { key: k.clone(), size: *s }).unwrap());
        body.push('\n');
    }
    atomic_write_text(path, "recent", &body);
}
```

The rewrite is O(N) on every PostToolUse; with N=32 it is
cheap. A future ADR appends in place and only rewrites when
the bound is exceeded.

### Runtime lock (deferred — Ponytail)

ponytail: the earlier draft specified a `flock`-style
advisory lock at `$XDG_RUNTIME_DIR/plugin3/lock` using
`fs2::FileExt` and a `with_lock` helper. The MVP has **no
concurrent hook invocations** in a single session: each
Claude Code / Cursor / Aider invocation runs the plugin as
a fresh process, so the second invocation writes to a
different PID's runtime state. Adding a `flock` helper
without a measured contention case would lock a dep
(`fs2` was removed in the ADR-0017 reconciliation) for
zero observed benefit.

The drift test
`tests/state_spec_drift.rs::adr_0014_no_runtime_lock_section`
pins the absence of the § Runtime lock example block, the
`fs2::FileExt` import, and the `with_lock` helper. If a
future ADR adds a multi-process / daemon mode that genuinely
needs the lock, re-introduce the section with a `fcntl`
binding (no new dep) and update the drift test in lockstep.

## Consequences

Negative first:

- Three XDG dirs is more than one. The trade is the
  separation of concerns: config is small + user-editable,
  data is large + system-managed, runtime is ephemeral +
  per-session.
- The 32-entry recent outputs bound is arbitrary. A user
  with very large tool outputs may want a larger bound.
  The MVP default is fine for the average session.
- The atomic write via `tempfile::NamedTempFile` requires
  the parent directory to be writable. A user with a
  read-only data dir cannot save budget state — the plugin
  logs a warning and runs in-memory (ADR-0010's `// ponytail`
  comment covers this).

Positive:

- The flag file is atomic. A crash mid-write leaves the
  previous state intact.
- The XDG layout is standard. A user with multiple KirkForge
  plugins sees one set of XDG dirs per plugin.

## Implementation notes

The `paths` module lives at
`crates/plugin3-core/src/paths.rs`. The atomic write helper
lives at `crates/plugin3-core/src/atomic_write.rs`. The
budget save/load + recent-outputs FIFO list live in
`crates/plugin3-cli/src/main.rs` (the CLI's `main.rs`
because they are CLI-side concerns: precedence chain, the
32-entry bound, and the recent-outputs wire shape).

ponytail: the earlier draft prescribed a
`crates/plugin3-core/src/state.rs` module. The split that
materialised puts the *path resolution* in `paths.rs` (a
small, pure helper that callers want without the binary's
`main`) and the *write orchestration* in the CLI's
`main.rs` (because it interacts with the precedence chain).
A pure `state.rs` module that bundled both would have
forced the CLI to depend on a module that also served
core-only callers (e.g. a future daemon); keeping the two
concerns separate lets each live where its callers live.

The drift test (ADR-0016) pins:

- The default paths (`$XDG_*_HOME/plugin3/...`).
- The atomic write round-trip.
- The 32-entry recent-outputs bound (via
  `recent_bound_is_pinned_at_32` in `main.rs`).

A contributor who changes the path layout updates the
README and the drift test; the test catches unintentional
changes.