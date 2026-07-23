# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.5 (2026-07-24)

**`dev` at HEAD, `main` at 6f1e37d.** Phase 5 complete (4 languages). Phase 6 complete (import + call-graph edges). 10 bench tasks. 62 ADRs.

### What shipped this session (6.1–6.4)

| Item | What |
|---|---|
| WO 6.1: Bench harness realism | Replaced `CompositeToolset::empty()` with `build_bench_toolset()` providing sandboxed `read_file`, `write_file`, `edit_file`, `bash`, `glob`, `grep`. Fixed `add_adr` task verify path from `039-` to `062-benchmark-delta-comparison`. Updated ADR-038. |
| WO 6.2: Bench delta comparison | `DeltaReport`, `TaskDelta`, `compare_reports()`, `write_markdown_delta()` in `kirkforge-bench`. `bench compare` CLI subcommand. 3 unit tests for comparison. |
| WO 6.3: Bench CI wiring | CI bench job now uses `if: always()`, path filters, baseline download, PR comments via `gh`, artifact uploads. New `bench-baseline.yml` scheduled workflow for nightly `main` reports. |
| WO 6.4: Bench list and verify-only | `bench list` and `bench verify-only` subcommands. `TaskInfo`, `list_tasks()`, `verify_only()` in `kirkforge-bench`. 3 unit tests. `bench` CLI restructured from flat args to subcommands (`run`, `compare`, `list`, `verify-only`). |

### Gates

- `cargo test --locked --workspace --no-fail-fast` = all pass
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- `cargo check --workspace --all-targets` = clean
- `cargo test -p plugin3-core --test adr_xref_drift` = 3 passed
- `cargo test -p kirkforge-bench` = 10 passed (incl. new comparison + listing + verify tests)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P1-long-1 Phase 7 — Embeddings/graph-walk retrieval | 2-3 weeks | Future |
| P1-long-2 follow-up (cont.) — Multi-model leaderboard | 1-2 weeks | Future |
| More TUI parity | ongoing | Future |

### Open cleanup items

- More TUI parity (doom_loop recovery, session child/parent nav, scout subagent, /share, /editor)