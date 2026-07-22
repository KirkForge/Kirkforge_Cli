# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.5 (2026-07-22)

**`dev` at `bb67d23`, `main` at `bb67d23`.** All P2 + P1-long-1/2/3/4 + P2-long-3/4 + P3-long-5/6 shipped. 62 ADRs.

### What shipped this session (3.8)

| Item | What |
|---|---|
| Task 1: P3-long-6 (computer-use depth) | `BrowserSession` with open/close, step tracking, max_steps limit. Multi-step browser flows. 4 new tests. ADR-044. |
| Task 2: Table-driven command dispatch | Refactored ~360-line inline match to `slash_commands.rs` with `COMMANDS` table + `dispatch_slash_command()`. `/help` text generated from table. 2 new tests. |
| Task 3: Consolidate workspace dependencies | 12 common deps (serde, serde_json, tokio, anyhow, tracing, clap, async-trait, chrono, thiserror, toml, tempfile, directories) consolidated into `[workspace.dependencies]`. |

### Gates

- `cargo test --locked --workspace --no-fail-fast` = **2839+ passed, 0 failed, 12 ignored**
- `cargo test -p plugin3-core --test readme_drift` = 2 passed
- `cargo test -p plugin3-core --test adr_xref_drift` = 3 passed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 62 ADRs (044 new from this session)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P3-long-6: Computer-use depth | — | **Shipped** |
| Table-driven command dispatch | — | **Shipped** |
| Workspace dependencies consolidation | — | **Shipped** |
| P1-long-1 Phases 5-7 (TS/Python/Go grammars, import/call-graph, embeddings) | 3-4 weeks | Future |
| P1-long-2 follow-up (benchmark harness: more tasks, multi-model comparison) | 1-2 weeks | Future |

### Open cleanup items

- Persist plugin enable/disable state
- More TUI parity (doom_loop recovery, session child/parent nav, scout subagent, agent steps limit, /share, /editor)