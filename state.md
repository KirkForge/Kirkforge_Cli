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
| WO 6.6: Fold Stratum | `stratum` feature flag (default on). 5 tool wrappers. 2 in-process hooks (`StratumSessionStartHook`, `StratumPreToolBashHook`). ADR-046. `stratum_mode` config field deferred. |
| WO 6.7: Fold Plugin3 | `budget` feature flag (default on). 7 tool wrappers. 4 in-process hooks with full event context (`SessionStartHook`, `PostToolBashHook`, `PostToolWriteFileHook`, `PreCompactHook`); lossy canned-JSON shim eliminated; shared `TokenBudget` via `OnceLock`. ADR-047. Config fields deferred. |
| WO 6.8: Fold Draw | `draw` feature flag (default on). `draw_render` tool. 1 in-process hook (`DrawPostTurnHook`). ADR-048. |
| WO 6.9: Fold Video | `video` feature flag (non-default). 8 tool wrappers. ADR-049. Dev build delta ~14.4 MB. |
| WO 7.0: Plugin system consolidation | Two-path dispatch (compiled-in vs external shell-out) unified behind a single `enabled_plugins` toggle. Folded plugins (Stratum, Plugin3, Draw, Video) with their feature ON are skipped by the shell loader and served compiled-in; with feature OFF they fall back to shell plugins (graceful degradation). Node SDK (`kirkforge-plugin`) stays external. `/plugins list` shows source and feature gate. ADR-050 pinned. |

### Deferred items (honest deferral)

| Item | Why deferred |
|---|---|
| `stratum_mode` config field | `enabled_plugins` toggle sufficient for on/off; mode selection passes through tool arguments. |
| `budget_ceiling` / `budget_approaching_ratio` config fields | Budget tools accept ceiling as a parameter; no persistent config needed for MVP. |
| Plugin3 hook action (slicing/compacting tool results in the turn loop) | The 4 hooks are in-process and receive real event context, but they observe and report budget usage only; they do not yet slice/compact tool results before they enter the conversation. |

### In-process hook infrastructure (shipped)

The hooks for WO 6.6/6.7/6.8 are now in-process Rust handlers (no shell scripts) built on shared infrastructure:

- `InProcessHook` trait in `src/session/hooks.rs`.
- `HookContext` struct with `tool_result` and `compact_stats` fields (replaces the env-var shim with real event context).
- `HookRunner.add_in_process_hook()` method.
- `HookRunner.run_with_context()` and `run_decision_with_context()` methods.
- `ToolOutcome.text_content()` helper in `src/shared/mod.rs`.
- `Executor::run_hook_with_result()` method; it and `run_compact_hook` pass the full `HookContext` to in-process hooks.

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
| Plugin3 hook action (slice/compact in turn loop) | 1-2 weeks | Deferred |
| More TUI parity | ongoing | Future |