# kf-code Repo State

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`wo/20-integrate`** at commit `5eb3e9c`. All 20 `wo/20.*` workorder branches merged.

## Recent work (WO 20 integration — completed this session)

All `wo/20.*` branches are merged into `wo/20-integrate`. The final two:
- **wo/20.0.7** (`41d654f`): cache breakpoint cap-4 with tools (CRIT-1), Stratum offload-marker resolution in `store_get` (CRIT-2).
- **wo/20.2.0** (`5eb3e9c`): `tool_choice` + `max_tokens` adapter feature surface. Merged against integrate's stricter extended-thinking impl (kept `supports_thinking` guard + dedicated `budget_tokens`; rejected wo/20.2.0 reusing `max_tokens` as thinking budget). Took wo/20.2.0's cache-breakpoint algorithm (simpler + satisfies all tests). Kept integrate's 4-Option `adapter_for_with_provider` shape over wo/20.2.0's `ProviderConfig` refactor.

`CONFIG_FIELD_COUNT` 84→85 (ModelConfig gained `max_tokens`: 29→30 fields). Drift guard + merge_toml/env-override counts updated to match.

## Recent work (WO 23.8 R1+R2 — doom-loop circuit breaker)

- **23.8-R1**: Hard circuit-breaker: after `doom_loop_max_hits` cumulative doom-loop detections (default: 1), the executor auto-switches to plan mode. New `DoomLoopRemediation` `TurnEvent` emitted. `doom_loop_max_hits` config field (env: `KF_CODE_DOOM_LOOP_MAX_HITS`, TOML: `doom_loop_max_hits`). Setting to 0 disables the breaker (pre-WO behavior).
- **23.8-R2**: Hard halt: if already in plan mode when the circuit breaker fires, the turn is halted with an error message. `doom_loop_halt` flag on `Executor` causes early return from `record_tool_result`.
- `CONFIG_FIELD_COUNT` 88→89 (ToolConfig gained `doom_loop_max_hits`). Drift guard counts updated.
- 3 new tests: `doom_loop_circuit_breaker_auto_plan_mode`, `doom_loop_circuit_breaker_halts_in_plan_mode`, `doom_loop_circuit_breaker_disabled_when_zero`.

## Recent work (WO 18 + 19, review-4 cross-review)

Squashed commit `75f79f6` — review-4 findings, test debt WO 18–19, CI fixes.

### Review-4 cross-review fixes (WO 18.0)

**Critical / High:**

- **18.0.1**: All 4 `"kf-plugin-sdk3"` runtime gates replaced with `"kf-budget"` (budget subsystem was silently dead on default builds).
- **18.0.3**: Integration test `budget_tools_present_in_default_toolset` asserts budget tools register under default config.
- **H5**: `check_auth()` on every jobd request with constant-time comparison.
- **H6**: `ScheduledJob.timeout` enforced for both bash and workflow jobs via `tokio::time::timeout`.
- **H7**: `DaemonClient` reads auth token from `KF_CODE_DAEMON_TOKEN_FILE`; `InstanceRegister` sends the token.
- **H3 partial**: Unix rlimits always applied regardless of `harden` flag; `PluginToolWrapper` receives `effective_trust` from `HostedPlugin`.
- **M11**: Trust tier enforced at dispatch — ReadOnly plugin tools return `AccessDenied`.

**Medium:**

- **M5**: `VerifierHandler::verify_event` collects all findings instead of short-circuiting. Most severe wins.
- **M9**: `CompositeToolset` resolution order documented in code comment (builtin > MCP > plugin > stratum > draw > video > budget).
- **M10**: `load_one` now rejects invalid manifests (matches `load_from_dir` behaviour).
- **M13**: `WorkflowExecutor::run` decomposed into named sub-methods (`check_budget`, `run_step`, `handle_step_result`, `handle_fan_out`, etc.).
- **M15**: Serde field count assertion added alongside `CONFIG_FIELD_COUNT` — catches struct/TOML/env drift automatically.
- **M20**: Docker bind-mount source validated against canonical project root (symlink escape blocked).
- **M6**: `jobd` stale-socket guard — connects before removing, refuses to hijack a live socket.

**Low:**

- **L2**: `PostTurnHookGuard::drop` spawns the hook asynchronously instead of blocking.
- **L7**: Minify cache replaced with proper `LruCache` (HashMap + VecDeque, O(1) lookup, true LRU eviction).
- **18.0.2**: `/plugins toggle` shows restart notice for compiled-in plugins.
- **18.0.4**: `budget.rs` module doc updated from `plugins/kf-plugin-sdk3/tools/` to `plugins/kf-budget/tools/`.

### Test debt (WO 19 series)

| WO | What | Status |
|---|---|---|
| 19.1 | Testdoctor `diagnose` scans all source dirs (`--dirs`, `--with-coverage`) | Done |
| 19.2 | Public API surface metric (`api_surface`, `test_density`, `roi`) replaces line-count heuristic | Done |
| 19.3 | Test monolith surgery: split `tests_adr_0015.rs` (5,183 lines) into 8 focused files; split `kf-draw-core` state tests into 4 files; split `approval.rs` into `auto/deny/timeout` | Done |
| 19.4 | No-assertion tests upgraded: hooks, process group, budget helpers now assert actual behavior | Done |
| 19.5 | Integration tests: daemon auth token enforcement, budget registration gate, job lifecycle timeout | Done |
| 19.6 | E2E TUI scenarios (TmuxDriver harness) | Planned |
| 19.7 | Shared test support module (de-duplicate helpers) | Planned |
| 19.8 | Testdoctor crate-aware path resolution (`suggest-detailed` uses binary-to-path map from Cargo.toml) | Done |
| 19.9 | Flaky test stabilization: `yield_now` + `try_recv` replaces sleep/timeout; assertions on actual outcomes | Done |

### CI fix

- Windows: `Arc` import gated behind `cfg(unix)` in `daemon/mod.rs`.

## Config drift guard

- `CONFIG_FIELD_COUNT = 82` (ModelConfig=27, SecurityConfig=18, ToolConfig=26, SessionConfig=8, DisplayConfig=3)
- `MERGE_TOML_EXPECTED` and `ENV_OVERRIDE_EXPECTED` counters in drift test.
- New: serde field count assertion cross-checks `CONFIG_FIELD_COUNT` against `serde_json::to_value(&Config::default())` key count.

## Plugin architecture

Two-path dispatch (ADR-050). Folded plugins (stratum, kf-budget, kf-draw, kf-video) are compiled-in when their feature flag is on; shell fallback when off. Runtime `enabled_plugins`/`disabled_plugins` gate registration. `/plugins toggle` shows restart notice for compiled-in tools.

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --all-targets -- -D warnings`: PASS
- `#[test]` attr count: 3,936 across `src/` + `crates/`
- NOTE: `cargo test --workspace --no-fail-fast` could not complete in one run — the full workspace test OOMs/hangs under the shared opencode.db load (this is what killed the prior session: gitnexus MCP disconnect → instance disposal mid-`cargo-test`). Verified per-module instead: adapters 373 pass, config 80 pass (drift guard green), session 1471 pass — **0 merge-introduced failures**.

## Known pre-existing test failures (NOT from WO 20.2.0; present on `41d654f`)

- `bundled_node_sdk_tool_executes_via_host` (requires Node.js)
- `adapters::m5_tests::openai_cache_mode_marks_last_two_prefix_messages` — stale vs WO 17.5 system-breakpoint (test expects system unmarked; impl marks it).
- `tui::keys::slash_commands::tests::slash_command_table_covers_all_triggers` — `/mcp` (wo/20.10.0) not in the drift-test known set.
- `session::plugin_tools::*` (7), `tools::bash::*` (2), `session::session_index::test_append_alert_writes_ndjson`, `bundled_stratum_mode_tool_executes_via_host` — env/binary-dependent.

## Open review-4 findings (deferred)

| ID | Finding | Tier |
|---|---|---|
| H8 | Duplicate SSE frame-parsing logic (Anthropic + OpenAI) | 2 |
| H10 | Bedrock `extract_payload` O(n*m) backtracking | 2 |
| M17 | `kf-testdoctor::diagnose` hardcoded dirs (fixed in WO 19.1) | Done |
| M21 | `computer_use` near-identical `run_on_tab`/`run_on_session_sync` | 3 |
| L5 | `AppState` 44+ fields (God object) | 3 |
| L6 | `m5_tests.rs` in wrong directory | 3 |
| L9 | Ruby minifier strips only whole-line comments | 3 |
| L10 | `is_path_safe` doesn't reject backslashes/colons | 3 |

## Next steps (prioritized)

1. **WO 20.8.0 C3**: `src/session` coverage from 68.6% toward 75% — needs async executor + MCP-HTTP tests.
2. **WO 19.6**: E2E TUI scenarios via TmuxDriver harness.
3. **WO 19.7**: Shared test support module (de-duplicate 6 test helpers).
4. **H8**: Extract shared `parse_sse_frames` from Anthropic + OpenAI adapters.
5. **H10**: Optimize Bedrock `extract_payload` to avoid O(n*m) backtracking.
6. **Stratum absorption**: Remove `stratum` feature flag, make always-on.

## Rust toolchain

Rust 1.88.0 at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before cargo commands.

## Known issues

- `bundled_node_sdk_tool_executes_via_host` test fails (requires Node.js) — pre-existing.
- `adr_0010_emission_site_block_uses_eprintln_for_errors` in `kf-budget-core` — ADR vs impl drift, pre-existing.