# KirkForge-Cli Production-Readiness State

## Current baseline: v0.3.6 (2026-07-25)

**`dev` at HEAD, `main` at 98e863a.** Phase 5 complete (4 languages). Phase 6 complete (import + call-graph edges). Phase 7 complete (embeddings + graph-walk retrieval). 10 bench tasks. 68 ADRs. Workorders 7.1–7.9 all Done. Workorders 8.1, 8.2, 8.4, 8.5, 8.6 Done.

### What shipped this session (8.2)

| Item | What |
|---|---|
| WO 8.2a: Doom loop detection | `DoomLoopTracker` (sliding window of 5 tool errors; fires at 3 identical). `TurnEvent::DoomLoopDetected` to TUI + `MetricEvent::DoomLoop` to metrics. Centered warning banner with break / plan / continue actions. Successful tool call resets the tracker. |
| WO 8.2b: `/sessions tree` | `session_index::build_fork_tree()` reads `<data_dir>/sessions/forks/<id>/fork.json` and groups forks under their parent. ASCII renderer with `├─` / `└─` / `│` connectors. Orphan forks listed as roots. No new dependencies (no `tui-tree-widget`). |
| WO 8.2c: Scout subagent | `ScoutSubagent` struct + `SCOUT_TOOLS` allow-list (`read_file`, `read_image`, `grep`, `glob`). `filter_tools()` enforces the read-only guarantee at the type level. `tools_for_scout` in `persona.rs` builds the full toolset and runs the filter. No `bash` — the scout is the most conservative subagent surface. |

### What shipped this session (6.1–6.9)

| Item | What |
|---|---|
| WO 6.1: Bench harness realism | Replaced `CompositeToolset::empty()` with `build_bench_toolset()`. Fixed `add_adr` verify path. Updated ADR-038. |
| WO 6.2: Bench delta comparison | `DeltaReport`, `TaskDelta`, `compare_reports()`, `write_markdown_delta()`, `bench compare` CLI. 3 unit tests. |
| WO 6.3: Bench CI wiring | CI bench job with `if: always()` (runs when quality fails), path filters, baseline download, PR comments, artifact uploads. `bench-baseline.yml` scheduled workflow. **Bug fix**: corrected `if` condition and artifact name mismatch. |
| WO 6.4: Bench list and verify-only | `bench list` and `bench verify-only` subcommands. `TaskInfo`, `list_tasks()`, `verify_only()`. |
| WO 6.5: Bench eval ADR | ADR-045 (continuous eval pipeline). ADR-038 updated. |
| WO 6.6: Fold Stratum | `stratum` feature flag (default on). 5 tool wrappers. 2 in-process hooks (`StratumSessionStartHook`, `StratumPreToolBashHook`). ADR-046. `stratum_mode` config field shipped in WO 7.5. |
| WO 6.7: Fold Plugin3 | `budget` feature flag (default on). 7 tool wrappers. 4 in-process hooks with full event context (`SessionStartHook`, `PostToolBashHook`, `PostToolWriteFileHook`, `PreCompactHook`); lossy canned-JSON shim eliminated; shared `TokenBudget` via `OnceLock`. ADR-047. Budget config fields shipped in WO 7.5. |
| WO 6.8: Fold Draw | `draw` feature flag (default on). `draw_render` tool. 1 in-process hook (`DrawPostTurnHook`). ADR-048. |
| WO 6.9: Fold Video | `video` feature flag (non-default). 8 tool wrappers. ADR-049. Dev build delta ~14.4 MB. |
| WO 7.0: Plugin system consolidation | Two-path dispatch (compiled-in vs external shell-out) unified behind a single `enabled_plugins` toggle. Folded plugins (Stratum, Plugin3, Draw, Video) with their feature ON are skipped by the shell loader and served compiled-in; with feature OFF they fall back to shell plugins (graceful degradation). Node SDK (`kirkforge-plugin`) stays external. `/plugins list` shows source and feature gate. ADR-050 pinned. |
| WO 7.7: KVB verifier bus bridge | Plugin-declared `Capability::Verifier` entries now register into the unified `VerifierBus` (ADR-043) via `VerifierBus::add_plugin_verifier` + `register_plugin_verifiers_into_bus`. Bus runs plugin verifiers through the host `PluginVerifier` env-cleared subprocess and tags results `VerifierSource::Plugin(name)`; error verdicts inject into the conversation. Live reload rebuilds bus plugin verifiers. Legacy `PluginVerifierAdapter` (event-driven) retained. ADR-028 updated to Accepted (partially implemented). |
| WO 7.5: Budget and Stratum config fields | Added `stratum_mode` (Option<String>), `budget_ceiling` (usize, default 200_000), `budget_approaching_ratio` (f64, default 0.8) to `ToolConfig`. `shared_budget()` reads config defaults; `budget::init_from_config()` syncs the shared budget from the live config at executor build time. `StratumSessionStartHook` now carries a `SharedConfig` and resolves mode from config with `STRATUM_MODE` env-var override. `config.toml.example` documents the three fields. Deferred-items table cleared of the two config-field rows. |
| WO 8.3: Bench task realism | Converted 5 real-repo tasks (add_adr, add_cli_flag, add_test_for_function, fix_clippy_warning, refactor_extract_function) to self-contained `setup_files` form. Added 4 new tasks that exercise plugin tools (use_stratum_compress, use_budget_check, use_draw_render, use_lsp_query). `use_workflow_run` deferred — no `Tool` impl exists for `kirkforge-workflow`. `build_bench_toolset` not extended (verify-only does not invoke tools). 13/24 tasks pass `verify-only` after this WO (up from 5/20). 11 pre-existing tasks still fail due to a flaw in their file_contains verify specs — out of WO 8.3 scope. |

### What shipped this session (Phase 8 — coverage + WO follow-ups)

| Item | What |
|---|---|
| WO 8.0: Raise coverage threshold for `src/session` | Bumped `src/session` tarpaulin threshold in `.github/workflows/ci.yml` from 61.0 to 62.0 after WO 7.2 added 20 real tests to the previously zero-test fold-in modules. Tarpaulin could not run to completion in the local sandbox within the WO's 5-minute budget (cold workspace compile alone exceeds it), so the threshold is set to the WO's documented fallback minimum; CI will catch any future regression below 62% on every push. |

### Deferred items (honest deferral)

| Item | Why deferred |
|---|---|
| `use_workflow_run` bench task | No `Tool` impl exists for `kirkforge-workflow`. The crate ships a library and a TUI slash command but the in-process `Tool` wrapper was never created. Belongs in a follow-up WO. |
| 11 pre-existing bench tasks (added in WO 7.8) fail `verify-only` | Their `file_contains` verify specs look for post-model substrings the setup file does not contain. Pre-existing flaw in those tasks; not part of WO 8.3 scope. |

### In-process hook infrastructure (shipped)

The hooks for WO 6.6/6.7/6.8 are now in-process Rust handlers (no shell scripts) built on shared infrastructure:

- `InProcessHook` trait in `src/session/hooks.rs`.
- `HookContext` struct with `tool_result` and `compact_stats` fields (replaces the env-var shim with real event context).
- `HookRunner.add_in_process_hook()` method.
- `HookRunner.run_with_context()` and `run_decision_with_context()` methods.
- `ToolOutcome.text_content()` helper in `src/shared/mod.rs`.
- `Executor::run_hook_with_result()` method; it and `run_compact_hook` pass the full `HookContext` to in-process hooks.

### Known CI issues

- **Ollama model pull fails intermittently**: The `integration` CI job fails when `ollama pull` encounters a registry redirect. External service issue; re-running typically succeeds.

### Gates

- `cargo test --locked --workspace --no-fail-fast` = all pass
- `cargo clippy --all-targets -- -D warnings` = clean
- `cargo fmt --check` = clean
- `cargo check --workspace --all-targets` = clean
- `cargo test -p plugin3-core --test adr_xref_drift` = 3 passed
- Feature-gated builds compile and pass

### Remaining (long-term, path to A agent)

| Item | Effort | Status |
|---|---|---|
| P1-long-1 Phase 7 — Embeddings/graph-walk retrieval | 2-3 weeks | Done (2026-07-24) |
| P1-long-2 follow-up (cont.) — Multi-model leaderboard | 1-2 weeks | Future |
| More TUI parity | ongoing | Future |