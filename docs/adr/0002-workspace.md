# ADR-0002: Workspace layout

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

Plugin3 needs to compile fast, ship lean, and be testable in
isolation. The workspace layout must:

- Separate the pure-logic crate from the CLI and host adapters
  so a contributor can edit `plugin3-core` without rebuilding
  `plugin3-cli`.
- Reuse the Stratum `OffloadStore` trait shape without
  creating a hard dependency on Stratum's source tree (the two
  plugins are released independently).
- Keep the binary under 8 MB (mirrors Stratum ADR-0018).

## Decision

### Workspace Cargo.toml

```toml
# Cargo.toml (workspace root)

[workspace]
resolver = "2"
members = [
    "crates/plugin3-core",
    "crates/plugin3-cli",
    "crates/plugin3-hosts",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
# ponytail: kept in lockstep with rust-toolchain.toml below. The
# transitive dep tree (`toml_edit 0.22` -> `indexmap 2.14` ->
# edition2024) forced the bump from the earlier 1.75 draft to 1.85.
rust-version = "1.85"
license = "MIT OR Apache-2.0"
# ponytail: repository/authors are absent — fill in when the
# public repo is created.
# repository = "https://github.com/kirkforge/plugin3"
# authors = ["KirkForge"]

[workspace.dependencies]
# Internal
plugin3-core = { path = "crates/plugin3-core", version = "0.1.0" }
plugin3-hosts = { path = "crates/plugin3-hosts", version = "0.1.0" }

# External — lean MVP set, no optionals wired (see ADR-0017).
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
clap = { version = "4", features = ["derive", "env"] }
thiserror = "1"
directories = "5"
tempfile = "3"
chrono = { version = "0.4", features = ["serde"] }
blake3 = "1"

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = "symbols"
debug = false
panic = "abort"

[profile.dev]
opt-level = 0
debug = true
incremental = true

[profile.ci]
inherits = "dev"
debug = "line-tables-only"
incremental = false
```

### Crate layout

```
crates/
├── plugin3-core/         # pure logic — transforms, budget, store
│   └── src/
│       ├── lib.rs
│       ├── slicing.rs    # SlicingTransform trait + impls
│       ├── compaction.rs # CompactionTransform trait + impls
│       ├── budget.rs     # TokenBudget, three-state guard
│       ├── detector.rs   # Tool output detection
│       ├── orchestrator.rs # Parallel slicing runner
│       ├── store.rs      # OffloadStore (ADR-0004)
│       ├── cost.rs       # usage.jsonl emission
│       ├── report.rs     # cost report aggregation
│       ├── atomic_write.rs # tmpfs-safe atomic file writes
│       ├── error.rs      # TransformError + thiserror impls
│       ├── paths.rs      # XDG base-directory resolution
│       └── text.rs       # shared text utilities
├── plugin3-cli/          # clap-derive CLI, hook handlers
│   └── src/
│       ├── main.rs       # CLI entrypoint; clap `Cli` struct, subcommand impls
│       ├── precedence.rs # CLI > env > file > default chain (ADR-0015)
│       ├── exit.rs       # sysexits exit codes
│       ├── hooks/
│       │   └── mod.rs    # PostToolUse / UserPromptSubmit / PreCompact
│       └── commands/
│           ├── mod.rs
│           ├── budget.rs
│           ├── config.rs
│           └── report.rs
├── plugin3-hosts/        # per-host output shims (ADR-0013)
│   └── src/
│       ├── lib.rs
│       ├── canonical.rs  # shared canonical payload + response types
│       ├── claude_code.rs
│       ├── cursor.rs
│       └── aider.rs
```

ponytail: the `hooks/` split (one file per hook) is deferred —
all three handlers live in `hooks/mod.rs` until a contributor
finds a reason to split. Likewise `args.rs` and `state.rs` are
folded into `main.rs`. The drift test in
`tests/build_spec_drift.rs` asserts the actual file set
matches this layout so a contributor who re-splits files
without updating the ADR surfaces here.

### Crate responsibilities

| Crate | Responsibility | Public API |
|-------|----------------|------------|
| `plugin3-core` | Pure logic: transforms, budget, store, orchestrator. No I/O outside the OffloadStore. | `SlicingTransform`, `CompactionTransform`, `TokenBudget`, `OffloadStore`, `slice_orchestrator`, `emit_usage` |
| `plugin3-cli` | clap binary, hook handlers, XDG state, syscalls. | `main()` |
| `plugin3-hosts` | Host detection and canonical payload types. Per-host output shims (`claude_code`, `cursor`, `aider`) are stubs today; the CLI's hook handlers consume the canonical payloads directly (ADR-0013). | `Host`, `detect_host`, `PostToolUsePayload`, `UserPromptSubmitPayload`, `PreCompactPayload` |

### Independence from Stratum

Plugin3 does not depend on Stratum's source tree. The
`OffloadStore` trait is duplicated in `plugin3-core` (ADR-0004
documents the byte-compatibility markers). A breaking change
to Stratum's `OffloadStore` is caught by the byte-compat drift
test (ADR-0016); the fix is a coordinated release.

This is a deliberate trade. Two plugins with duplicated traits
is cheaper than a hard workspace dependency that ties their
release cadence.

### Toolchain pin

```toml
# rust-toolchain.toml
[toolchain]
channel = "1.85.0"
components = ["rustfmt", "clippy", "rust-src"]
profile = "minimal"
```

The pin is shared with `Cargo.toml`'s `rust-version` (above)
and with ADR-0017. The earlier draft specified `1.75.0`; the
transitive dep tree (`toml_edit 0.22` -> `indexmap 2.14` ->
edition2024) forced the bump. A contributor who reverts the
example to `1.75.0` documents a toolchain that won't build
the workspace — drift caught in
`tests/build_spec_drift.rs::adr_0002_toolchain_block_uses_1_85`.

## Consequences

Negative first:

- The workspace is larger than a single crate. Three crates is
  one more than two. The trade is that `plugin3-core` compiles
  in ~30 s while `plugin3-cli` rebuilds on every CLI change.
- `OffloadStore` is duplicated across Plugin2 and Plugin3. A
  contributor who fixes a bug in Stratum's store must also fix
  it in Plugin3's store. The drift test (ADR-0016) catches
  the case where this is forgotten.

Positive:

- Pure logic (`plugin3-core`) compiles fast and tests fast.
- `plugin3-cli` and `plugin3-hosts` are swappable. A
  contributor adding a new host adapter touches only
  `plugin3-hosts`.
- The release profile produces a binary under 8 MB.

## Implementation notes

ponytail: the `clippy.toml` file is **absent** in this
workspace. The earlier draft prescribed a `disallowed-methods`
list forbidding `std::panic::panic` (mirroring Stratum's
ADR-0018). The MVP ships without that file: panic discipline
is enforced by code review and the `cargo test --workspace`
run, not by a clippy lint. The drift test
`tests/build_spec_drift.rs::clippy_toml_absence_is_documented_spec_state`
pins the absence, so a contributor who adds the file back
without first updating the spec surfaces here for review.

The `.cargo/config.toml` aliases (kept in place, not
aspirational):

```toml
# .cargo/config.toml
[alias]
bloat = "bloat --release --crates"
```

ponytail: only the `bloat` alias is wired today — there is
no `xtask` binary in the workspace. The earlier draft
prescribed an `xtask = "run --bin xtask --"` alias for a
planned but unshipped auto-generation tool; that work was
deferred (ADR-0017 § Implementation notes) and the alias
was removed from `.cargo/config.toml`. Adding `xtask` is a
future ADR with a generator-fixture rationale; update both
the ADR and the drift test together.

The README's "Building from source" section mirrors Stratum's
four-command entry point set.