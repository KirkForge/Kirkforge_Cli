# ADR-0066: KIRK-BENCH spec + signature Token Budget Challenge

- **Status:** Accepted
- **Date:** 2026-07-30

## Context

The bench harness (ADR-038) was built incrementally (WO 6.1 → 9.9)
by adding tasks that exercised specific features (plugin tools,
multi-file patterns, PR review). It grew to 30 ad-hoc tasks but
never adopted a *spec*: no `KIRK-BENCH.md` defining the category
taxonomy, the universal scoring format, or a signature challenge.

The Pass-14 REVIEW gave KirkForge an "A" on Bench/Measurement — the
only product of the four with a published bench (30 tasks,
multi-model leaderboard, PR-delta CI gate). But the differentiator
benchmark the architecture was built for — one that showcases the
tree-sitter context index + Stratum compression + Plugin3 budget
guard under progressively tighter context budgets — did not exist.

The `123.md` author's closing line: "If you package all of this into a
KIRK-BENCH.md with automated execution and publish results over time,
you'll have something more compelling than 'we support more providers.'"

## Decision

1. **Adopt `KIRK-BENCH.md` as the bench spec.** Eight categories (A
   Repository Understanding, B Refactoring, C Bug Fixes, D New
   Features, E Verification, F Context Intelligence, G Real
   Engineering, H Cost), 40 numbered tasks, one universal scoring
   block, 10 hero benchmarks. The 30 pre-existing tasks are mapped to the spec categories
   in a table — many-to-one, so they cover a subset of the 40 slots;
   19 spec tasks have no existing implementation and are listed as
   "planned" (honest deferral — they are future WOs, not built here).

2. **The Token Budget Challenge is the signature benchmark.** It runs
   the same task 5× under descending context budgets (128k → 64k →
   32k → 16k → 8k) and records six metrics per ceiling: success,
   prompt tokens, completion tokens, compression passes, cost. This
   is the one benchmark that aligns with KirkForge's design
   philosophy (tree-sitter indexing, Stratum, budget management)
   rather than mirroring existing suites.

3. **Wire the budget ceiling via an env override, not new budget
   code.** `BenchTask::budget_ceiling: Option<usize>` (serde-optional,
   default `None`) is the task-side field. When set, the runner
   exports `KF_CODE_BUDGET_CEILING=<n>` to the agent's env. The
   existing config env-override layer
   (`src/session/config/env_overrides.rs`) reads it into
   `cfg.tools.budget_ceiling`, and `init_from_config` applies it to
   the shared `TokenBudget`. This reuses the existing budget
   infrastructure (ADR-0005 / WO 7.5 / WO 8.6) — no new budget code.

4. **The runner loop lives in `src/session/bench.rs`.**
   `run_token_budget_challenge` runs the task once per ceiling in
   `BUDGET_CHALLENGE_CEILINGS`, cloning the task with
   `budget_ceiling` set per run. `run_all` dispatches on the task
   name (`token_budget_challenge`) to the loop instead of the
   single-run path. The per-ceiling results fold into the standard
   `BenchReport`; the dedicated `BudgetChallengeReport` (markdown
   table) is the public scoreboard artifact.

5. **Do NOT gate the 30 existing tasks on the new spec.** They keep
   their current verify specs. The spec is organization, not a
   rewrite mandate.

## Consequences

- This ADR is the durable spec artifact; the task
  implementations live in `benches/tasks/*.toml`. A standalone
  `KIRK-BENCH.md` may be extracted later; until then, the ADR +
  TOML files are canonical.
- `BenchTask` gains a serde-optional `budget_ceiling` field; existing
  task TOML files parse unchanged (default `None`).
- `TaskResult` gains a serde-optional `compression_passes` field
  (counts `TurnEvent::CompactionReport`); existing serialized reports
  parse unchanged.
- The `KF_CODE_BUDGET_CEILING` env hook is a 4-line addition to
  `env_overrides.rs` mirroring `KIRKFORGE_MINIFY_ABOVE_BYTES` (WO 9.7).
- The signature challenge requires a live model to run (the setup has
  a failing test the model must fix); `bench verify-only` skips it
  (`requires_model = true`), `bench run` executes it. A live `bench
  run` is the real test, gated on Ollama per AGENTS.md §4.
- The remaining 19 spec tasks (Find Dead Code, Cross-Repository
  Search, Large Repository Navigation, etc.) are future WOs — each
  exercises a specific feature (context index, workspace support,
  verifier bus) and is listed in `KIRK-BENCH.md` as planned.

## Notes

- The `123.md` author's "Universal Scoring" block is the contract
  every bench task should emit. The current bench reports (JSON +
  markdown) emit some of these fields; the Token Budget Challenge
  report emits all of them per ceiling level.
- The signature challenge is the *showcase* — the report table is the
  public artifact. `write_budget_challenge_report` emits a clean
  markdown table (ceiling × success × prompt tokens × completion
  tokens × compression passes × cost).
- `ponytail:` / `ceiling:` annotations are unaffected — the new
  `BUDGET_CHALLENGE_CEILINGS` const is pinned by the
  `budget_challenge_ceilings_are_descending_powers_of_two` test, and
  the env wiring is pinned by `test_env_budget_ceiling`.