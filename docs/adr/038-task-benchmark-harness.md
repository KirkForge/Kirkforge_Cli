# ADR-038: Task-benchmark harness

## Status

Accepted

## Context

The 6th-pass review named agent capability as B+ (code-reading grade) and identified the benchmark harness as the single item that turns that grade into a measured grade. Without measurement, every agent-feature claim ("context-index helps") is unfalsifiable.

## Decision

Build `crates/kirkforge-bench/` — a standalone crate that runs benchmark tasks against a headless kirkforge session and collects metrics (success rate, tokens, time, cost, tool calls).

Components:

1. **Task format** — `benches/tasks/*.toml`. Each task file defines a name, difficulty, prompt, setup files, and a verify spec (test_passes, file_contains, or command_exits_zero).

2. **5 representative tasks** — add_test_for_function (easy), fix_clippy_warning (easy), add_cli_flag (medium), refactor_extract_function (medium), add_adr (hard).

3. **Data types** — `BenchTask`, `TaskResult`, `BenchReport`, `BenchSummary` in the bench crate. `load_tasks`, `verify_task`, `write_report`, `write_markdown_summary` are pure functions in the crate. `run_task` and `run_all` live in `src/session/bench.rs` because they depend on the executor.

4. **`kirkforge bench` subcommand** — loads tasks, runs them headlessly, writes JSON report + markdown summary.

5. **CI integration** — a `bench` job in `.github/workflows/ci.yml` that runs on PRs touching agent code, uses the cheapest model via Ollama, and posts the markdown summary as a PR comment.

## Consequences

### Positive

- Every PR that touches agent code gets a benchmark delta.
- Agent capability becomes measured (success rate, tokens, time, cost) rather than code-read.
- TOML task format is simple enough that adding more tasks is trivial.

### Negative

- CI time increases by ~5-10 min per bench job.
- Requires a model endpoint in CI (Ollama with `qwen2.5:0.5b`).
- Task definitions need maintenance as the codebase evolves.

ponytail: TOML task definitions + headless session execution. The upgrade path is a leaderboard, multi-model comparison, and CI benchmark deltas.