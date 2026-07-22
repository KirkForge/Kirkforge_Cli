# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.5 (2026-07-22)

**`dev` at `b149e72`, `main` at `b149e72`.** All P2 + P1-long-1/2/3/4 + P2-long-3/4 + P3-long-5 shipped. 61 ADRs.

### What shipped this session (3.7)

| Item | What |
|---|---|
| Task 1: P3-long-6 (computer-use depth) | DEFERRED — requires headless Chrome dep, 2-3 hours |
| Task 2: P1-long-1 Phase 4 (disk caching) | `CachedIndex` with git-HEAD invalidation. Cache at `.kirkforge/context-index/cache.json`. 5 new tests. ADR-037 updated. |
| Task 3: Table-driven command dispatch | DEFERRED — 1345-line refactor, 45 min |
| Task 4: Remove dead PromptBuilder.cache | Removed unused `HashMap<String, String>` field. |
| Task 5: edit_file fuzzy-fallback tests | 4 new tests: exact match, whitespace-tolerant, no-match, partial-match. |

### Gates

- `cargo test --locked --workspace --no-fail-fast` = **2839 passed, 0 failed**
- `cargo test -p plugin3-core --test readme_drift` = 2 passed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 61 ADRs (037 updated, 041-043 new from 3.5/3.6)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P3-long-6: Computer-use depth | 2-3 hours | DEFERRED |
| Table-driven command dispatch | 45 min | DEFERRED |
| P1-long-1 Phases 5-7 (TS/Python/Go, import/call-graph, embeddings) | 3-4 weeks | Future |
| P2-long-3: Execution replay | — | **Shipped** |
| P2-long-4: VS Code extension | — | **Shipped** |
| P3-long-5: Verifier-bus bridge | — | **Shipped** |
| P1-long-1 Phase 4: Disk caching | — | **Shipped** |

### Open cleanup items

- `src/tui/keys/mod.rs` ~1345-line match → table-driven command dispatch (deferred)
- Consolidate `[workspace.dependencies]` for serde/tokio/clap/tracing
- Persist plugin enable/disable state