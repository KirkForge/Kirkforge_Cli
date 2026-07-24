# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.5 (2026-07-24)

**`dev` at HEAD, `main` at 6f1e37d.** Phase 5 complete (4 languages). Phase 6 complete (import + call-graph edges). 10 bench tasks. 67 ADRs.

### What shipped this session (6.1–6.9)

| Item | What |
|---|---|
| WO 6.1: Bench harness realism | Replaced `CompositeToolset::empty()` with `build_bench_toolset()` providing sandboxed `read_file`, `write_file`, `edit_file`, `bash`, `glob`, `grep`. Fixed `add_adr` task verify path from `039-` to `062-benchmark-delta-comparison`. Updated ADR-038. |
| WO 6.2: Bench delta comparison | `DeltaReport`, `TaskDelta`, `compare_reports()`, `write_markdown_delta()` in `kirkforge-bench`. `bench compare` CLI subcommand. 3 unit tests. |
| WO 6.3: Bench CI wiring | CI bench job uses `if: always()`, path filters, baseline download, PR comments via `gh`, artifact uploads. `bench-baseline.yml` scheduled workflow. |
| WO 6.4: Bench list and verify-only | `bench list` and `bench verify-only` subcommands. `TaskInfo`, `list_tasks()`, `verify_only()`. 3 unit tests. `bench` CLI restructured to subcommands. |
| WO 6.5: Bench eval ADR | ADR-045 (continuous eval pipeline). ADR-038 updated to reflect shipped pipeline. ADR drift tests pass. |
| WO 6.6: Fold Stratum | `stratum` feature flag (default on). `src/session/stratum.rs` with 5 tool wrappers (`run`, `apply`, `mode`, `rules`, `config_validate`). ADR-046. |
| WO 6.7: Fold Plugin3 | `budget` feature flag (default on). `src/session/budget.rs` with 7 tool wrappers. ADR-047. Hooks remain shell scripts (upgrade path: in-process). |
| WO 6.8: Fold Draw | `draw` feature flag (default on). `src/session/draw.rs` with `draw_render` tool. ADR-048. |
| WO 6.9: Fold Video | `video` feature flag (non-default). `src/session/video.rs` with 8 tool wrappers. ADR-049. |

### Gates

- `cargo test --locked --workspace --no-fail-fast` = all pass
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- `cargo check --workspace --all-targets` = clean
- `cargo test -p plugin3-core --test adr_xref_drift` = 3 passed
- Feature-gated builds: `--features stratum`, `--features budget`, `--features draw`, `--features video` all compile and pass

### Known CI issues

- **Ollama model pull fails intermittently**: The `integration` CI job fails when `ollama pull` encounters a registry redirect (`realm host "ollama.com" does not match original host "registry.ollama.ai"`). This is an external service issue, not a code problem. Re-running the job typically succeeds.

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P1-long-1 Phase 7 — Embeddings/graph-walk retrieval | 2-3 weeks | Future |
| P1-long-2 follow-up (cont.) — Multi-model leaderboard | 1-2 weeks | Future |
| More TUI parity | ongoing | Future |

### Open cleanup items

- More TUI parity (doom_loop recovery, session child/parent nav, scout subagent, /share, /editor)