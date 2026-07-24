# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.5 (2026-07-24)

**`dev` at HEAD, `main` at 6f1e37d.** Phase 5 complete (4 languages). Phase 6 complete (import + call-graph edges). 10 bench tasks. 67 ADRs.

### What shipped this session (6.1–6.9)

| Item | What |
|---|---|
| WO 6.1: Bench harness realism | Replaced `CompositeToolset::empty()` with `build_bench_toolset()`. Fixed `add_adr` verify path. Updated ADR-038. |
| WO 6.2: Bench delta comparison | `DeltaReport`, `TaskDelta`, `compare_reports()`, `write_markdown_delta()`, `bench compare` CLI. 3 unit tests. |
| WO 6.3: Bench CI wiring | CI bench job with `if: always()` (runs when quality fails), path filters, baseline download, PR comments, artifact uploads. `bench-baseline.yml` scheduled workflow. **Bug fix**: corrected `if` condition and artifact name mismatch. |
| WO 6.4: Bench list and verify-only | `bench list` and `bench verify-only` subcommands. `TaskInfo`, `list_tasks()`, `verify_only()`. |
| WO 6.5: Bench eval ADR | ADR-045 (continuous eval pipeline). ADR-038 updated. |
| WO 6.6: Fold Stratum | `stratum` feature flag (default on). 5 tool wrappers. ADR-046. Hooks deferred. |
| WO 6.7: Fold Plugin3 | `budget` feature flag (default on). 7 tool wrappers. ADR-047. Hooks explicitly deferred. |
| WO 6.8: Fold Draw | `draw` feature flag (default on). `draw_render` tool. ADR-048. Hook deferred. |
| WO 6.9: Fold Video | `video` feature flag (non-default). 8 tool wrappers. ADR-049. Dev build delta ~14.4 MB. |

### Deferred items (honest deferral)

| Item | Why deferred |
|---|---|
| 6.6/6.7/6.8 hooks → in-process | Requires wiring into `turn.rs`/`microcompaction.rs` event loop; out of scope for fold-in MVP. ADRs updated to mark hooks as deferred with upgrade path. |
| `stratum_mode` config field | `enabled_plugins` toggle sufficient for on/off; mode selection passes through tool arguments. |
| `budget_ceiling` / `budget_approaching_ratio` config fields | Budget tools accept ceiling as a parameter; no persistent config needed for MVP. |

### Known CI issues

- **Ollama model pull fails intermittently**: The `integration` CI job fails when `ollama pull` encounters a registry redirect. External service issue; re-running typically succeeds.

### Gates

- `cargo test --locked --workspace --no-fail-fast` = all pass
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- `cargo check --workspace --all-targets` = clean
- `cargo test -p plugin3-core --test adr_xref_drift` = 3 passed
- Feature-gated builds compile and pass

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P1-long-1 Phase 7 — Embeddings/graph-walk retrieval | 2-3 weeks | Future |
| P1-long-2 follow-up (cont.) — Multi-model leaderboard | 1-2 weeks | Future |
| Hook fold-in (6.6/6.7/6.8 hooks → in-process) | 1-2 weeks | Deferred |
| More TUI parity | ongoing | Future |