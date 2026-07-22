# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.0+ (2026-07-22)

**`dev` at `8174dd6`, `main` at `7cfb8c7`.** All P2 short-term items shipped. P1-long-1 (repo-graph) shipped. P1-long-2 (benchmark harness) shipped. P2-long-3 (execution replay) shipped. P2-long-4 (VS Code extension) shipped. 58 ADRs.

### What shipped this session (3.3)

| Item | What |
|---|---|
| Task 1: VS Code extension | Inline diffs (accept/reject/status bar), TODO panel (3-state color-coded), chat panel (input + send + tool call details), LSP bridge (diagnostics on save + debounce), bridge sendPrompt/sendApproval, format.ts pure module. 13 tests. `.vsix` packages. CI vscode job. ADR-040. |

### Gates

- `cargo test --locked --workspace --no-fail-fast` = **2817 passed, 0 failed, 45 ignored**
- `cd editors/vscode && npm run build && npm test` = **13 passed, 0 failed**
- `npx vsce package` = produces `kirkforge-vscode-0.2.0.vsix`
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 58 ADRs (18 native 3-digit + 18 vendored 4-digit + 22 new: 019-040)

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

### Open cleanup items

- `src/tui/keys/mod.rs:71-990` ~900-line match → table-driven command dispatch (P3.1)
- Consolidate `[workspace.dependencies]` for serde/tokio/clap/tracing (P3.3)
- Remove dead `PromptBuilder.cache` (P3.4)
- `edit_file` fuzzy-fallback has zero coverage (test gap)
- Persist plugin enable/disable state (Phase 5.6)