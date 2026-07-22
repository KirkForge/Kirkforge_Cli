# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.3 (2026-07-22)

**`dev` at `1ca1f56`, `main` at `1ca1f56`.** All P2 short-term items shipped. P1-long-1 (repo-graph) shipped. P1-long-2 (benchmark harness) shipped. P2-long-3 (execution replay) shipped. P2-long-4 (VS Code extension) shipped. Subagent model selection + OpenCode Zen provider shipped. `/thinking` TUI toggle shipped. 60 ADRs.

### What shipped this session (3.5)

| Item | What |
|---|---|
| Task 1: Subagent model selection + Zen provider | `TaskRequest.model` field, `subagent_allowed_models` allowlist, `AdapterKind::OpenCodeZen`, `opencode/` prefix routing, `opencode_zen_api_key` + `opencode_zen_endpoint` config. ADR-041, ADR-042. 5 new tests. |
| Task 2: Merge dev→main | Fast-forward merge. `main == dev`. |
| Task 3: @file references | Already shipped (mentions.rs). Full path/range/raw support, PathGuard safety, tilde expansion. |
| Task 4: !bash prefix | Already shipped (bang.rs). Permission model, timeout, collapsible output. |
| Task 5: /thinking toggle | `/thinking` slash command, `[thinking hidden]` marker when collapsed, Esc toggle. 2 new tests. |

### Gates

- `cargo test --locked --workspace --no-fail-fast` = **2823 passed, 0 failed**
- `cargo test -p plugin3-core --test readme_drift` = 2 passed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 60 ADRs (041 + 042 new)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| VS Code NDJSON bridge (Option B) | 2-3 weeks | **Shipped** (P2-long-4, ADR-040) |
| Unify two verifier buses | 1-2 weeks | ADR-028 design done |
| Context management depth | 1-2 weeks | ADR-027 design done |
| Workflow parallel steps | 2-3 days | Not started |
| Repo-graph context retrieval | 3-4 weeks | Phase 1-3 shipped. Phase 4-7 future |
| Task-benchmark harness | 2-3 weeks | **Shipped** (P1-long-2, ADR-038) |
| Execution replay + time-travel | 2-3 weeks | **Shipped** (P2-long-3, ADR-039) |
| Computer-use depth | 2-3 weeks | Not started |
| Doom-loop recovery, session nav, /share, /editor | 1-2 weeks each | Future TUI parity |

### Open cleanup items

- `src/tui/keys/mod.rs:71-990` ~900-line match → table-driven command dispatch (P3.1)
- Consolidate `[workspace.dependencies]` for serde/tokio/clap/tracing (P3.3)
- Remove dead `PromptBuilder.cache` (P3.4)
- `edit_file` fuzzy-fallback has zero coverage (test gap)
- Persist plugin enable/disable state (Phase 5.6)