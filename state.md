# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.4 (2026-07-22)

**`dev` at `b69c0dc`, `main` at `b69c0dc`.** All P2 + P1-long-1/2 + P2-long-3/4 + subagent model + Zen provider + verifier bus shipped. 61 ADRs.

### What shipped this session (3.6)

| Item | What |
|---|---|
| Task 1: Fix CI | `cargo fmt` + `clippy unnecessary_map_or` lint fix. CI green. |
| Task 2: P3-long-5 verifier bus | `VerifierBus`, `BusVerifier` trait, `VerdictEntry`, `VerifyContext`, `VerifierSource`, `Severity` types. Executor wires bus after file-modifying tool calls. ADR-043. 7 unit tests. |

### Gates

- `cargo test --locked --workspace --no-fail-fast` = **2830 passed, 0 failed**
- `cargo test -p plugin3-core --test readme_drift` = 2 passed
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- 61 ADRs (043 new)

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| Repo-graph context retrieval | 3-4 weeks | Phase 1-3 shipped. Phase 4-7 future |
| Task-benchmark harness | 2-3 weeks | **Shipped** (P1-long-2, ADR-038) |
| Execution replay + time-travel | 2-3 weeks | **Shipped** (P2-long-3, ADR-039) |
| VS Code extension | 2-3 weeks | **Shipped** (P2-long-4, ADR-040) |
| Verifier-bus bridge code | 2-3 hours | **Shipped** (P3-long-5, ADR-043) |
| Computer-use depth | 2-3 weeks | Not started |
| Doom-loop recovery, session nav, /share, /editor | 1-2 weeks each | Future TUI parity |

### Open cleanup items

- `src/tui/keys/mod.rs:71-990` ~900-line match → table-driven command dispatch (P3.1)
- Consolidate `[workspace.dependencies]` for serde/tokio/clap/tracing (P3.3)
- Remove dead `PromptBuilder.cache` (P3.4)
- `edit_file` fuzzy-fallback has zero coverage (test gap)
- Persist plugin enable/disable state (Phase 5.6)