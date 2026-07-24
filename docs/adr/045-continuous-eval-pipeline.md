# ADR-045: Continuous Evaluation Pipeline

## Status

Accepted

## Context

Workorders 6.1-6.4 delivered the benchmark harness, delta comparison, CI wiring, and verify-only subcommands. ADR-038 described the intent but overclaimed the CI integration. This ADR pins the actual continuous evaluation pipeline design.

## Decision

Implement a continuous evaluation pipeline with:

1. **Nightly baseline** — a scheduled workflow (`bench-baseline.yml`) runs `kirkforge bench run` on `main` nightly at 04:00 UTC and on push to main, uploading the JSON report as a `bench-baseline` artifact (90-day retention).

2. **Per-PR run with path filter** — the CI `bench` job runs only when `src/session/**`, `src/adapters/**`, `src/tools/**`, `crates/kirkforge-bench/**`, `benches/tasks/**`, or `.github/workflows/ci.yml` change. It uses `if: always()` so it runs even when quality fails (but not when fmt fails — a broken build can't bench).

3. **Delta comparison** — the CI bench job downloads the last `main` baseline artifact and runs `kirkforge bench compare --baseline <baseline> --current <current> --summary bench-delta.md`.

4. **PR comment** — the delta markdown (or plain summary if no baseline exists) is posted as a PR comment using `gh pr comment`.

5. **Fast deterministic smoke** — `kirkforge bench verify-only` runs task verification without LLM, catching stale task definitions in seconds.

6. **Non-gating** — the bench job does not fail CI. A lenient 10% success-rate floor warns but does not block. Gating will be added once baselines stabilize.

## Consequences

### Positive

- Every PR that touches agent code gets a benchmark delta.
- Task definitions are validated without spending model tokens.
- Nightly baselines accumulate for trend analysis.

### Negative

- CI time increases by ~5-10 min per bench job.
- Requires a model endpoint in CI (Ollama with `qwen2.5:0.5b`).
- Ollama model pull fails intermittently (registry redirect); re-running typically succeeds.

## Implementation notes

- `DeltaReport`, `TaskDelta`, `compare_reports()`, `write_markdown_delta()` in `kirkforge-bench`.
- `bench compare --baseline <json> --current <json> --summary <md>` CLI subcommand.
- `bench list` and `bench verify-only` subcommands.
- CI bench job uses `if: always()` with path filter, baseline download via `gh run download`, PR comment via `gh pr comment`.
- `bench-baseline.yml` scheduled workflow runs on `main` push and nightly cron.
- `benches/baselines/` is gitignored; CI uploads/downloads artifacts instead.