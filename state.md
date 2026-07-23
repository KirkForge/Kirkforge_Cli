# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.5 (2026-07-23)

**`dev` at `d60eb96`, `main` at `6f1e37d`.** Phase 5 complete (4 languages). Phase 6 complete (import + call-graph edges). 10 bench tasks. 62 ADRs.

### What shipped this session (4.2)

| Item | What |
|---|---|
| Task 1: Call-graph edges | `CallEdge` + `CallSite` structs. `extract_call_edges()` walks AST for call expressions. `resolve_call_edges()` resolves callee names to definition files. `retrieve()` returns `called_by` alongside `imported_by`. Prompt builder shows call-chain context. 5 new tests. ADR-037 Phase 6 complete. |
| Task 2: 5 more bench tasks | `fix_failing_test`, `add_error_handling`, `rename_function`, `add_doc_comment`, `extract_module`. 10 total tasks in `benches/tasks/`. |

### Gates

- `cargo test -p kirkforge-context-index --lib` = 32 passed
- `cargo test -p plugin3-core --test readme_drift` = 2 passed
- `cargo test -p plugin3-core --test adr_xref_drift` = 3 passed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 62 ADRs (037 updated for Phase 6 complete)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P1-long-1 Phase 7 — Embeddings/graph-walk retrieval | 2-3 weeks | Future |
| P1-long-2 follow-up (cont.) — Multi-model comparison, CI bench deltas | 1-2 weeks | Future |
| More TUI parity | ongoing | Future |

### Open cleanup items

- More TUI parity (doom_loop recovery, session child/parent nav, scout subagent, /share, /editor)