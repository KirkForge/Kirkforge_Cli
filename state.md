# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.0 (2026-07-21)

**`dev` at `cd35070`.** P2-6 (git-worktree-per-session + Docker execution) landed. All 7 short-term production items from CLI-workorder.md are now on dev. ADR-035 (git worktree) and ADR-036 (Docker) added.

### What shipped in v0.3.0

| Item | What |
|---|---|
| P0 | Restored plugin 1 bench harness + `tool-graphify`. Restored plugin 3 tests. ADR-029. |
| P2-1 | `build` + `test` verifier slots. ADR-031. |
| P2-2 | `PlanReason` trace events. ADR-032. |
| P2-3 | Exponential backoff on tool-call retries. ADR-033. |
| P2-4 | Mid-batch tool-result checkpointing. ADR-034. |
| P2-5 | `--seed <u64>` deterministic mode. ADR-030. |
| P2-6a | `--worktree` flag: isolated git worktree per session. ADR-035. |
| P2-6b | `--docker` flag + `[docker]` config: bash in Docker containers. ADR-036. |

### Gates (v0.3.0 baseline)

- `cargo test --lib` = **1297 passed, 0 failed**
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 50 ADRs (18 native 3-digit + 18 vendored 4-digit + 14 new: 019-036)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| VS Code NDJSON bridge (Option B) | 2-3 weeks | Not started |
| Unify two verifier buses | 1-2 weeks | ADR-028 design done |
| Context management depth | 1-2 weeks | ADR-027 design done |
| Workflow parallel steps | 2-3 days | Not started |
| Repo-graph context retrieval | 3-4 weeks | Not started |
| Task-benchmark harness | 2-3 weeks | Not started |
| Execution replay + time-travel | 2-3 weeks | Not started |
| Computer-use depth | 2-3 weeks | Not started |

### Open cleanup items

- `src/tui/keys/mod.rs:71-990` ~900-line match → table-driven command dispatch (P3.1)
- Consolidate `[workspace.dependencies]` for serde/tokio/clap/tracing (P3.3)
- Remove dead `PromptBuilder.cache` (P3.4)
- `edit_file` fuzzy-fallback has zero coverage (test gap)
- Persist plugin enable/disable state (Phase 5.6)
