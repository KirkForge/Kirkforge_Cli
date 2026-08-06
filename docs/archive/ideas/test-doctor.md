# Idea: `kirkforge-testdoctor` — test-performance doctor for Rust workspaces

- **Status:** Proposal
- **Date:** 2026-07-21
- **Author:** kirk
- **See also:** [ADR-0029](../adr/0029-test-partitioning.md), [ADR-0016](../adr/0016-test-strategy.md)

## Motivation

KirkForge-Cli has 2772+ tests. Local runtime is ~98s, but a single CI job
takes 7-8 minutes because every job re-runs the full workspace suite:

- `quality` runs `cargo test --locked --workspace --no-fail-fast` (lib + ints).
- `coverage` runs `cargo tarpaulin --workspace` — the *entire* suite again,
  this time under LLVM instrumentation (2-3x slower than plain `cargo test`).
- `windows` runs `cargo test --workspace` again.
- `integration` runs `cargo test --test integration_test -- --include-ignored`
  (the slow Ollama-spawning tests) plus `--test smoke_test`.

The wall-clock budget of a PR is dominated by **re-running the same suite
under different harnesses**, not by the cost of any individual test. The
problem is not unique to KirkForge-Cli — any large Rust workspace that
adopts tarpaulin hits the same cliff.

This is the Rust equivalent of PicoSentry's
`scripts/test_doctor.py` — a diagnostic that knows which tests are slow,
which are subprocess-heavy, and which can safely be skipped per CI stage.

## The problem, in detail

1. **`cargo test --workspace` builds every crate and runs every test**,
   including slow integration tests that spawn `cargo`, `docker`, or
   `ollama` subprocesses. On a 14-crate workspace the build alone is
   ~40s; the test pass is another ~60s.
2. **`cargo-tarpaulin --workspace` reruns the entire suite under LLVM
   instrumentation**, which is 2-3x slower than the uninstrumented run.
   Tarpaulin also serialises more aggressively than `cargo test`, so
   parallelism is lost on top.
3. **Tests that spawn subprocesses are slow and serialised.** A test
   that calls `cargo build` inside itself takes 2-10s; tarpaulin's
   tracing engine makes that worse.
4. **Tests with `tokio::time::sleep` / `std::thread::sleep` waste wall
   time** without exercising code.
5. **No partitioning.** Every CI job runs the full suite, so the
   marginal cost of adding a job is the full suite cost — not the
   incremental cost of what the job actually checks.

## The solution: a `kirkforge-testdoctor` crate + CI integration

A standalone binary that:

1. **Profiles** the suite — runs `cargo test --workspace --no-fail-fast`
   and captures per-binary timings (per-test timings require nightly
   `--format json`; the doctor falls back to per-binary totals and
   estimates per-test timings from a single-threaded probe of the slow
   binaries). Outputs `test-profile.json`:

   ```json
   {
     "binaries": [
       {
         "binary": "kirkforge (lib)",
         "suite": "lib",
         "duration_ms": 43140,
         "passed": 1285,
         "failed": 0,
         "ignored": 2
       },
       {
         "binary": "integration_test",
         "suite": "tests/integration_test.rs",
         "duration_ms": 38000,
         "passed": 14,
         "failed": 0,
         "ignored": 3
       }
     ],
     "total_duration_ms": 98000,
     "wall_time_ms": 98000
   }
   ```

2. **Classifies** each binary (and, where per-test data is available,
   each test) as:
   - `fast` (< 1s per test, or binary total < 5s) — pure logic, unit
     tests. Run on every PR.
   - `medium` (1-10s per test, or binary total 5-30s) — tests with I/O,
     temp files, short waits. Run on PRs and in coverage.
   - `slow` (> 10s per test, or binary total > 30s) — subprocess
     spawners, network, long sleeps. Skip in coverage; gate behind
     `--ignored` or the integration job.
   - `ignored` — tests already marked `#[ignore]`. Run only in the
     dedicated `--ignored` job.

3. **Partitions** — emits three suite definitions:

   - `fast-suite.json` — `fast` + `medium` lib tests. Target: < 60s.
     Run on every PR.
   - `full-suite.json` — all non-ignored tests across the workspace.
     Run on merge to `main` / `dev`.
   - `coverage-suite.json` — `fast` + `medium` only, lib targets.
     Used by the tarpaulin job to skip the slow integration tests.

   Each suite is a JSON manifest with the exact `cargo test` / `cargo
   nextest` invocation plus the per-binary allow-list, so CI can run
   `kirkforge-testdoctor run --suite fast` without re-deriving the
   partition.

4. **Suggests** fixes for `slow` entries:
   - "Mark `#[ignore = \"slow: spawns cargo\"]` and move to the
     `--ignored` suite."
   - "Use `tokio::time::pause` instead of `tokio::time::sleep` — the
     runtime advances virtual time instantly."
   - "Mock the subprocess (see `wiremock` for HTTP, or factor the
     command into a trait the test can stub)."
   - "Move to `tests/` integration test directory so it runs in its
     own binary and is not re-instrumented by tarpaulin's `--lib`."

5. **CI integration** — a `scripts/test-doctor.sh` (or
   `cargo run --bin kirkforge-testdoctor -- run --suite <suite>`) that:
   - On **PR**: runs the fast suite. Prefers `cargo nextest run --lib`
     (2-3x faster via better parallelism) when nextest is installed;
     falls back to `cargo test --lib` otherwise.
   - On **merge to main**: runs the full suite
     (`cargo test --workspace`).
   - **Coverage job**: runs `cargo tarpaulin --lib` (unit tests only —
     not `--workspace`, not `--tests`). This skips integration tests
     that spawn subprocesses, which are both slow and poorly served
     by line-coverage anyway.

## Key insight

The biggest win is **not** the profiling. The profiling is a
diagnostic that tells you *why* the suite is slow. The biggest win
is **partitioning the CI jobs** so the coverage job does not rerun
the full suite and the PR job only runs the fast suite:

| Lever | Win | Cost |
|-------|-----|------|
| `cargo tarpaulin --lib` instead of `--workspace` | coverage 4min → ~2min | none — integration tests are already excluded from coverage gate |
| `cargo test --lib` on PRs instead of `--workspace` | quality 7min → ~3min on PRs | need a separate main-merge job that runs `--workspace` |
| `cargo nextest run` instead of `cargo test` | 2-3x on top of the above | one extra `taiki-e/install-action` step |
| Mark slow tests `#[ignore]` + dedicated `--ignored` job | removes 30-60s from the hot path | requires per-test annotation |

The doctor automates the discovery and the annotation suggestions; the
partition config is checked in so CI does not depend on running the
doctor first.

## Reusability

The crate is workspace-agnostic. It shells out to `cargo test` and
parses the standard text output (`test result: ok. N passed; ...
finished in X.XXs`), so it works on any Rust workspace without
codebase-specific knowledge. The KirkForge-Cli-specific parts (which
tests spawn `ollama`, which spawn `cargo`) live in the suggestions
database, which is a JSON file the doctor loads at runtime — not
compiled in.

## Out of scope (for the prototype)

- Per-test timing on stable Rust (requires nightly `--format json`).
  The prototype uses per-binary totals + a single-threaded probe of
  the slowest binaries.
- Automatic `#[ignore]` rewriting of source files. The doctor
  *suggests* the edit; the human applies it.
- Cross-workspace profiling (e.g. profiling a dependency's test
  suite). The doctor profiles the workspace it is run from.
- A tui. The doctor is a CLI; the report is JSON + a text summary.

## Next steps

1. Prototype the crate at `crates/kirkforge-testdoctor/` (not a
   workspace member yet — standalone build).
2. Apply the immediate CI wins to `.github/workflows/ci.yml`
   (tarpaulin `--lib`, `--test --lib` on PRs, nextest install).
3. Record the partitioning decision in
   [ADR-0029](../adr/0029-test-partitioning.md).
4. Once the prototype is exercised on a few real PRs, promote it to a
   workspace member and wire `scripts/test-doctor.sh` into CI.