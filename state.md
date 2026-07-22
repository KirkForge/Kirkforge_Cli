# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.5 (2026-07-22)

**`dev` at `4aa2ae8`, `main` at `327d457`.** All P2 + P1-long-1/2/3/4 + P2-long-3/4 + P3-long-5/6 shipped. 62 ADRs.

### What shipped this session (3.8 + 3.9)

| Item | What |
|---|---|
| Task 1: P3-long-6 (computer-use depth) initial | `BrowserSession` with open/close, step tracking, max_steps limit. 4 new tests. ADR-044. |
| Task 1: P3-long-6 (depth completion) | `BrowserSessionOwner` keeps Chrome alive per session. `SessionLauncher` for async per-session Chrome creation. 6 new session tests + 2 ignored Chrome integration tests. |
| Task 2: Table-driven command dispatch | Refactored ~360-line inline match to `slash_commands.rs` with `COMMANDS` table + `dispatch_slash_command()`. `/help` text generated from table. 2 new tests. |
| Task 3: Consolidate workspace dependencies | 12 common deps consolidated into `[workspace.dependencies]`. |
| Task 4 (3.9): Stale cleanup item removed | "Persist plugin enable/disable state" was already shipped in `4b36211`. Removed from cleanup list. |

### Already-shipped items found during 3.9

- **`max_tool_calls_per_turn`** is already enforced in `src/session/executor/turn.rs:267-416` with a for-loop limit and `TurnEvent::Error("Tool call loop limit reached")`. Test at `tests/mod.rs:2627`. Was listed as "open" but is actually shipped.
- **`toggle_plugin` persistence** already works via `save_config`. Was listed as "open" but shipped in `4b36211`.

### Gates

- `cargo test --locked --workspace --no-fail-fast` = **2845+ passed, 0 failed, 9+ ignored**
- `cargo test -p plugin3-core --test readme_drift` = 2 passed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 62 ADRs (044 updated with depth status)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P3-long-6: Computer-use depth | — | **Shipped** (session lifecycle + step tracking + per-session Chrome) |
| Table-driven command dispatch | — | **Shipped** |
| Workspace dependencies consolidation | — | **Shipped** |
| ~~Persist plugin enable/disable state~~ | — | **Already shipped** (commit `4b36211`) |
| ~~Agent steps limit enforcement~~ | — | **Already shipped** (turn.rs:267-416) |
| P1-long-1 Phases 5-7 (TS/Python/Go grammars, import/call-graph, embeddings) | 3-4 weeks | Future |
| P1-long-2 follow-up (benchmark harness: more tasks, multi-model comparison) | 1-2 weeks | Future |

### Open cleanup items

- More TUI parity (doom_loop recovery, session child/parent nav, scout subagent, /share, /editor)