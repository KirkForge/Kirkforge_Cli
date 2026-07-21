# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.0+ (2026-07-22)

**`dev` at `1fa14fc`.** All 7 P2 short-term items shipped + tested. P1-long-1 (repo-graph) shipped. P1-long-2 (benchmark harness) shipped. 56 ADRs.

### What shipped this session

| Item | What |
|---|---|
| CI fix | `cargo fmt` + `clippy::new_without_default` on ContextIndex + duplicate context_index block removed. Dev and main both green. |
| P1-long-2 | `crates/kirkforge-bench/` — BenchTask, TaskResult, BenchReport, BenchSummary, load_tasks, verify_task, write_report, write_markdown_summary. 10 unit tests. 5 task TOML files. `kirkforge bench` subcommand. CI bench job. ADR-038. |

### Gates

- `cargo test --locked --workspace --no-fail-fast` = **2805 passed, 0 failed, 45 ignored**
- `cargo test -p plugin3-core --test readme_drift` = 2 passed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- `cargo test -p kirkforge-bench` = 10 passed, 0 failed
- 56 ADRs (18 native 3-digit + 18 vendored 4-digit + 20 new: 019-038)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| VS Code NDJSON bridge (Option B) | 2-3 weeks | Not started |
| Unify two verifier buses | 1-2 weeks | ADR-028 design done |
| Context management depth | 1-2 weeks | ADR-027 design done |
| Workflow parallel steps | 2-3 days | Not started |
| Repo-graph context retrieval | 3-4 weeks | Phase 1-3 shipped. Phase 4-7 future |
| Task-benchmark harness | 2-3 weeks | **Shipped** (P1-long-2, ADR-038) |
| Execution replay + time-travel | 2-3 weeks | Not started |
| Computer-use depth | 2-3 weeks | Not started |

### Open cleanup items

- `src/tui/keys/mod.rs:71-990` ~900-line match → table-driven command dispatch (P3.1)
- Consolidate `[workspace.dependencies]` for serde/tokio/clap/tracing (P3.3)
- Remove dead `PromptBuilder.cache` (P3.4)
- `edit_file` fuzzy-fallback has zero coverage (test gap)
- Persist plugin enable/disable state (Phase 5.6)