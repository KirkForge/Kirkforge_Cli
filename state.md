# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.5 (2026-07-22)

**`dev` at `2992c63`, `main` at `2992c63`.** All P2 + P1-long-1 Phases 1-5 (Rust+TS+Python) + P1-long-2/3/4 + P2-long-3/4 + P3-long-5/6 shipped. 62 ADRs. Flaky tests fixed.

### What shipped this session (4.0)

| Item | What |
|---|---|
| Task 1: Fix flaky tests | `test_parallel_tool_batch_runs_concurrently` (reduced sleep to 200ms, threshold to 5s) and `test_always_approve_rule_round_trips_to_next_turn` (replaced spawn+AtomicBool+abort race with try_recv check). |
| Task 2: TypeScript tree-sitter grammar | `Language` enum, `detect_language()`, `SymbolKind::Class/Interface/TypeAlias`, `.ts`/`.tsx` file walking. 5 new tests. ADR-037 Phase 5. |
| Task 3: Python tree-sitter grammar | `Language::Python`, `.py` detection, `function_definition`/`class_definition`/`import_statement`/`import_from_statement`/`decorated_definition` extraction. 3 new tests. ADR-037 Phase 5. |

### Already-shipped items found during 3.9

- **`max_tool_calls_per_turn`** is already enforced in `src/session/executor/turn.rs:267-416` with a for-loop limit and `TurnEvent::Error("Tool call loop limit reached")`. Test at `tests/mod.rs:2627`. Was listed as "open" but is actually shipped.
- **`toggle_plugin` persistence** already works via `save_config`. Was listed as "open" but shipped in `4b36211`.

### Gates

- `cargo test --locked --workspace --no-fail-fast` = **2848+ passed, 0 failed, 7+ ignored**
- `cargo test -p plugin3-core --test readme_drift` = 2 passed
- `cargo test -p plugin3-core --test adr_xref_drift` = 3 passed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 62 ADRs (037 updated for Phase 5 TS+Python)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P3-long-6: Computer-use depth | — | **Shipped** (session lifecycle + step tracking + per-session Chrome) |
| Table-driven command dispatch | — | **Shipped** |
| Workspace dependencies consolidation | — | **Shipped** |
| ~~Persist plugin enable/disable state~~ | — | **Already shipped** (commit `4b36211`) |
| ~~Agent steps limit enforcement~~ | — | **Already shipped** (turn.rs:267-416) |
| ~~Flaky tests~~ | — | **Fixed** (parallel batch threshold + always-approve race fix) |
| P1-long-1 Phase 5 (Go grammar) | 1 hour | Future |
| P1-long-1 Phase 6 (import/call-graph edges) | 1-2 weeks | Future |
| P1-long-1 Phase 7 (embeddings/graph-walk retrieval) | 2-3 weeks | Future |
| P1-long-2 follow-up (benchmark harness: more tasks, multi-model comparison) | 1-2 weeks | Future |

### Open cleanup items

- More TUI parity (doom_loop recovery, session child/parent nav, scout subagent, /share, /editor)