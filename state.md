# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.5 (2026-07-22)

**`dev` at `e68ba8f`, `main` at `e68ba8f`.** Phase 5 complete (4 languages). Phase 6 import edges shipped. 62 ADRs.

### What shipped this session (4.1)

| Item | What |
|---|---|
| Task 1: Go tree-sitter grammar | `Language::Go`, `detect_language()` dispatches `.go` → Go. Extracts `function_declaration`, `method_declaration`, `type_declaration` (struct/interface/type alias), `import_declaration`. 4 new tests. ADR-037 Phase 5 complete. |
| Task 2: Import-graph edges | `ImportEdge` struct with `source_file`, `imported_symbol`, `resolved_file`, `line`. `resolve_imports()` resolves relative/crate imports. `RetrievalResult` (symbol + `imported_by`). Prompt builder shows import context. `CachedIndex` includes edges. 5 new tests. ADR-037 Phase 6 (import edges) shipped. |

### Gates

- `cargo test --locked --workspace --no-fail-fast` = all pass (27 context-index tests)
- `cargo test -p plugin3-core --test readme_drift` = 2 passed
- `cargo test -p plugin3-core --test adr_xref_drift` = 3 passed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 62 ADRs (037 updated for Phase 6)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P1-long-1 Phase 6 (cont.) — Call-graph edges | 1-2 weeks | Future |
| P1-long-1 Phase 7 — Embeddings/graph-walk retrieval | 2-3 weeks | Future |
| P1-long-2 follow-up — More bench tasks, multi-model comparison | 1-2 weeks | Future |

### Open cleanup items

- More TUI parity (doom_loop recovery, session child/parent nav, scout subagent, /share, /editor)