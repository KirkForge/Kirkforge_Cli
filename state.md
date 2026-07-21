# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.0 (2026-07-21)

**`dev` at `035586f`.** All 7 P2 short-term items shipped + tested. P1-long-1 (repo-graph context retrieval) scaffolded. 55 ADRs.

### What shipped

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
| P1-long-1 | `crates/kirkforge-context-index/` scaffolded with line-based symbol extraction. ADR-037 (Experimental). |

### 2.9 gaps closed

| Gap | Fix |
|---|---|
| Worktree untested | `worktree_create_write_file_drop_cleanup` test — creates temp git repo, creates worktree, writes file, drops, verifies cleanup. |
| Docker untested | `bash_docker_executes_command_in_container` — `#[ignore = "requires Docker"]` test runs `echo hello` in alpine:latest. |
| run_docker task-orphaning | `out_handle`/`err_handle` awaited with 1s timeout after `child.kill()` on timeout/cancellation paths. |

### Gates

- `cargo test --lib` = **1301 passed, 0 failed, 1 ignored** (+4 new: worktree + 3 context-index)
- `cargo test -p kirkforge-context-index` = 3 passed, 0 failed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 55 ADRs (18 native 3-digit + 18 vendored 4-digit + 19 new: 019-037)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| VS Code NDJSON bridge (Option B) | 2-3 weeks | Not started |
| Unify two verifier buses | 1-2 weeks | ADR-028 design done |
| Context management depth | 1-2 weeks | ADR-027 design done |
| Workflow parallel steps | 2-3 days | Not started |
| Repo-graph context retrieval | 3-4 weeks | Phase 1 scaffolded (line-based), Phase 2-3 future |
| Task-benchmark harness | 2-3 weeks | Not started |
| Execution replay + time-travel | 2-3 weeks | Not started |
| Computer-use depth | 2-3 weeks | Not started |

### Open cleanup items

- `src/tui/keys/mod.rs:71-990` ~900-line match → table-driven command dispatch (P3.1)
- Consolidate `[workspace.dependencies]` for serde/tokio/clap/tracing (P3.3)
- Remove dead `PromptBuilder.cache` (P3.4)
- `edit_file` fuzzy-fallback has zero coverage (test gap)
- Persist plugin enable/disable state (Phase 5.6)
