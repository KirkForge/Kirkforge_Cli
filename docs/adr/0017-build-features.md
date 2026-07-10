# ADR-0017: Build profile and feature gating discipline

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

A plugin binary that ships at 30 MB when it could ship at
5 MB is wasteful. A binary that takes 8 seconds to compile
from scratch is a tax on every contributor. A test suite
that produces different output in debug vs release is a
maintenance burden.

Plugin3's build configuration mirrors Stratum ADR-0018 with
one delta: Plugin3 does not ship a `yaml` feature gate
(TOML only).

## Decision

### Workspace Cargo.toml

```toml
# Cargo.toml (workspace root)

[workspace]
resolver = "2"
members = [
    "crates/plugin3-core",
    "crates/plugin3-hosts",
    "crates/plugin3-cli",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.85"
license = "MIT OR Apache-2.0"
# ponytail: repository/authors are absent — fill in when the public
# repo is created.
# repository = "https://github.com/kirkforge/plugin3"
# authors = ["KirkForge"]

[workspace.dependencies]
# Internal
plugin3-core = { path = "crates/plugin3-core", version = "0.1.0" }
plugin3-hosts = { path = "crates/plugin3-hosts", version = "0.1.0" }

# External
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

# ponytail: CI profile (ADR-0017 § Reproducible builds).
# `incremental = false` pairs with `CARGO_INCREMENTAL=0` so the
# build is byte-for-byte reproducible across runs. `line-tables-only`
# debug info keeps stack traces readable while shedding the bulk of
# the per-line variable info that bloats the CI artefact.
[profile.ci]
inherits = "dev"
debug = "line-tables-only"
incremental = false
```

No `serde_yaml` (TOML only). The earlier draft listed
`tracing`/`rayon`/`rusqlite`/`parking_lot`/`fs2`/`uuid`/
`walkdir`/`clap_complete`/`proptest`/`assert_cmd`/
`predicates`/`tracing-subscriber`/`anyhow` — each of those
was aspirational; the lean MVP shipped without them. Adding
any of them is its own ADR with a binary-size and
compile-time rationale; the drift test
`workspace_dependencies_match_impl` in
`tests/build_spec_drift.rs` pins the absence so a
contributor who re-pastes one of them into the ADR surfaces
here.

### Toolchain pin

`rust-toolchain.toml` at the workspace root pins the
toolchain:

```toml
[toolchain]
channel = "1.85.0"
components = ["rustfmt", "clippy", "rust-src"]
profile = "minimal"
```

The pin is shared with the rest of the workspace MSRV. A
contributor who upgrades the toolchain does so deliberately,
in a separate commit, with a CI green-light. The earlier
ADR draft specified `1.75.0`; the transitive dep tree
(`toml_edit 0.22` → `indexmap 2.14` → edition2024) forced
the bump to `1.85.0`.

### Feature gates (none today)

The MVP has no `[features]` section in any `Cargo.toml`. The
default binary builds with the lean workspace dep set above
(`InMemoryOffloadStore` + `FileOffloadStore` per ADR-0004);
no SQLite backend is wired. Adding a `sqlite` feature is a
future ADR with a `rusqlite = { version = "0.31", features =
["bundled"], optional = true }` dep — until then the
`store.rs` module exposes only the in-memory and file
backends. The drift test in `tests/build_spec_drift.rs`
asserts no `[features]` section exists today, so a
contributor who pastes one back surfaces here for review.

### Reproducible builds

CI sets:

```yaml
env:
  CARGO_INCREMENTAL: "0"
  CARGO_PROFILE_DEV_DEBUG: "line-tables-only"
  SOURCE_DATE_EPOCH: "0"
  RUSTFLAGS: "--remap-path-prefix $HOME=/build"
```

`SOURCE_DATE_EPOCH=0` makes every `chrono::Utc::now()` call
deterministic. `RUSTFLAGS=--remap-path-prefix` ensures the
binary does not embed the contributor's home directory in
debug info.

### Size budget

The release binary target is <8 MB. CI measures the binary
size and fails if it exceeds the budget:

```bash
SIZE=$(stat -c%s target/release/plugin3)
if [ "$SIZE" -gt 8388608 ]; then
    echo "plugin3: binary size $SIZE exceeds 8MB budget"
    exit 1
fi
```

The budget is enforced, not aspirational.

### Compile-time budget

CI measures `cargo build --release` wall time and fails if
it exceeds 5 minutes:

```bash
time cargo build --release
```

A 5-minute compile is the threshold for "this is taking too
long, find a faster path". A 10-minute compile is a
contributor experience failure.

### What this forbids

- Unconditional `tokio` dependency. The MVP is synchronous
  (ADR-0001).
- Unconditional `serde_json::Value` parsing in hot paths.
  Strongly-typed structs are preferred.
- `unsafe` code without a `// SAFETY:` comment and a test
  that exercises the unsafe path.
- Build scripts (`build.rs`) that download assets at build
  time.
- `serde_yaml` or any YAML parser. Plugin3 is TOML-only.

### What this allows

- A single `tokio` runtime in a future ADR. Not now.
- Native dependencies via the `bundled` feature (`rusqlite`).
- `#[cfg(feature = "...")]` gating throughout `plugin3-core`
  to keep the minimum binary small.

## Consequences

Negative first:

- The 8 MB binary budget is tight. Adding `tokio` or a heavy
  crypto crate would blow it. A future contributor who needs
  one of those must negotiate the budget.
- `SOURCE_DATE_EPOCH=0` makes `chrono::Utc::now()` return the
  Unix epoch. Tests that assert on the current time must
  inject a clock.

Positive:

- The release binary is small, fast, and reproducible.
- The minimum-feature binary works on the most constrained
  target.
- The toolchain pin means a contributor's local toolchain
  cannot silently drift from CI's.

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
Adding the lint is a future ADR with a CI-noise-vs-discipline
trade-off rationale.

The `.cargo/config.toml` at the workspace root configures
build-time aliases:

```toml
# .cargo/config.toml
[alias]
bloat = "bloat --release --crates"
```

The README's "Building" section documents:

```bash
# Default build (ADR-0017)
cargo build --release

# Minimum build (no default features)
cargo build --release --no-default-features

# Run tests
cargo test --workspace

# Self-check
cargo run --release --bin plugin3 -- self-check
```

The four commands are the contributor's entry points.

ponytail: the earlier draft specified an `xtask` alias
(`xtask = "run --bin xtask --"`) and a README feature
matrix auto-generated by an `xtask`-side feature-matrix
generator. Neither exists — the MVP has no `xtask` binary,
and the README has no feature matrix because no crate ships
a `[features]` section today (see § Feature gates). Adding
either is a future ADR with a generator-fixture rationale.

CI's `target/` directory is cached between runs:

```yaml
- uses: Swatinem/rust-cache@v2
  with:
    workspaces: "."
```

The cache is keyed on the toolchain pin and the `Cargo.lock`
hash. A change to either invalidates the cache.