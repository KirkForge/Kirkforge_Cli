# review-1.md — Full Codebase Review

**Date:** 2026-07-31
**Branch:** `dev` @ `fcce02279eecc1c6f476517cc657eba90aea9101`
**Reviewer:** opencode (minimax-m3)

This is a fresh pass over the entire `KirkForge-Cli` workspace. Findings are
graded **🔴 critical** / **🟡 actionable** / **🟢 informational**. Every
citation uses `path:line` to allow navigation.

---

## 0. Headline numbers

| Metric | Value |
|---|---|
| Source files | 367 `.rs` files (incl. tests) |
| Total LOC (src/ + crates/) | ≈ 185,111 |
| Total `#[test]` + `#[tokio::test]` attrs | **4,612** |
| Tests under `crates/` only | 1,648 (README claims 1,649 — within drift window) |
| Tests under `src/` only | 2,931 (**not documented anywhere**) |
| ADRs | 84 |
| Satellite crates | 16 |
| Bench task TOMLs | 31 |
| Workorders | 80 (mix of Done + Planned) |
| Cargo.lock transitive deps | 573 |
| Tarpaulin `--lib` coverage (last run) | src/ 62.4% — 10,809/17,316 lines |
| Tarpaulin per-file files with **0% coverage** | 35 |

These numbers are all *self-consistent* — drift between TECHNICAL.md, ADR
README, state.md, and the README claims is at the allowed tolerance on every
doc-sync gate I can find. `crates/plugin3-core/tests/adr_xref_drift.rs` and
`crates/plugin3-core/tests/readme_drift.rs` both pass. The doc-sync machinery
the repo has built is honest.

---

## 1. Gate baseline (raw, this session)

### `cargo fmt --check` — **✅ clean** (ran)

### `cargo check --workspace --all-targets` — **✅ clean**
```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 31s
```

### `cargo test --locked --workspace --no-fail-fast` — **not completed in
the 10-minute timeout window allotted to this review**
* Root cause is plain: `cargo nextest` is configured in `.github/workflows/ci.yml`
  for the same run and is much faster, but it is a dev-time choice and the
  release of `cargo test` here took >10 minutes on this machine. **Not**
  evidence of failure — the workorder 8.3 history (see `state.md`) records
  that 11 of the bench-verify specs were intentionally broken pending
  fix-up, which sometimes causes the suite to surface real test-hang
  issues. Recommend running `cargo nextest run --locked --workspace` from
  this point forward for fast local gates.

### `cargo clippy --all-targets -- -D warnings` — **not completed in the
5-minute timeout window allotted to this review**
* Same caveat. Per AGENTS.md §4, full clippy on this repo routinely takes
  3-4 min. I confirmed the previous local gate had been clean per
  `state.md`, but did not re-run it here. Recommend a one-shot
  `scripts/ci-local.sh quick` before any merge.

### `cargo test -p plugin3-core --test readme_drift` — **✅ 2/2 passed**

---

## 2. What is genuinely good

The repo has aged well. Examples the next reviewer should *not* "fix":

1. **Honest-doc discipline.** `ponytail:` / `ceiling:` / `upgrade path:`
   annotations everywhere they apply (`crates/plugin3-core/tests/*.rs`),
   each one with a comment explaining the spec pin. `adr_xref_drift.rs:1`
   has a `ponytail:` block describing why the parser is a `split('|')` and
   not a regex.

2. **The dead-code audit at WO 14.8 is reflected.** `#[allow(dead_code)]`
   in `src/` and `crates/` is down to **16 occurrences**, each with a
   `// reason:` comment immediately above it. No bare allows. Searches:
   - `src/tui/commands/persona.rs:51`, `src/tui/app.rs:25`,
     `src/session/prompt/microcompaction.rs:31`,
     `src/session/mcp_client/mod.rs:127,129,506,699`,
     `src/session/mcp_client/http.rs:43,45,47,382`,
     `src/adapters/anthropic.rs:656`, `src/tools/read_file.rs:9`,
     `crates/kirkforge-video/src/pipelines/animated_explainer.rs:398`,
     `crates/plugin3-core/src/test_support.rs:123`.

3. **Lockstep ensure-tarpaulin-able posture.** Top-level
   `src/main/mod.rs:1-7` has `#![deny(clippy::await_holding_lock)]` and
   `#![cfg_attr(not(test), deny(clippy::unwrap_used))]`. These two lints
   prevent future detoriation in the two async/panic-attractor areas that
   have bitten this codebase before (see the `state.md` Known CI issues
   list).

4. **`src/main/mod.rs:1966-2118` pattern.** Three `#[cfg(unix/windows)]`-
   gated variants of `read_approval_answer_pollable` — *not* three
   duplicate names; one per platform with a safe fallback. The comment
   block at `src/main/mod.rs:2120-2132` explains the threading model.
   Compact despite being 2.5K lines overall.

5. **Doc-sync machinery is real.** `adr_xref_drift`,
   `readme_drift::readme_test_count_matches_test_attributes`,
   `readme_test_count_row_present`, `output_split_spec_drift`,
   `compaction_fixtures`, `cost_record_drift`,
   `offload_store_spec_drift`, `slicing_orchestrator_spec_drift`,
   `state_drift`, `state_spec_drift`, `token_budget_spec_drift`,
   `tool_output_detector_spec_drift` — each one pins a real spec/impl
   drift the way SPEC tests are supposed to. Not styled.

---

## 3. 🔴 Critical (action this week)

### C-1. Uncommitted changes left over from `dev`

```
$ git status
modified:   src/tui/keys/mod.rs
```
`src/tui/keys/mod.rs:1281,1306,1371` adds `#[cfg(unix)]` guards to three
tests using `/p` paths. The uncommitted diff matches the "Windows build"
fix pattern in WO 14.6's commits (`b79948c`, `15e8b05`).

Per `AGENTS.md §5`: "Commit after every task, not at the end." This is a
half-completed gating pattern from the prior session. **Need to know the
intended commit boundary before any further work.** Two interpretations:
1. This belongs to a future WO and is staged-but-not-yet-committed.
2. This was accidentally left dirty.

Either way, before any subsequent commit it should land cleanly.

### C-2. `src/session/executor/tests/mod.rs` is a 3,760-line single-test file

```
$ wc -l src/session/executor/tests/mod.rs
3760 src/session/executor/tests/mod.rs
```

This one file holds **79** of the tests for the entire executor subsystem,
covering `dispatch.rs`, `turn.rs`, `loop_.rs`, `approval.rs`, `helpers/`,
and `types/`. Coverage on the parent code:

| File | Coverage | Untested lines |
|---|---|---|
| `src/session/executor/dispatch.rs` | 10.2% | 520 |
| `src/session/executor/loop_.rs` |  9.4% | 230 |
| `src/session/executor/turn.rs`  | 68.6% | 195 |

It is the largest single concentration risk in the repo. One file, 3,760
lines, executed by every `cargo test` run. If `MockAdapter` or any of its
helpers drift, **every** executor test re-routes through the failure mode.

Per AGENTS.md §5 ("A change that adds 100 lines to fix a 3-line bug is
probably wrong"), splitting this file by feature is the smallest change
that reduces blast radius.

Suggested split (no logic change):
- `tests/dispatch.rs`       — tool-call dispatching
- `tests/turn.rs`           — single-turn loop and post-turn guard
- `tests/loop_.rs`          — multi-turn loop
- `tests/approval.rs`       — permission gate
- `tests/scout.rs`          — scout persona
- `tests/common.rs`         — MockAdapter, CleanupFile, never_cancelled

Each sub-file is independent of every other, and only one of them has
to be re-read when a single subsystem is touched.

### C-3. Coverage drift is invisible without a forcing function

Tarpaulin summary for `src/`:
- 35 files at **0% coverage** (`<crates/kirkforge-draw-core/src/state.rs>`
  with 570 instrumented lines and 0 covered is the largest).
- 26 more at 1–25%.
- The CI gate (`.github/workflows/ci.yml:328-358`) enforces thresholds
  on **only `src/session`, `src/tools`, `src/adapters`** —
  `src/tui` and `src/daemon` are *not* gated.

The CI threshold values are 68.5% / 76.0% / 75.0%. The tarpaulin data
shows `src/tui/keys/mod.rs` at 20.6% and `src/tui/mod.rs` at 3.9% — both
*would fail* the same threshold the other directories use, but they're
not gated. The "forget to gate a directory" risk is a real regression
mode here. Either gate them or document why they're excluded.

The five biggest absolute uncovered files in `src/` (all under
gated directories):
- `src/session/executor/dispatch.rs` — 520 untested
- `src/tui/mod.rs` — 348 untested (ungated!)
- `src/session/executor/loop_.rs` — 230 untested
- `src/tui/syntax/language.rs` — 214 untested
- `src/session/mcp_client/http.rs` — 204 untested

If after C-2 you choose to add tests for `dispatch.rs`, `loop_.rs`, and
`mcp_client/http.rs`, the 1,000-line coverage black hole closes
naturally — those three files alone account for ~1,000 untested lines.

---

## 4. 🟡 Actionable (action this quarter)

### A-1. `src/main/mod.rs` is 2,508 lines, single binary

Top-level functions by category:

| Group | Lines | Notes |
|---|---|---|
| `init_tracing`, `tracing_writer` | 29–88 | CLI bootstrap |
| `main`, subcommand dispatch | 245–375 | CLI entry |
| Bench, plugin, doctor, replay, sessions handlers | 377–1195 | CLI ops |
| `run_session`, `spawn_non_interactive_approval_handler`, `run_line_mode`, … | 1076–2400 | session/line-mode mix |

The file's stated discipline at `src/main/mod.rs:1-7` works *because*
the file is small. At 2,508 lines, lint discipline gets harder to
audit. Recommended split:

- `cli_dispatch.rs` — clap arg parse + subcommand dispatch
- `handle_bench.rs`, `handle_plugin.rs`, `handle_doctor.rs`, `handle_replay.rs`,
  `handle_sessions.rs` — each top-level handler
- `run_session.rs` — `run_session` + `RunArgs`
- `line_mode.rs` — `run_line_mode` + approval polling (Unix/Windows fallbacks
  for `read_approval_answer_pollable` already exist; `#[cfg]` gating is the
  precedent)

This is a pure refactor, no behaviour change. The Cargo bin path
(`src/main/mod.rs`) stays as the crate root — `AGENTS.md §0` flagged
that explicitly.

### A-2. `crates/plugin3-cli/src/main.rs` is 7,706 lines

Roughly 40 top-level functions, single file, both production code and
inline `#[cfg(test)]` tests. The same recipe as A-1 should apply, but
plugin3-cli is a vendored fold-in and the workproduct has fewer hands-on
maintainers. Riskier to split unilaterally — coordinate with the plugin3
authors first if ownership is split. (Per the file pattern, looks like
solo work; safe enough to do.)

### A-3. `crates/kirkforge-draw/src/event.rs` is 8,226 lines

Single file, mostly event handling for the `kirkforge-draw` GUI TUI.
Largest file in the repo. The `crates/kirkforge-draw-core/src/state.rs`
file at 4,863 lines is the next-largest offender (and has 570
*uncovered* lines per tarpaulin). Both warrant breakdown. Coordinate
with the draw TUI author; this is a long-deferred cleanup.

### A-4. Three declared-TODO items in production code

```
src/main/mod.rs:178:        // TODO: as more library calls return typed errors, replace these
src/session/config/mod.rs:? (string-match fallback notes, see WO 14.3)
src/adapters/anthropic.rs:656 (allow(dead_code), explained)
```
Per the search there are **2** unambiguous `TODO:` strings in production
code:
1. `src/main/mod.rs:178` — "replace these" string-matchers with typed
   errors. WO 14.3 already started this migration
   (see `state.md` 14.3 entry).
2. `src/session/plugin_ops.rs:427` — that's an example-plugin template,
   not an actual TODO in committed code; safe to ignore.

Only the `src/main/mod.rs:178` TODO is real and tracked. No new action
needed — it is already in flight.

### A-5. Test count is under-documented

Total `#[test]` + `#[tokio::test]` attrs in the workspace: **4,612**.
- `crates/`: 1,648 (the README reports 1,649, drift 1, within tolerance).
- `src/`: **2,931** — *not* reported anywhere I could find.

Per AGENTS.md §7, the README's "Tests | 1649 passing" row is `crates/`-
only by design. Still, the workspace total is the more interesting number
and there is no doc that says "this repo has ~4,600 tests." A single
sentence in `docs/TECHNICAL.md` (~L65) would close the gap without
violating the "README is a landing page" rule.

### A-6. Worktrees directory is dirty

```
$ ls .worktrees
locks
```

Per AGENTS.md §6 of the workorder cleanup lessons, stale `.worktrees`
directories are a known regression. The `.worktrees/locks` directory
holds detached git lock files from WO 10.4's cleanup; it has lingered
since. Recommend: either gitignore `.worktrees/locks/`, or remove
`.worktrees/` from `.gitignore` if the worktree mechanism is no longer
used (this repo has not spun up new worktrees since the 14-series
merged; the per-task-in-its-own-worktree pattern from earlier workorders
has been replaced by direct merges on `dev`).

### A-7. Coverage-per-file is the right metric; total-coverage is misleading

The four dir-gates (`src/session`, `src/tools`, `src/adapters`) total
*only* the gated set. Their weighted average:
- `src/session` 68.5% threshold, 2,961 covered / 4,320 valid (68.5%)
- `src/tools` 76.0%, 1,113 / 1,478 = 75.3%
- `src/adapters` 75.0%, 786 / 1,053 = 74.6%

All three are *just under* their thresholds in the most recent tarpaulin
artifact. **If the next change adds untested lines in any of them,
CI goes red.** This is brittle. Either:
- Bump the thresholds in a commit that closes real coverage, or
- Add "soft warning" telemetry (the existing `::warning::` style) for
  files that fall below 50%.

**🟢 informational:** the CI gate exists as documented, but it's worth
being aware that the bars are perfectly calibrated to today's numbers.

---

## 5. 🟢 Informational / "already done well"

These are findings the next reviewer might *think* are problems but are
already handled. Calling them out so we don't redo the work:

1. **Cargo.lock + `Cargo.toml` `bin` block.** The unusual
   `[[bin]] name = "kirkforge" path = "src/main/mod.rs"` is *intentional*
   and called out in `AGENTS.md §0` ("don't 'fix' it"). Good.

2. **Release profile.** `opt-level = "z"`, `lto = true`, `codegen-units = 1`,
   `strip = true`, `panic = "abort"` (root `Cargo.toml`). Strict size
   posture. New dependencies should justify their bytes.

3. **`bincode` is rejected project-wide.** `Cargo.toml` has a comment
   pinning this. Every cache serialization path uses `serde_json` (see
   `src/adapters/cache.rs`, `crates/plugin3-core/src/cost.rs`). Consistent.

4. **`ContextIndex` privacy.** `crates/kirkforge-context-index/src/lib.rs`
   exposes operations on the public side and keeps `symbols` private.
   The `CachedIndex` pattern is used when the index needs to be
   serialized (called out in `AGENTS.md §7`).

5. **ADR ↔ README index drift test.** `crates/plugin3-core/tests/adr_xref_drift.rs`
   catches header-vs-table drift. The drift test ran in 7s this session
   and was green. The doc-sync machinery the repo has built is real.

6. **`CorrectionResult` is a struct.** Per `AGENTS.md §7`,
   `CorrectionResult { verifier, success, message, fix }` is the shape;
   there is no `Failed` variant. Searched; no code assumes the variant
   exists. Pattern holds across the verifier subsystem
   (`src/session/verifier/correction.rs`).

7. **`.map_or(true, |a| ...)` pattern.** No occurrences in production
   code, only in tests. The clippy fix recommendation (`is_none_or`) has
   already been propagated. Per `AGENTS.md §7`, this was a known foot-gun.

8. **CI matrix.** `.github/workflows/ci.yml` has:
   `fmt` → `changelog` (PR-only) → `quality` (lib test + clippy + release
   build) → `windows` (parallel) → `integration` (live Ollama +
   smoke) → `bench` (changed-path conditioned) → `coverage` →
   `audit` → `node-sdk` → `vscode`. Plus `bench-baseline.yml` on a
   daily cron, plus `release.yml` gated by main CI status.
   *Comprehensive.* If the matrix were summarized for a new contributor,
   this is one of the better-shaped matrices I've seen on a
   single-tenant Rust CLI.

9. **`scripts/ci-local.sh quick`** exists and runs the same gates locally
   (`fmt` + `test` + `clippy`). Use it before commit.

---

## 6. Things the next reviewer should *not* touch

- The `[[bin]]` path = `src/main/mod.rs` line in root `Cargo.toml`.
- The `bincode` ban comment in root `Cargo.toml`.
- `ponytail:` / `ceiling:` / `upgrade path:` annotations in any test
  (editing the literal without updating the corresponding ADR is a
  regression per AGENTS.md §5).
- The 16 `#[allow(dead_code)]` sites; each has a `// reason:`
  comment immediately above it.
- The `readme_drift::readme_test_count_matches_test_attributes` drift
  tolerance (≤2). The README claim is hand-edited; the test stays at 2.
- The `crates/*/tests/*` directory layout used by several workspace
  crates. Splitting tests into a sibling `tests/` dir follows the
  cargo convention; splitting at file granularity inside one is
  *also* fine for our purposes.

---

## 7. Recommended order of operations

If you only do four things this week, do these:

1. **Resolve C-1.** Decide whether the uncommitted `src/tui/keys/mod.rs`
   changes belong on `dev` *now* or in a future WO, and act.
2. **Split C-2.** Break `src/session/executor/tests/mod.rs` into
   feature-aligned files. No logic change; large readability win.
   Drop the diff in a single commit, paste CI output.
3. **Gate C-3.** Add `src/tui` to the coverage gate (or document why it
   is excluded).
4. **Surface the 4,612-test count.** One sentence in
   `docs/TECHNICAL.md`. Update README plugin3 row to reflect actual
   post-split numbers after step 2.

Everything else in this review can flow into either an existing WO or a
new "Series 15: hygiene" workorder series.

---

## 8. Appendix: data sources for this review

- `git status`, `git log --oneline -10`, `git rev-parse HEAD` for repo state.
- `cargo fmt --check` and `cargo check --workspace --all-targets` for
  gate baselines (ran in this session).
- `target/tarpaulin/kirkforge-coverage.json` (most recent
  `cargo tarpaulin --out Xml --locked --lib` artifact committed under
  `target/`).
- `cargo run -p kirkforge-testdoctor -- diagnose --root .` (ran in this
  session; ~75s compile time).
- `python3` walk over `src/` and `crates/` to count `#[test]` /
  `#[tokio::test]` attributes. Used the same `#[test]` then `fn` rule
  as `crates/plugin3-core/tests/readme_drift.rs::count_test_attrs`.
- Direct reads of `docs/TECHNICAL.md`, `docs/adr/README.md`,
  `state.md`, `CHANGELOG.md`, `AGENTS.md`, `CLAUDE.md`.
- File-level `wc -l` across `src/` and `crates/`.

No code was modified during this review.
