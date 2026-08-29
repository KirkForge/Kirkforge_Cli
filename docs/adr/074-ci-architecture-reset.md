# ADR-074: CI architecture reset — PR / merge / nightly tiers

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-08-15
- **Related:** [ADR-0029](./0029-test-partitioning.md) (test partitioning),
  [ADR-0017](./0017-build-features.md) (e2e-tests feature gate),
  [ADR-0016](./0016-test-strategy.md) (test categories)

## Context

The CI had accumulated historical fixes on top of historical fixes. The
workflow comments themselves told the story: WO 28.10, WO 28.11 R2, "historic
Windows-flake source", "old workflows", "disabled workflows", commit
`4028424`. Each problem was patched locally but the workflow architecture was
never subsequently simplified. The result was a tech-debt nightmare:

- **One giant workflow** doing repository validation, formatting, unit tests,
  workspace tests, clippy, typecheck, Windows compatibility, Windows release
  compilation, E2E, Ollama integration, dependency audit, and coverage — all
  with slightly different historical rules.
- **No concurrency cancellation.** Three rapid pushes spawned three complete
  CI runs; the runners kept working on superseded commits.
- **Artificial `needs:` chains.** `fmt → quality → e2e/coverage/integration`
  created a serial critical path even though E2E/coverage/integration depend
  on the source checkout, not on a separate test job succeeding.
- **Redundant compile gates.** `cargo check` ran after `cargo clippy` on the
  same scope — clippy with `-D warnings` already fails on any compile error,
  so `cargo check` only re-ran the build without adding coverage.
- **Wrong tier for Ollama and coverage.** Real-model integration tests
  (network, model download, GPU variance) and coverage instrumentation
  (rebuilds + re-runs the workspace) sat beside ordinary CI as though they had
  the same reliability/cost characteristics.
- **Inline nextest config** (`--config 'profile.default.timeout-period="60s"'`)
  duplicated across jobs, even though `.config/nextest.toml` already defined
  `ci-fast`/`ci-full`/`integration`/`e2e` profiles.
- **`--all-targets` on every PR** pulled benches/examples into compilation
  even when the goal was library correctness.
- **`--no-fail-fast` on the PR fast path** continued after the first failures,
  counter to the "tell me quickly that my PR is broken" goal.
- **`fmt` job name** described only one of four concerns (conflict markers +
  TOML schema + artifact consistency + rustfmt).
- **Comments as incident changelog** instead of current-architecture
  documentation.

A prior series of workorders (WO 33.4 removed redundant `cargo check`; WO 33.6
added changed-package selection; the 3-workflow split landed earlier) began the
reset. This ADR pins the completed architecture so the workflow comments can
document the *current* model rather than the incident history.

## Decision

Three trigger-scoped workflows, each with a distinct purpose and schedule:

### PR gate (`ci-pr.yml`, `on: pull_request`)

```
static ──→ fast-tests  (ci-fast profile, fail-fast, changed-packages only)
       ──→ clippy      (--lib --bins, fail-fast)
       ──→ targeted-integration  (if Rust changes only)
```

- **`static`** (renamed from `fmt`): conflict markers + TOML schema + artifact
  consistency + rustfmt. Cheap gate that everything else depends on. Runs
  repo-file linting BEFORE installing Rust.
- **`fast-tests`**: `cargo nextest run --profile ci-fast` on changed packages
  only (via `scripts/changed-packages.sh`). `ci-fast` profile = `kind(lib)`,
  `fail-fast = true`, 60s slow-timeout. Skipped entirely on docs-only changes.
- **`clippy`**: `cargo clippy --lib --bins` (NOT `--all-targets`) — skip
  test-target/bench/example compilation for PR speed. `--features e2e-tests`
  keeps the gated e2e crate under the lint gate. Clippy IS the compile gate;
  no separate `cargo check`.
- `concurrency: { cancel-in-progress: true }` — a new push supersedes the
  previous run.

### Post-merge gate (`ci-merge.yml`, `on: push: [main, dev]`)

```
static ──→ full-tests   (ci-full profile, --no-fail-fast, + doctests)
       ──→ clippy       (--all-targets, full)
       ──→ windows      (tests only, NO release build)
       ──→ e2e          (--profile e2e, --features e2e-tests, --no-fail-fast)
       ──→ platform-build (release compile validation — in nightly, not here)
```

- All jobs are **parallel siblings** depending on `static` only — no artificial
  `needs:` chain between test jobs. They depend on the source checkout, not on
  each other's test success.
- **`clippy`**: `--all-targets` (full validation on merge, vs `--lib --bins` on
  PR).
- **`full-tests`**: `cargo nextest run --profile ci-full --workspace
  --no-fail-fast` + doctests under a hard wall-clock cap. `ci-full` profile =
  `fail-fast = false`, 120s slow-timeout.
- **`windows`**: tests only via `cargo nextest run --profile ci-full
  --workspace --no-fail-fast`. NO release build — release compile validation
  lives in the nightly `release-build` job / tag-triggered `release.yml`.
- **`e2e`**: `cargo nextest run --profile e2e --features e2e-tests
  --no-fail-fast`. The `e2e` profile = `binary(e2e)`, 600s slow-timeout.
- **No Ollama, no coverage** on merge. Both moved to nightly.
- `concurrency: { cancel-in-progress: true }`.

### Nightly (`ci-nightly.yml`, `on: schedule + workflow_dispatch`)

```
coverage  ·  ollama  ·  e2e-exhaustive  ·  audit  ·  release-build (linux+windows)
```

- **`coverage`**: `cargo llvm-cov --workspace` + regression gate.
- **`ollama`**: real-model integration tests against a live Ollama instance
  (network, model download, GPU variance — wrong shape for PR/merge CI).
- **`e2e-exhaustive`**: full e2e suite with `--profile e2e` so a flaky e2e
  failure is observed without blocking merge.
- **`audit`**: `cargo audit` (critical/high/unsound block; lower = warning).
- **`release-build`**: release profile compile validation on linux + windows
  (the tag-triggered `release.yml` does the actual publish).
- `concurrency: { cancel-in-progress: false }` — a partial nightly run is not
  superseded; we want the full report.

### Cross-cutting policies

- **Nextest profiles are declarative.** Workflows say `--profile ci-fast`, not
  inline `--config 'profile.default.timeout-period=...'`. Policy lives in
  `.config/nextest.toml`.
- **PR fail-fast, merge/nightly collect-all.** `ci-fast` profile has
  `fail-fast = true`; `ci-full`/`integration`/`e2e` have `fail-fast = false`.
- **Changed-package selection.** `scripts/changed-packages.sh` maps
  `git diff <base>..HEAD` to affected cargo packages (+ reverse-dep closure);
  ci-pr gates clippy + fast-tests on the output. Docs-only changes skip Rust
  CI entirely.
- **No redundant `cargo check`.** Clippy with `-D warnings` is the compile
  gate; a same-scope `cargo check` only re-runs the build.

## Consequences

- **Developer feedback latency drops.** PR critical path is `static →
  max(fast-tests, clippy)` with changed-package selection, not a serial chain
  through a monolithic `quality` job.
- **CI compute on superseded commits is cancelled** (concurrency group per
  PR/ref).
- **Ollama/coverage failures no longer block PRs or merges** — they surface in
  the nightly report.
- **Workflow comments document the current architecture**, not WO incident
  history. Historical rationale lives here (this ADR).
- **`.config/nextest.toml` is the single source of truth** for test timeout /
  fail-fast / filter policy. Changing a timeout no longer requires editing
  YAML in multiple jobs.
- **Release compile validation is decoupled from Windows test validation.**
  The Windows merge job tests; the nightly `release-build` job validates the
  release profile compiles.

## Amendment (2026-08-29) — timeout retune + nightly job growth

Two numeric drifts since this ADR landed:

- **Timeouts (WO 40.3)**: the slow-timeouts cited above were retuned —
  `ci-fast` 60s → **30s**, `ci-full` 120s → **60s**, `e2e` 600s →
  **300s** (`integration` 120s and `nightly` 600s are as listed).
  `.config/nextest.toml` remains the single source of truth.
- **Nightly jobs (5 → 7)**: `subprocess-lifecycle` (WO 48.22 — the two
  `#[ignore]`d timeout/reap tests) and `mutants` (informational
  mutation-testing report) were added to the five listed above.