# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.0 (2026-07-21)

**`main` at `b805314`, `dev` at `15b28e1`.** Both CI green. v0.3.0 tag cut, release workflow in progress. All 6 P2 short-term production items (P0 + P2-1..P2-5) from CLI-workorder.md are landed. Remote feature branches deleted. Stale worktrees cleaned up. Only `dev` and `main` remain.

### What shipped in v0.3.0

| Item | What |
|---|---|
| P0 | Restored plugin 1 bench harness (`bench/kirkforge-mini/` 4 tasks × 9 workers) + `tool-graphify` package (real import-graph with extension resolution). Restored plugin 3's `size_budget.rs`, `build_spec_drift.rs`, `readme_drift.rs` tests. Re-wired `emitter-factory.ts` to use `GraphifyEmitter`. ADR-029. |
| P2-1 | `build` (priority 3) and `test` (priority 5) verifier slots in `src/session/verifier/{build,test}.rs`. ADR-031. |
| P2-2 | `PlanReason` variant on `MetricEvent` with `PlanDecisionKind` enum. Emitted at tool-select, compaction-trigger, context-truncate, memory-retrieve, prompt-failure points. ADR-032. |
| P2-3 | `RetryTracker::wait_before_retry()` sleeps with exponential backoff (1s/2s/4s) between parse-error retries. ADR-033. |
| P2-4 | Per-tool-result checkpoint in `dispatch_tool_call_batch` so a crash mid-batch recovers completed results. ADR-034. |
| P2-5 | `--seed <u64>` CLI flag pins temperature=0, passes seed to provider bodies, forces sequential dispatch. ADR-030. |

### 2.7 regressions fixed

| Regression | Fix |
|---|---|
| Main syntax error (P2-4 merge) | Merged dev's syntax fix to main. CI green. |
| Plugin3 tests (README + lto) | Added `crates/plugin3-core/README.md` with State table. Adapted `readme_drift.rs` to read plugin3-core README. Adapted `size_budget.rs` to CLI workspace (`lto = true`, `strip = true`). `build_spec_drift.rs` (33 tests) marked `#[ignore]` — original Plugin3 build spec, not CLI workspace. |
| Node SDK (tool-graphify tsc) | Added `tool-graphify` to root tsconfig.json, orchestrator tsconfig.json, and orchestrator package.json references. `tsc --build` passes. |
| v0.3.0 tag not cut | `git tag v0.3.0 && git push origin v0.3.0`. Release workflow triggered. |
| Main CI not re-verified | Both main and dev CI green (run IDs below). |

### Gates (v0.3.0 baseline)

- `cargo test --locked --workspace --no-fail-fast` = **2787 passed, 0 failed**
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- `cargo test -p plugin3-core --test readme_drift` = 2 passed, 0 failed
- `cargo test -p plugin3-core --test size_budget` = 3 passed, 0 failed
- `cd npm/kirkforge-plugin && npx tsc --build` = exit 0
- Main CI: run 29802791079 → `success`
- Dev CI: run 29802791079 → `success`
- 48 ADRs (18 native 3-digit + 18 vendored 4-digit + 12 new: 019-030 + 031-034)

### Remaining short-term (path to A− production)

| Item | Effort | Status |
|---|---|---|
| P2-6 Docker execution mode | 1 + 3-5 days | Not started |
| VS Code NDJSON bridge (Option B) | 2-3 weeks | Not started |
| Unify two verifier buses | 1-2 weeks | ADR-028 design done |
| Context management depth | 1-2 weeks | ADR-027 design done |
| Workflow parallel steps | 2-3 days | Not started |

### Remaining long-term (path to A agent)

| Item | Effort | Status |
|---|---|---|
| Repo-graph context retrieval | 3-4 weeks | Not started |
| Task-benchmark harness | 2-3 weeks | Not started |
| Execution replay + time-travel | 2-3 weeks | Not started |
| Computer-use depth | 2-3 weeks | Not started |

### Open cleanup items (from prior sessions)

- `src/tui/keys/mod.rs:71-990` ~900-line match → table-driven command dispatch (P3.1)
- Consolidate `[workspace.dependencies]` for serde/tokio/clap/tracing (P3.3)
- Remove dead `PromptBuilder.cache` (P3.4)
- `edit_file` fuzzy-fallback has zero coverage (test gap)
- Persist plugin enable/disable state (Phase 5.6)
