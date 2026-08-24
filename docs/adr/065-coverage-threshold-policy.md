# ADR-065: Coverage-gate threshold policy (75% target, headroom, --skip workaround)

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-07-30

## Context

The 12th-pass review named the coverage gate as a shipped feature that
sat at 62/50/61 (`src/session` / `src/tools` / `src/adapters` line
coverage). The 12-series (WO 12.0–12.9) is the "test infrastructure +
coverage" series: it fixed the tarpaulin flake (12.0), raised the
intermediate thresholds (12.1–12.3), folded the testdoctor into the
workspace (12.4), added per-test timings + flaky detection (12.5) +
coverage-gap reporting (12.7), closed the coverage with 144 new tests
(12.8), and now enforces the final thresholds (12.9).

The CI coverage gate lives in `.github/workflows/ci.yml` (`Enforce
coverage gate` step): a Python script parses tarpaulin's `cobertura.xml`
and fails the job when any of the three `src/` prefixes drops below its
threshold. WO 12.9's goal was 75% on all three.

## Actual coverage (measured, not estimated)

The WO 12.8 commit message estimated `src/session` at ~75%, but a
tarpaulin run against the 12.8 HEAD showed the estimate was optimistic.
The authoritative numbers are the CI `coverage` job from the green run
that landed 12.8 (`30333515698`, 2026-07-28):

- `src/session/`: 68.6% (6710/9781 lines)
- `src/tools/`: 76.5% (1012/1323 lines)
- `src/adapters/`: 84.1% (1255/1492 lines)

`src/tools` and `src/adapters` already clear the 75% bar. `src/session`
does not: the remaining gap is dominated by the async executor loop
(`executor/dispatch.rs`, `executor/loop_.rs`, `executor/turn.rs`) and
the network-bound MCP HTTP transport (`mcp_client/http.rs`), which are
inherently hard to cover with pure unit tests. WO 12.8 already extracted
and tested the pure helpers it could; pushing `src/session` to 75%
requires deep async/network test work that is out of scope for a single
workorder.

## Decision

1. **Set thresholds to reflect actual coverage, not the 75% wish.**
   `src/tools` (76.5%) and `src/adapters` (84.1%) exceed 75%, so their
   thresholds stay at/above 75 (`src/tools` keeps 76.0 — stricter than
   75, since lowering a passing threshold would *weaken* the gate).
   `src/session` is raised from 68.0 to 68.5 — just below the measured
   coverage nudged up by WO 12.9's pure-helper test batch, with ~0.4%
   headroom. The 75% target for `src/session` is **honestly deferred**
   to a follow-up workorder (AGENTS.md §5: honest deferral over a fake
   claim). The WO 12.8 ~75% estimate proved optimistic by ~6 points;
   a single WO of pure-helper tests cannot close a gap dominated by
   async executor + MCP-HTTP code.

2. **Headroom policy.** The threshold is set at or just below the
   actual coverage so the gate catches regressions without being flaky
   on run-to-run tarpaulin variance (~1-2% is normal for the
   instrumented executor tests). Zero headroom (threshold == actual) is
   intentionally strict at first: a single uncovered line fails CI. If
   that proves too strict (a one-line regression fails CI repeatedly
   across benign churn), relax by at most 2 percentage points and record
   the relaxation here + in `state.md` as a conscious decision, never a
   silent drift. Do not relax for a *pattern* of regressions — only for
   one-off flakes.

3. **`--skip test_build_fork_tree_nests_children` stays in CI.** WO 12.0
   fixed the root cause of the tarpaulin flake (the `save()` temp-rename
   race in `session_index.rs`), so the skip is no longer load-bearing.
   It remains as belt-and-suspenders until a tarpaulin-verified run
   confirms the flake is fully gone. Removing it is a separate
   verification step, not part of this ADR.

## What the gate enforces

The `coverage` job runs `cargo tarpaulin --out Xml --locked --lib
--timeout 120 -- --skip test_build_fork_tree_nests_children` and the
Python gate aggregates line coverage per `src/` prefix (matching
`<package name="src/...">` in the Cobertura XML). The thresholds
(`.github/workflows/ci.yml`) are `src/session: 68.5, src/tools: 76.0,
src/adapters: 75.0`. The most recent green CI run (30333515698) measured
68.6 / 76.5 / 84.1 — all three clear these thresholds, so the gate is
proven-green at the chosen floors. Only `src/session`, `src/tools`, and
`src/adapters` are gated — integration tests live in `tests/` (not
`src/`) and are excluded from `--lib` instrumentation, so they never
counted toward the gate. The gate is a regression guard, not a vanity
number: its job is to fail when coverage drops, not to display a high
percentage.

## Consequences

- The 12-series finale ships an honest ≥75% floor on tools (76.0) +
  adapters (75.0) and a `src/session` floor at 68.5 (the measured
  level), with 75% for `src/session` explicitly deferred (not faked).
  The gate catches
  regressions on all three prefixes.
- A follow-up workorder is needed to reach 75% on `src/session`. That
  work is async/network test coverage (executor loop, MCP HTTP), which
  is a larger effort than pure-helper tests.
- The `--skip` workaround stays until a verified run confirms the flake
  is gone; removing it prematurely risks re-introducing the 12.0 flake.
- `ponytail:` / `ceiling:` annotations are unaffected (this WO touches
  `ci.yml` thresholds + docs only; no test annotations changed).

## Notes

- Tarpaulin coverage is non-deterministic (~1-2% run-to-run) on the
  executor tests that run long under instrumentation; the headroom
  policy accounts for this.
- The CI number (9781 valid lines for `src/session`) differs from a
  local tarpaulin run (7892) because CI and local builds can resolve a
  different set of feature-gated lines. The CI number is authoritative
  for setting the threshold; local tarpaulin is used only to find gaps.