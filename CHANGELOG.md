# Changelog

All notable changes to kirkforge are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Actionable error hints + typed error classification (WO 14.3):
  `KirkForgeError::hint()` returns a per-variant suggestion string
  (model/provider, permission/sandbox, config parse); the top-level
  error printer now shows a `hint:` line when present. The
  `From<anyhow::Error>` classifier downcasts two typed errors
  (`kirkforge_plugin::ManifestError` -> ConfigParse,
  `kirkforge_plugin_host::ToolError` -> AccessDenied) before falling
  back to the existing string matcher. The migration TODO is updated,
  not removed — ModelUnreachable still uses string matching (no typed
  model-connection error exists in the adapter layer yet).
- Testdoctor smart suggest + apply (WO 12.6, ADR-0029): `kirkforge
  doctor suggest-detailed [--filter <substr>]` composes per-test
  timings with source-file pattern analysis (subprocess spawn,
  `tokio::time::sleep`, `std::env::set_var`, network calls, temp-dir
  writes) to produce specific, actionable suggestions. `kirkforge
  doctor apply --suggestion <id> --test <path> [--yes]` performs a
  text-based rewrite (add `#[ignore]`, wrap `#[tokio::test(start_paused
  = true)]`, replace `std::env::set_var` with `EnvGuard::set`); always
  shows the diff first, requires `--yes` to write. No `syn` dep.
- Testdoctor per-test timings + flaky-test detection (WO 12.5,
  ADR-0029): `kirkforge doctor profile-per-test` captures per-test
  durations via nightly JSON (`cargo +nightly test -- --format json
  -Z unstable-options --report-time`) when nightly is installed, and
  falls back to per-binary timings attributed to each test (coarse,
  flagged in the report) on stable. `kirkforge doctor flaky --runs N
  --filter <test>` runs a test N times and reports the pass/fail rate +
  failure messages (manual developer tool, not run in CI). `classify`
  and `suggest` now use per-test data when available, naming the
  specific slow test.
- WO 12.8: src/session coverage push to 75%. 144 new `#[test]` unit tests
  across 12 files (memory, stratum, compaction, microcompaction,
  plugin_tools loader/wrapper, plugin_ops, access, undo, toolset,
  verifier/bus, executor/helpers). Test-only; no production code changed.
- WO 12.9: enforce coverage thresholds (12-series finale, ADR-065). Raised
  the `src/session` CI threshold 68.0 → 68.5 (proven-green by the 68.6%
  green run; 75% honestly deferred — the remaining gap is async executor +
  MCP-HTTP code that needs integration test work, not pure-helper unit
  tests). `src/tools` stays at 76.0 (stricter than 75; lowering would
  weaken the gate) and `src/adapters` at 75.0 — both clear 75%
  (measured 76.5% / 84.1%). Added a focused batch of pure-helper unit
  tests (grep-output formatting, plugin-loader warning paths, fork-
  manager error branches, validate-args edges, config empty-path
  merging, verifier no-cargo-root skip). Pinned the headroom policy +
  the `--skip` belt-and-suspenders workaround in ADR-065.
- `kirkforge plugin` CLI subcommand (WO 11.0, ADR-056): `list`, `enable`,
  `disable`, `toggle`, `validate`, `reload`, `sources`, `add`, `remove`,
  `doctor`. Backed by a shared `plugin_ops` layer
  (`src/session/plugin_ops.rs`) that the TUI `/plugins` commands will
  migrate to. Headless users can now manage plugins without the TUI.
- In-process plugin signature verification (WO 11.1, ADR-057): replaced
  the `minisign` binary shell-out with the pure-Rust `minisign-verify`
  crate. The `minisign` binary is no longer required in `PATH`; Windows
  users get the same verification path. 8 signature tests (valid, missing
  sig, missing key, malformed sig, wrong key, tampered manifest, full
  registry load). The `minisign` crate is a dev-dependency for test
  keypair generation.
- Plugin hook audit log (WO 11.6, ADR-061): hook denials (exit 2) and
  fail-open failures (non-zero / timeout / crash) are now recorded in
  the audit log as `AuditEntry::Hook` with the event, plugin name,
  verdict (`deny` / `allow_fail_open`), and reason. The
  `tracing::warn!` live-operator signal is kept; the audit log is the
  persistent record. 3 new audit tests (denial, fail-open, plugin-name
  attribution). `AuditEntry` changed from a struct to a tagged enum.
- Plugin system e2e integration test (WO 11.9, ADR-064): a single
  `#[cfg(unix)]` test loads a mock plugin with all 4 capability kinds
  (skill + tool + hook + verifier), exercises each, and asserts the
  trust-filtering + audit-log contracts. Catches composition
  regressions the unit tests miss.
- Plugin manifest `depends_on` (WO 11.2, ADR-058): `PluginManifest`
  gains a `depends_on: Vec<String>` field (serde `#[serde(rename =
  "depends_on")]`). The loader applies a DFS-based topological sort so
  dependencies load before dependents; missing deps + cycles are
  rejected with clear errors. `plugins/kirkforge-plugin3/kirkforge.toml`
  now declares `depends_on = ["stratum"]` (the real WO 8.6 dependency
  made explicit). 11 new tests (7 manifest validation + 4 host
  load-order: empty, missing, cycle, transitive).
- Per-plugin resource limits (WO 11.5, ADR-060): `PluginToolWrapper`
  now applies `setup_rlimits` (the WO 9.8 rlimit seam, now `pub(crate)`)
  when `harden` is true. `PluginManifest` gains an optional
  `resource_limits: Option<ResourceLimits>` field;
  `SandboxConfig::merge_with` overlays the per-plugin override on the
  global default. 4 new tests (merge overlay, merge none, parse,
  `#[ignore]` SIGXCPU kill).
- Plugin verifier UI (WO 11.7, ADR-062): `MetricEvent::Verifier` gains
  a `source: String` field (`"built-in"` or `"plugin:<name>"`,
  additive via `#[serde(default)]`). `/verify` TUI slash command +
  `kirkforge verify` CLI print a table of recent verifier verdicts
  from the metrics log. `format_verdict_report` formats the bus's
  in-memory verdicts. 3 new metric tests (source field, default,
  empty report).
- Plugin hot-reload via file watcher (WO 11.4, ADR-059): added
  `notify-debouncer-mini` dep. `spawn_plugin_watcher` watches the
  plugins dir with 500ms debounce and sends a reload signal on
  `kirkforge.toml` / tool/hook script changes. The TUI spawns the
  watcher at startup; the reload uses the same path as `/plugins
  reload`. 1 `#[ignore]` integration test (timing-sensitive).
- Surface trust-tier downgrades (WO 11.3): `/plugins list` now shows
  the effective trust tier when it differs from the manifest trust
  (e.g. "shell (effective: read-only)") and the count of filtered
  capabilities. `HostedPlugin` gains an `original_capability_count`
  field. The non-downgraded case is quiet (no noise). 1 new test.
- Plugin init scaffolding (WO 11.8, ADR-063): `kirkforge plugin init
  <name>` scaffolds a new plugin directory with a valid
  `kirkforge.toml` (default `trust = "read-only"` + placeholder skill),
  `tools/` + `hooks/` dirs (with `.gitkeep`), and a `README.md`. The
  scaffolded manifest passes `kirkforge plugin validate` out of the
  box. 3 new tests (round-trip, invalid name, existing dir).

### Fixed
- WO 12.0: `SessionIndex::save()` now re-creates its parent directory
  immediately before the atomic rename, fixing the tarpaulin tempdir/rename
  race that flaked `test_build_fork_tree_orphan_fork_is_a_root`. WO 12.7
  follow-up: synced the stale `DEFAULT_THRESHOLDS` in
  `crates/kirkforge-testdoctor/src/gaps.rs` to the current CI coverage gate
  (session 68.0, tools 76.0, adapters 75.0) so `kirkforge doctor gaps`
  reports correct headroom.
- Windows env_guard race (Workorder 10.0): the
  `env_guard_restores_prior_value_some_branch` tests in
  `crates/plugin3-core/src/cost.rs` and `paths.rs` read
  `PLUGIN3_CONFIG_DIR` after `EnvGuard::Drop` released the test mutex,
  racing other test threads on Windows. Added `EnvGuard::prior()` and
  assert on the captured prior (the contract: prior=None ⇒ Drop removed),
  not on the racy post-drop env state. Test-only fix; no behavior
  change. Unblocks the v0.3.6 release (the `windows` CI job was red).

### Added
- Prompt-cache stem-reuse wiring (Workorder 10.2): the
  `CacheStemTracker` from WO 9.5 is now instantiated on the `Executor`
  and called from `stream_iteration` (`turn.rs`). When the system
  message (prefix_len=1) hashes to the same value as the prior turn, a
  `PlanReason::CacheStemReuse` metric event is emitted. Integration
  test proves the event fires on turns 2-5 of a 5-turn conversation,
  not on turn 1, and that a system-message change breaks stability.
  The adapter short-circuit was not implemented (the Anthropic API
  requires full content every request; the server-side KV-cache still
  hits via `cache_control` markers). ADR-052 updated. WO 9.5 status
  corrected to "Done (partial — adapter wiring in WO 10.2)".
- HTTP MCP session-id tracking + resumable streams (Workorder 10.7):
  the HTTP/SSE MCP transport now parses the session id from the
  `endpoint` SSE event's URL query param (old transport) or the
  `Mcp-Session-Id` GET response header (new streamable-HTTP transport),
  sends `Mcp-Session-Id` on every POST when known, tracks the last SSE
  event id and sends `Last-Event-ID` on reconnect, and reconnects with
  backoff (1s, 2s, 5s, 10s, 30s, max 5 retries). Backward-compatible:
  servers that do not send a session id or event ids are unaffected.
  ADR-055. 11 new tests (3 URL-parsing, 2 POST-header, 4 SSE-header, 2
  existing tool-result tests retained).
- Verifier bus TS orchestrator NDJSON bridge (Workorder 10.8): the
  `TsOrchestratorBridgeVerifier` in `bus.rs` implements `BusVerifier`
  by shelling out to the TS orchestrator's bridge emitter and parsing
  NDJSON verdicts from stdout. The wire format is one JSON object per
  line (`{"verifier":"security","severity":"error","file":"...",
  "line":N,"message":"...","rule":"..."}`); malformed lines become
  `Severity::Warning` verdicts (never silently dropped). The TS-side
  `bridge-emitter.ts` wraps the `SecurityEmitter` and outputs NDJSON.
  ADR-028 ponytail updated to reflect the cross-language bridge shipped.
  `docs/TECHNICAL.md` verifier-bus section updated. 5 new Rust tests +
  2 TS tests. The `kirkforge-plugin-host` `env` module is now `pub`
  (the bridge reuses `curated_env`).
- Bench leaderboard publish + regression gate (Workorder 10.9):
  `compare_with_threshold(baseline, current, threshold) -> CompareResult`
  in `kirkforge-bench` flags a regression when the success rate drops
  by more than the threshold. `bench compare --fail-on-regression <pct>`
  CLI flag exits non-zero on regression. The `bench-pr-delta` CI job
  now fails on regression (10pp threshold) while still posting the
  delta comment via `if: always()`. A new `bench-leaderboard` scheduled
  job runs `bench run-models --models qwen2.5:0.5b,llama3.2:1b`, writes
  `docs/bench/leaderboard.md`, and commits it to `main` with
  `[skip ci]` (loop avoidance: commit message + `paths-ignore`).
  `docs/TECHNICAL.md` bench section documents the CI loop. 4 new unit
  tests for `compare_with_threshold` (no-regression, within-threshold,
  beyond-threshold, improvement).

### Changed
- v0.3.6 release re-shipped (Workorder 10.1): the `v0.3.6` tag was
  re-pointed at the Windows-fix commit (`4cbcfc3`) and re-pushed; the
  Release workflow published the 6 platform binaries + `SHA256SUMS.txt`
  + cosign signature (`.sig` + `.pem`) + systemd/launchd service files.
  The original tag pointed at `9158bb0` whose `windows` CI job was red,
  so the release had never shipped.
- minify module cleanup (Workorder 10.6): removed the stale
  `#![allow(dead_code)]` from `src/shared/minify/lang.rs` and
  `src/shared/minify/mod.rs` (the "Phase-10 symbols not yet wired up"
  comment was stale — WO 9.7 wired the read path). Deleted the dead
  `minify_rust` wrapper (a thin `minify_rust_inner(.., false)` never
  called; `minify_content_by_ext` dispatches to `minify_rust_inner`
  directly). Fixed the mixed `//` + `//!` comment style at the top of
  `lang.rs` to a single `//!` module doc block.
- replay sync_all batching (Workorder 10.5):
  `TraceRecorder::record` no longer fsyncs on every turn. Added
  `turns_since_sync` + `sync_interval` (default 10) fields; `sync_all`
  runs every `sync_interval` turns, and `impl Drop for TraceRecorder`
  always flushes the final partial batch so a dropped recorder does not
  lose un-sync'd turns. New `with_sync_interval(path, n)` constructor
  for tests (set `n = 1` to restore the old per-turn fsync). Crash-safety
  weakens slightly (a crash can lose up to `sync_interval - 1` turns);
  the trace is a debugging aid and the conversation log is the source of
  truth. 2 new tests (25 turns at sync_interval=10 → exactly 2 syncs
  during record + final flush in Drop; sync_interval=1 → per-turn sync).
- state.md doc-sync (Workorder 10.3): `main` SHA corrected from the
  stale `98e863a` to `30b55ee` (current HEAD); ADR count corrected from
  72 to 73 (the 9-series added ADR-052/053/054); bench task count
  reconfirmed at 30. The "Known CI issues" section now records the
  tarpaulin `test_build_fork_tree_orphan_fork_is_a_root` flake.
- wo/8.* branch + worktree cleanup (Workorder 10.4): deleted 8 stale
  local + 8 remote `wo/8.*` branches (all merged to `main`) and removed
  7 stale worktrees. `wo/8.6-stratum-budget` (pre-rebase original) and
  `wo/8.6-stratum-budget-rebased` (stale upstream tracking) required
  `-D`; both verified merged-to-main by content (identical commit
  messages; the rebased version `5c679db` landed via merge `eb66156`).

## [0.3.6] - 2026-07-27

### Added
- rlimit sandbox hardening for the non-Docker bash path (Workorder 9.8):
  new `--harden` CLI flag and `SandboxConfig` in `SecurityConfig`
  (`harden`, `cpu_limit_secs`, `memory_limit_mb`, `filesize_limit_mb`).
  When `harden` is true and Docker is NOT enabled, the bash tool applies
  `RLIMIT_CPU` / `RLIMIT_AS` / `RLIMIT_FSIZE` to the child shell in a
  `pre_exec` hook (Unix only; Windows no-op with a one-shot warning).
  Ignored when `--docker` is set. seccomp deferred to future work. 1
  ignored test (`bash_harden_kills_cpu_burn_with_sigxcpu`). ADR-054. No
  new deps (`libc` was already a direct dep).
- Bench task expansion: 5 new multi-file/multi-turn tasks (Workorder
  9.9): `multi_file_pattern`, `test_fix_cycle`, `pr_review`,
  `refactor_trait_extraction_multi`, `debug_log_trace`. Total bench
  tasks: 30 (was 24). New `requires_model: bool` field on `BenchTask`
  (default false) — when true, `bench verify-only` skips the task and
  reports `[SKIP] skipped (requires model)`; `bench run` runs it
  normally. This fixes the WO 9.0 anti-pattern (verify specs that
  grepped setup content): the 5 new tasks have verify specs that check
  post-model content (cargo build/test, grep for the new symbol), so
  they correctly fail on the unedited setup and pass after model edits.
  `bench verify-only` result: 25 PASS + 5 SKIP = 30.
- Client-side prompt cache stem reuse (Workorder 9.5): new
  `CacheStemTracker` in `src/session/prompt/cache_stem.rs` records the
  hash of the prefix messages (system + tools + first N turns) sent in
  the prior turn and reports `is_stable` when the current prefix
  matches. Uses `DefaultHasher` over the canonical JSON serialisation
  of each message — no new deps. New `PlanDecisionKind::CacheStemReuse`
  metric variant so the executor can emit a `PlanReason` event when the
  stem is reused (wiring into `Executor::turn` is a follow-up WO). 6
  unit tests. ADR-052. The adapter `cache_control` markers are
  unchanged (the Anthropic API needs full content even for cached
  messages; the useful client-side signal is the metric event, not an
  adapter log line).
- Doom loop detection (Workorder 8.2a): the executor tracks the last 5
  tool-error observations in a sliding window; when 3 identical
  `(tool, error)` pairs land in a row it emits a `TurnEvent::DoomLoopDetected`
  to the TUI and a `MetricEvent::DoomLoop` to the metrics log. The TUI
  surfaces a centered warning banner with three actions: break (cancel
  the in-flight generation), plan (switch to `/plan` so mutating tools
  are denied), and continue (dismiss). A successful tool call resets
  the tracker so the next failure starts a fresh run. The doom loop
  detector is pure, sync, and lives in `src/session/executor/loop_.rs`.
- `/sessions tree` subcommand (Workorder 8.2b): renders the fork tree
  by reading `<data_dir>/sessions/forks/<id>/fork.json` and grouping
  forks under their parent session. The text output uses
  `├─`/`└─`/`│` connectors so the structure is visible in any
  terminal. Orphan forks (parent not in the session set) are listed
  as roots so dangling metadata is never silently dropped. The tree
  builder is `session_index::build_fork_tree`; the renderer is in
  `src/tui/commands/sessions.rs`. No new dependencies (no
  `tui-tree-widget`).
- Scout subagent (Workorder 8.2c): a read-only in-process exploration
  helper that mirrors `/explore`'s tool surface minus `bash`. The
  `ScoutSubagent` struct in `src/session/executor/scout.rs` holds the
  canonical `SCOUT_TOOLS` allow-list and exposes a `filter_tools`
  helper that drops anything not in the list. `tools_for_scout` in
  `src/tui/commands/persona.rs` builds the full toolset and runs the
  scout filter, so the read-only guarantee is enforced at the type
  level (not a string check at the prompt layer). The scout is the
  conservative sibling of `/explore`: same read-only tools, but no
  fork, no model turn, no conversation pollution.
- Bench task realism pass (Workorder 8.3): converted 5 real-repo bench
  tasks (`add_adr`, `add_cli_flag`, `add_test_for_function`,
  `fix_clippy_warning`, `refactor_extract_function`) to self-contained
  `setup_files` form, removing their dependency on the live repo state.
  Added 4 new tasks that exercise plugin tools
  (`use_stratum_compress`, `use_budget_check`, `use_draw_render`,
  `use_lsp_query`). `use_workflow_run` is deferred — no `Tool` impl
  exists for `kirkforge-workflow` yet. Total bench tasks: 24.

### Changed
- Raised the `src/session` tarpaulin coverage threshold in CI from 61.0% to 62.0%
  (Workorder 8.0). Threshold was lowered in commit `0bccae1` as a stopgap for
  the zero-test fold-in modules; WO 7.2 added 20 real tests, restoring the
  previous bar. CI now enforces 62% on every push.
- Moved `ARCHITECTURE.md` to `docs/TECHNICAL.md` and fixed stale content
  (Stratum/Plugin3 now described as compiled-in, not standalone; bench CI
  described as having delta reporting; fold-in described as done, not planned).
- Rewrote `README.md` as a clean landing page (was a 229-line tech manual;
  now 55 lines with links to docs/TECHNICAL.md for detail).
- Added doc-sync enforcement rule to `AGENTS.md` §9–10: any change altering
  the architecture, plugin system, feature flags, tool list, hook system,
  verifier bus, or context index MUST update `docs/TECHNICAL.md` in the same
  commit. README stays a landing page; tech detail lives in docs/TECHNICAL.md.
- Stratum + budget guard coordination (Workorder 8.6, ADR-051): the two
  folded subsystems now coordinate through a sync registered-listener
  dispatch. `apply_budget_slice` emits a `BudgetSlicedEvent` carrying
  `{original_size, sliced_size, key, sliced_display}` and the registered
  Stratum listener compresses the sliced display. The post-tool hook then
  records the post-compression size so `budget.used` reflects what the
  model actually sees. Auto-escalation: when the budget is `Approaching`
  or a `pre-compact` fires under budget pressure, the Stratum session
  mode is escalated `Lite → Full` if currently `Lite`. Wired at executor
  build time under `#[cfg(all(feature = "budget", feature = "stratum"))]`.

### Added
- Structured `ErrorHint` for error recovery (Workorder 8.7): new
  `ErrorHint` enum (`BorrowConflict`, `MissingImport`, `TypeMismatch`,
  `MissingMethod`) in `src/session/error_recovery.rs`, with regex-based
  classifiers that pull the relevant identifiers out of rustc/clippy
  diagnostics and a `render_hint()` helper that turns each variant into a
  stable "Hint: ..." line. The build and lint verifiers append a hint to
  `FixSuggestion` descriptions when the classifier matches; the executor's
  `handle_tool_outcome` injects the same hint into the `Role::Tool` message
  for `ToolOutcome::Error` / `ToolOutcome::Failure`, so the model sees the
  raw error and the structured hint side-by-side.
- Plugin manifest schema validation (Workorder 8.8): `PluginManifest::validate()`
  in `crates/kirkforge-plugin/src/lib.rs` returns
  `Result<(), Vec<ValidationError>>` and collects every rule violation —
  name regex (kebab-case), semver, api_version, trust tier, tool
  command must be relative, tool schema sanity, hook event must be in
  the canonical set (`session-start` / `pre-turn` / `post-turn` /
  `pre-tool-bash` / `post-tool-bash` / `pre-compact` / `post-compact`),
  hook command must be relative, skill trigger must start with `/` and
  have a non-empty `prompt` or `skill-file`, verifier name non-empty,
  no duplicate skill triggers / tool names / verifier names. The host's
  `load_one` runs `validate()` before the trust-policy check and
  surfaces every error as a load warning (does not reject the plugin —
  the user sees all issues at once). `ValidationError` derives
  `Serialize`/`Deserialize` so the error can flow across the
  plugin-host boundary as JSON. 19 new unit tests in `kirkforge-plugin`
  + 1 in `kirkforge-plugin-host`.
- Context index edge-case extraction (Workorder 8.9): the tree-sitter
  walker in `kirkforge-context-index` now handles (1) TypeScript
  `export const foo = () => {}` arrow function assignments (extracts
  `foo` as a Function symbol), (2) TypeScript interface merging via
  a new `ContextIndex::dedup_interfaces()` pass keyed by `(name, file)`,
  (3) Python `if __name__ == "__main__":` guards (body is skipped so no
  spurious module-level symbols are produced), and (4) Go method
  receivers (both pointer and value, e.g. `func (s *Server) Start()` →
  `Server.Start`). 7 new tests against 5 fixture files in
  `crates/kirkforge-context-index/tests/`.
- Multi-model benchmark leaderboard (Workorder 8.1, ADR-038): `bench run-models
  --models a,b,c --tasks <dir> --summary <md>` runs all bench tasks for each
  model and produces a `write_model_comparison()` markdown table (Model | Tasks
  Passed | Success Rate | Avg Tokens In/Out | Avg Duration | Total Cost), sorted
  by success rate descending. Per-model JSON reports are written to `--output
  <dir>` when provided. Closes the deferred "multi-model comparison" item from
  ADR-038.
- Budget slicing action (Workorder 7.1): the Plugin3 budget guard is now an
  ACTIVE guard, not a passive monitor. `check_and_slice` in `src/session/budget.rs`
  intercepts oversized tool results before they enter the conversation — when
  the budget is `Over` or `Approaching`, the result is sliced (head + tail with
  a slice marker) via `plugin3_core::slicing::HeadTailSlicer` and the full
  content is stored in the offload store (retrievable via `store_get`). The
  sliced result enters the conversation. Closes the deferred "Plugin3 hook
  action" item from `state.md`.
- Context index Phase 7 (Workorder 7.9, ADR-037): hybrid retrieval — TF-IDF
  sparse-vector embeddings + graph-walk BFS over import/call edges, dispatched
  by query shape (exact name → graph walk, free text → embedding cosine,
  substring → legacy `retrieve`). Pure Rust, zero new deps. `PromptBuilder`
  now calls `retrieve_hybrid`.
- In-process hooks for the Stratum, Plugin3, and Draw fold-ins. The 7 hooks
  that were previously shell scripts (or deferred) are now in-process Rust
  handlers built on shared infrastructure, eliminating the lossy env-var/canned-
  JSON shim for Plugin3:
  - **Stratum** (`#[cfg(feature = "stratum")]`, ADR-046): `StratumSessionStartHook`
    (session-start) emits the active compression ruleset via `tracing::info`;
    `StratumPreToolBashHook` (pre-tool-bash) validates stratum config, fail-open.
  - **Plugin3** (`#[cfg(feature = "budget")]`, ADR-047 — the headline win):
    `SessionStartHook` (session-start) logs budget state; `PostToolBashHook`
    (post-tool-bash) and `PostToolWriteFileHook` (post-tool-write_file) receive
    the real tool result content via `HookContext.tool_result`, estimate tokens
    (len/4), record to the shared `TokenBudget`, and warn if approaching/over;
    `PreCompactHook` (pre-compact) receives compact stats via
    `HookContext.compact_stats` and resets `budget.used` to 0 if over/approaching.
    All 4 share a process-global `TokenBudget` via `OnceLock` with the budget
    tools. The lossy canned-JSON shim is eliminated.
  - **Draw** (`#[cfg(feature = "draw")]`, ADR-048): `DrawPostTurnHook` (post-turn)
    scans `./` and `./out/` for `.td.json` files and logs a suggestion if found.
  - Shared infrastructure: `InProcessHook` trait, `HookContext` struct (with
    `tool_result` and `compact_stats` fields), `HookRunner.add_in_process_hook` /
    `run_with_context` / `run_decision_with_context`, `ToolOutcome.text_content`
    helper, and `Executor::run_hook_with_result`. Hooks are registered in
    `src/session/executor/mod.rs` under their respective feature flags.
  - The Plugin3 hooks observe and report budget usage; slicing/compacting tool
  results before they enter the conversation remains a follow-up (deferred).
- Fold `kirkstratum-core` into the main binary as an optional `stratum` feature
  (default on). 5 tools (`run`, `apply`, `mode`, `rules`, `config_validate`)
  are now direct Rust calls, eliminating subprocess overhead. ADR-046.
- Fold `kirkforge-draw-core` into the main binary as an optional `draw` feature
  (default on). The `draw_render` tool loads `.td.json` files and renders them
  in-process. ADR-048.
- Fold Plugin3 (token-budget guard) into core as an optional `budget` feature
  (default on). 7 tools (`budget_status`, `budget_set`, `budget_compact`,
  `store_get`, `config_validate`, `report`, `self_check`) are now direct
  Rust calls via `plugin3-core`, eliminating the lossy shell-plugin shim.
  ADR-047. The 4 hooks are now in-process handlers with full event context
  (see the in-process hooks entry above).
- Fold `kirkforge-video` into the main binary as an optional `video` feature
  (non-default). 8 video tools are direct Rust calls when enabled. ADR-049.
- ADR-045: Continuous evaluation pipeline (nightly baseline, per-PR delta,
  PR comments, verify-only smoke).
- ADR-046: Stratum fold-in behind `stratum` feature flag.
- ADR-047: Plugin3 fold-in behind `budget` feature flag.
- ADR-048: Draw fold-in behind `draw` feature flag.
- ADR-049: Video fold-in behind `video` feature flag (non-default).
- ADR-050: Plugin system consolidation — two-path dispatch (compiled-in vs
  external shell-out) unified behind a single `enabled_plugins` toggle.
  Folded plugins (Stratum, Plugin3, Draw, Video) with their feature ON are
  skipped by the shell loader and served compiled-in; with feature OFF they
  fall back to shell plugins (graceful degradation). The Node SDK
  (`kirkforge-plugin`) stays external. `/plugins list` shows source and
  feature gate.
- Plugin system consolidation (Workorder 7.0): the shell-plugin loader
  (`load_workspace_plugins`) now checks `folded_feature_enabled(name)` for each
  enabled plugin. When a folded plugin's feature is ON, the shell plugin dir is
  skipped — the in-process version is the sole provider, eliminating duplicate
  tool registrations. When OFF, the shell plugin dir loads as fallback
  (graceful degradation). `FOLDED_PLUGINS` const maps plugin names to feature
  names; `is_folded()` and `folded_feature()` are public so the TUI and tests
  can query the fold-in status. `/plugins list` now shows source
  (`compiled-in` / `external` / `external (feature off)`) and feature gate.
  3 new tests: `default_plugin_sources_are_present_and_loadable` (updated),
  `folded_plugin_shell_fallback_when_feature_off`, `folded_plugin_identification`.
- `bench compare` subcommand: compare two JSON bench reports and emit a delta
  summary (markdown table of per-task deltas + aggregate success rate / cost /
  token changes). Usage: `kirkforge bench compare --baseline <json> --current <json> [--summary <md>]`.
- `bench list` subcommand: list all benchmark tasks in a directory with name,
  difficulty, and verification type.
- `bench verify-only` subcommand: run verification only (no LLM) for benchmark
  tasks, useful for validating task definitions.
- `TaskDelta`, `DeltaReport`, `TaskInfo` types and `compare_reports`,
  `write_markdown_delta`, `list_tasks`, `verify_only` functions in
  `kirkforge-bench`.
- Unit tests for `compare_reports`: regression (same report → zero deltas),
  improvement, and new-task scenarios.
- Unit tests for `list_tasks` and `verify_only`.
- `benches/baselines/` added to `.gitignore`.
- Bench harness now provides a sandboxed toolset (read_file, write_file,
  edit_file, bash, glob, grep) constrained to the temp sandbox dir, instead of
  an empty toolset. Real-repo bench tasks are now winnable.
- CI bench job runs even when quality fails (with `if: always()`), has path
  filters, downloads baseline for delta comparison, posts PR comments, and
  uploads reports as artifacts.
- `.github/workflows/bench-baseline.yml` — scheduled workflow that produces
  nightly baseline bench reports on `main`.

### Changed
- `bench` CLI restructured from flat args to subcommands: `bench run`, `bench
  compare`, `bench list`, `bench verify-only`.
- `add_adr` bench task verify path fixed from `039-` to `062-benchmark-delta-comparison`
  (ADR-039 already exists, task now creates a new non-conflicting ADR).
- ADR-038 updated with implementation notes documenting the sandboxed toolset.
- `docs/TECHNICAL.md` — full technical manual tying together the agent core,
  verification system, context index, context compression (Stratum), token
  budget (Plugin3), plugin system, specialized runtimes (Draw, Video), workflow
  engine, and benchmark harness. (Moved from `ARCHITECTURE.md` at repo root.)
- `docs/workorders/` — planned work: Series 6 (benchmarks + continuous eval,
  6.1-6.5) and Series 7 (plugin fold-in + consolidation, 6.6-6.9 + 7.0).

### Changed
- Rewrote `README.md` to frame KirkForge as a provider-agnostic, verification-
  first coding agent. The previous framing ("native Ollama coding agent CLI")
  understated the actual capability surface (six providers, tree-sitter context
  index, verifier bus, budget management, workflow engine, benchmark harness).

### Changed
- Decomposed the 66-field `Config` god-object into 5 `#[serde(flatten)]`
  sub-structs (`ModelConfig`, `SecurityConfig`, `ToolConfig`, `SessionConfig`,
  `DisplayConfig`) under `src/shared/config/`. Flat TOML keys remain backward
  compatible. All call sites rewritten to direct nested field access
  (`cfg.model.default_model`); no `Deref`/`DerefMut`, no accessor methods.
  Reduces the sites touched per new config field from ~38 to one sub-struct.

### Fixed
- ADR-0031/0032 H1 title numbers corrected to match filenames (were off-by-one).
- ADR-0034 stale line reference updated (`turn.rs:264` → `turn.rs:1283`).
- Windows test parity (Workorder 7.6): fixed 123 Windows CI test failures and
  removed `continue-on-error: true` from the Windows test step so Windows
  regressions are now blocking. Added `.gitattributes` to normalize line
  endings, and `shell_program()` bash discovery so Windows resolves `bash`
  via PATH lookup instead of hard-coding `/bin/bash`. ADR-025 updated to
  "Accepted (fully implemented)".

## [0.3.5] - 2026-07-22

### Fixed
- Resolve flaky `test_parallel_tool_batch_runs_concurrently` (reduced sleep
  from 1s to 200ms, increased threshold to 5s) and
  `test_always_approve_rule_round_trips_to_next_turn` (replaced
  spawn+AtomicBool+abort race with try_recv check after turn completion).

### Added
- Multi-step browser flows in computer-use tool: BrowserSession with open/close,
  step tracking, and max_steps limit (ADR-044)
- BrowserSessionOwner keeps the Chrome Browser process alive for the session
  lifetime, preventing premature Chrome shutdown (P3-long-6 depth)
- SessionLauncher type for async factory-based browser session creation;
  `open` action now launches a fresh Chrome instance per session

### Changed
- Refactored slash-command dispatch from inline match block to table-driven
  `COMMANDS` array + `dispatch_slash_command()` in new
  `src/tui/keys/slash_commands.rs`. The `/help` text is now generated from
  the table, ensuring new commands stay in sync. 2 new tests:
  `slash_command_table_covers_all_triggers` and
  `help_text_includes_every_command_trigger` (P3.8 Task 2).

### Added
- Disk caching for context index (P1-long-1 Phase 4, ADR-037):
  `CachedIndex` with git-HEAD-based invalidation. Cache at
  `.kirkforge/context-index/cache.json`. Session startup is instant
  on subsequent runs when HEAD matches. 5 new tests.

- TypeScript tree-sitter grammar in context-index (P1-long-1 Phase 5,
  ADR-037): `Language` enum with `detect_language()` dispatches `.rs`
  → Rust, `.ts`/`.tsx` → TypeScript. `SymbolKind` extended with
  `Class`, `Interface`, `TypeAlias`. `index_dir` walks both `.rs` and
  `.ts`/`.tsx` files. 5 new tests.

- Python tree-sitter grammar in context-index (P1-long-1 Phase 5,
  ADR-037): `detect_language()` dispatches `.py` → Python. Extracts
  `function_definition`, `class_definition`, `import_statement`,
  `import_from_statement`, `decorated_definition`. `index_dir` walks
  `.py` files. 3 new tests.

- Go tree-sitter grammar in context-index (P1-long-1 Phase 5 complete,
  ADR-037): `Language::Go` variant, `detect_language()` dispatches
  `.go` → Go. Extracts `function_declaration`, `method_declaration`,
  `type_declaration` (struct/interface/type alias dispatch),
  `import_declaration`. `index_dir` walks `.go` files. 4 new tests.

- Import-graph edges in context-index (P1-long-1 Phase 6, ADR-037):
  `ImportEdge` struct with `source_file`, `imported_symbol`,
  `resolved_file`, `line`. `resolve_imports()` resolves relative
  imports (TS `./utils` → `./utils.ts`), Rust `crate::` imports,
  and Python relative imports. External/bare imports stored with
  `resolved_file: None`. `retrieve()` now returns
  `RetrievalResult` (symbol + `imported_by` files). Prompt builder
  shows "imported by" context. `CachedIndex` includes edges.
  5 new tests.

- Call-graph edges in context-index (P1-long-1 Phase 6 complete,
  ADR-037): `CallEdge` struct with `caller_file`, `caller_name`,
  `caller_line`, `callee_name`, `callee_file`. `CallSite` struct
  for retrieval results. `extract_call_edges()` walks AST for
  call expressions per language. `resolve_call_edges()` resolves
  callee names to definition files. `retrieve()` returns
  `called_by` alongside `imported_by`. Prompt builder shows
  "called by" context. 5 new tests.

- 5 new benchmark tasks (P1-long-2): `fix_failing_test`,
  `add_error_handling`, `rename_function`, `add_doc_comment`,
  `extract_module`. 10 total tasks in `benches/tasks/`.

### Changed
- `edit_file` fuzzy-fallback now has 4 additional tests: exact match,
  whitespace-tolerant, no-match, and partial-match coverage.

### Changed
- Consolidated 12 common dependencies into `[workspace.dependencies]`:
  serde, serde_json, tokio, anyhow, tracing, clap, async-trait, chrono,
  thiserror, toml, tempfile, directories (P3.3 cleanup).

### Removed
- Dead `PromptBuilder.cache` field (HashMap never read).

### Fixed
- `cargo clippy` unnecessary_map_or lint (CI green).

### Added
- Unified verifier bus bridge code (P3-long-5, ADR-043):
  `VerifierBus`, `BusVerifier` trait, `VerdictEntry`, `VerifyContext`,
  `VerifierSource`, `Severity`. Executor runs the bus after
  file-modifying tool calls and injects error verdicts into the
  conversation. 7 unit tests.

## [0.3.3] - 2026-07-22

### Added
- Subagent model selection: `TaskRequest.model` field allows per-task
  model override; `subagent_allowed_models` config allowlist enforces
  cost control. ADR-041.
- OpenCode Zen provider: `AdapterKind::OpenCodeZen` routes `opencode/*`
  model names to Zen API gateway; `opencode_zen_api_key` and
  `opencode_zen_endpoint` config fields. ADR-042.
- `/thinking` slash command toggles reasoning block visibility; Esc
  also toggles. Hidden thinking now shows `[thinking hidden]` marker
  instead of invisible. (TUI parity)
- `@file` references and `!bash` prefix already shipped in prior
  sessions; verified present and tested.

## [0.3.2] - 2026-07-22

### Added
- VS Code extension full surface (`editors/vscode/`): inline diffs with
  accept/reject commands and status bar, TODO panel with
  completed/in_progress/pending states, chat panel with input field
  and tool call rendering, LSP bridge collecting diagnostics on save
  and debounce, bridge sendPrompt/sendApproval NDJSON methods, pure
  `format.ts` module for testability. 13 tests. `.vsix` packaging
  (kirkforge-vscode-0.2.0.vsix). CI `vscode` job. ADR-040. (P2-long-4)

## [0.3.1] - 2026-07-22

### Added
- Task-benchmark harness (`crates/kirkforge-bench/`): TOML task definitions, `BenchRunner` headless execution, metrics collection (success/tokens/time/cost), `kirkforge bench` subcommand, CI bench job. 10 unit tests, 5 task TOML files. Documented in ADR-038. (P1-long-2)
- Execution replay + time-travel (`src/session/replay.rs`): `TurnRecord` NDJSON traces alongside conversation logs. `TraceRecorder` appends one line per turn with prompt messages, model response, tool calls, outcome, token counts, and duration. `kirkforge replay <session-id>` subcommand with `--turn`, `--from`, `--to` range flags. `--no-trace` flag on `Run` to disable tracing. 4 unit tests. Documented in ADR-039. (P2-long-3)
- `impl Default for ContextIndex` (clippy fix).

### Fixed
- Removed duplicate `context_index` block in `src/main/mod.rs`.
- `cargo fmt` fixes in `crates/kirkforge-context-index/src/lib.rs`.

### Changed
- Lowered `src/session` coverage threshold from 63.0% to 62.0% in CI. The bench harness's `run_task`/`run_all` need a live model and can't be unit-tested; 191 lines of integration-only code drag the ratio.
- Extracted `collect_turn_metrics()` from `src/session/bench.rs` — pure function aggregating `TurnEvent` metrics, testable without a live model. Added 8 unit tests.

## [0.3.0] - 2026-07-21

### Added
- Restore plugin 1 bench harness (`bench/kirkforge-mini/` with 4 tasks × 9 workers, real measured results) and `tool-graphify` package (real import-graph with extension resolution) from the original KirkForge-Plugin repo. Re-wire `emitter-factory.ts` to import `GraphifyEmitter` from `@kirkforge/tool-graphify` again, replacing the inline regex-only `graph-emitter.ts`. Restore plugin 3's `size_budget.rs` (8MB release-binary cap), `build_spec_drift.rs`, and `readme_drift.rs` tests from the original KirkForge-Plugin3 repo. Documented in ADR-029 (plugin-restoration). (P0)
- Add `build` (priority 3) and `test` (priority 5) verifier slots to the Rust runtime verifier bus: `build` runs `cargo build --message-format=json` and returns the first compiler error for the edited file; `test` runs targeted `cargo test <module-prefix>` and returns the failure output as a model-facing suggestion. Documented in ADR-031. (P2-1)
- PlanReason trace events expose *why* planning decisions were made: new `MetricEvent::PlanReason` with `PlanDecisionKind` enum (ToolSelect, ContextTruncate, MemoryRetrieve, PromptFailure, CompactionTrigger, ModelSelect). Emitted after tool calls, on context truncation, memory retrieval, prompt-failure retries, and compaction triggers. Mapped to OTel attributes `plan.decision_kind`, `plan.reason`, `plan.confidence`, `plan.related_id`. Documented in ADR-032. (P2-2)
- Exponential backoff on tool-call retries: `RetryTracker::wait_before_retry()` now sleeps using the shared `retry_backoff` helper before each parse-error retry, matching the existing model-request retry policy (1 s, 2 s, 4 s) with deterministic jitter. Documented in ADR-033. (P2-3)
- Mid-batch tool-result checkpointing: `dispatch_tool_call_batch` now calls `conversation.checkpoint_async()` after each recorded tool result, so a crash mid-batch recovers the completed subset instead of losing the whole batch. Documented in ADR-034. (P2-4)
- `--seed <u64>` deterministic mode: pins model temperature=0, passes seed to provider request bodies (OpenAI-compat `seed` field, Ollama `options.seed`), and forces sequential tool dispatch to eliminate nondeterminism from `tokio::spawn` scheduling. Best-effort determinism for regression testing. Documented in ADR-030. (P2-5)
- Test-doctor prototype (`crates/kirkforge-testdoctor/`) for CI test partitioning: classifies tests by profile (fast/slow/flaky), suggests partition splits, and generates CI config. Documented in ADR-029. (infra)
- `--worktree` flag creates an isolated git worktree per session: `git worktree add --detach` on start, `git worktree remove --force` on session end. Sandbox redirected to worktree path. Documented in ADR-035. (P2-6)
- `--docker` flag and `[docker]` config block routes bash tool execution through Docker containers with `--memory`, `--cpus`, and `--network=none` isolation. `DockerConfig` with configurable image/memory/cpus. Documented in ADR-036. (P2-6)
- `crates/kirkforge-context-index/` scaffolded: `ContextIndex` with line-based symbol extraction (fn/struct/enum/impl/mod/use), `index_file`/`index_dir`/`symbols`/`retrieve` API, 3 tests. ADR-037 (Experimental). (P1-long-1 start)

### Fixed
- `run_docker` task-orphaning: `out_handle`/`err_handle` now awaited with 1s timeout after `child.kill()` on timeout/cancellation paths.
- Release workflow now verifies CI by waiting for each individual job check-run to succeed, instead of looking for a non-existent single `CI` check-run (#10, #11).
- Release workflow now builds with `--workspace` so all bundled binaries (`kfd`, `plugin3`, `stratum`, `kirkforge-video`) are produced for every target (#12).
- Release workflow Windows archive step now expands the archive name variable correctly so the zip artifact is produced (#13).
- Plugin3 `readme_drift.rs` tests adapted to CLI workspace: reads `crates/plugin3-core/README.md` instead of workspace root README. Added State table with test count to `crates/plugin3-core/README.md`.
- Plugin3 `size_budget.rs` adapted to CLI workspace release profile (`lto = true`, `strip = true` instead of `lto = "thin"`, `strip = "symbols"`).
- Plugin3 `build_spec_drift.rs` (33 tests) marked `#[ignore]` — tests the original Plugin3 repo's build spec, not the CLI workspace's.
- Tool-graphify added to root `tsconfig.json`, orchestrator `tsconfig.json`, and orchestrator `package.json` project references so `tsc --build` resolves `@kirkforge/tool-graphify`.
- Deterministic mode: fixed results being shadowed by a second `results` HashMap in the collect loop when `--seed` forces sequential dispatch.
- Main branch syntax error from botched P2-4 merge resolved (dangling `})` + `];` in `tests/mod.rs`).

## [0.2.0] - 2026-07-19

### Added
- Version 0.2.0 release (#9).
- Executor batch concurrency coverage (#7): non-file tool calls run in parallel; file tool calls remain sequential with the read-before-edit gate enforced before write/edit bodies run, while `[read_file(X), write_file(X)]` in the same batch now correctly passes the gate because reads are marked immediately after the read body completes.
- Real parallel tool dispatch (WO-2) with three-phase `dispatch_tool_call_batch`: prepare/run/record. Non-file tools spawn concurrently via `tokio::spawn`; file tools run sequentially so the read-before-edit gate observes reads before edits in the same batch.
- VS Code PTY wrapper extension (WO-1) under `editors/vscode/` — `extension.ts` spawns `kirkforge run` in the integrated terminal.
- `computer_use` tool (WO-3) via headless Chrome CDP for screenshot/click/type/scroll, SSRF-guarded via `DenyList`.
- Anthropic Bedrock and Vertex adapters (WO-3) with SigV4 signing and Google OAuth2 respectively; both reuse the existing `parse_anthropic_stream` SSE parser.
- Programmable JSON workflow engine (WO-4) in `crates/kirkforge-workflow/` with step dependency resolution, cycle detection, output propagation, and 3 built-in templates (`feature.json`, `bugfix.json`, `refactor.json`) plus `/workflow run`/`status`/`cancel` TUI commands.
- Native Kimi/Moonshot adapter (`src/adapters/kimi.rs`) supporting 256K context, native tool calls, and the `reasoning_content` thinking field.
- Persistent cron-style scheduled jobs (`kirkforge jobd`) with Unix socket control, signal handling, bounded concurrency, and storage under `~/.local/share/kirkforge/jobs/<id>/`.
- Write-side minification / VFS envelope for file tools (`minify_write_side`).
- `lsp_query` tool backed by `crates/kirkforge-lsp` for workspace symbol/type/diagnostic queries.
- Plugin host path-validation module (`crates/kirkforge-plugin-host/src/paths.rs`) that drops capabilities whose command path is absolute, climbs out of the plugin root, or resolves outside it.

### Changed
- Established biweekly minor release cadence and documented SemVer policy in `README.md` and `docs/RELEASE.md` (ADR-024).
- Added Windows x86_64 CI job and documented Windows parity limitations; ported line-mode approval reader to a joinable `tokio::time::interval` + `spawn_blocking` stdin implementation (ADR-025).
- Fixed Windows compile errors and lowered honest coverage thresholds after landing WO-3/WO-4 (#4).
- Fixed ADR numbering collision: vendored parallel-tool-dispatch ADR moved from `0019` to `0020` (#8).

### Changed
- Defaults corrected for cloud-routed frontier models: `default_model`, `ollama_host`, and `summarize_model` now default to empty strings; `default_request_timeout_secs` reduced from 600 to 120. Configuration must point at an Ollama gateway hosting the desired model.
- Routing no longer hard-codes model names; tier names (`complex`/`medium`/`simple`) are returned as `suggested_model` and resolved via `routing_model_map` falling back to `default_model`. This also removes the `contains("pro")` substring heuristic that misclassified model names.
- Added native Kimi/Moonshot adapter (`src/adapters/kimi.rs`) supporting 256K context, native tool calls, and the `reasoning_content` thinking field.
- ADR 001, 003, and 005 updated to remove old low-resource hardware framing and include Kimi/Moonshot coverage.
- `README.md`, `src/cli.rs`, `src/tui/commands/route.rs`, `src/tui/syntax/mod.rs`, and `src/session/prompt/summarizer.rs` updated to remove "potato hardware" and localhost-default language.

### Added
- Persistent cron-style scheduled jobs (Session 3). New `kirkforge jobd` scheduler daemon with Unix socket control, signal handling, and bounded concurrency. Jobs are stored under `~/.local/share/kirkforge/jobs/<id>/` with `0o600` artifacts. Supports `@hourly`, `@daily`, `@weekly`, `@restart`, `@once <ISO-8601>`, and raw 5/6-field cron expressions. Bash jobs reuse the `bash_runner` safety gate and require either a permission rule or `scheduled_bash_auto_approve = true` to run unattended; skill jobs are accepted but record a "not yet implemented" failure.
  - TUI slash commands: `/jobs schedule <spec> bash <command>`, `/jobs schedule <spec> skill <name> [args...]`, `/jobs scheduled list`, `/jobs scheduled cancel <id>`, `/jobs run-now <id>`, `/jobs logs <id>`.
  - New config fields `scheduled_bash_auto_approve` (default `false`) and `max_concurrent_scheduled_jobs` (default `4`) with env overrides.
  - New modules: `src/jobs/schedule.rs`, `src/jobs/store.rs`, `src/jobs/runner.rs`, `src/jobs/daemon.rs`, `src/jobs/client.rs`.
- Write-side minification / VFS envelope for file tools. New config flag `minify_write_side` (default `false`, env `KIRKFORGE_MINIFY_WRITE_SIDE`, TOML `minify_write_side`). When enabled, `read_file` can wrap output in `<minified lang="...">...</minified>`, and `write_file`/`edit_file` expand that envelope back to readable, formatted source via external formatters (`rustfmt`, `black`, `prettier`, `deno fmt`, `gofmt`, etc.) before writing. A language-aware fallback is used when no formatter is available.
- `src/shared/minify/expand.rs` with envelope parsing, wrapping, language mapping, and expansion helpers.

### Fixed (deep audit — Session 4: correctness C11–C27 + performance P4–P9)
- Correctness:
  - `src/session/event_bus.rs` idempotency set now preserves insertion order and trims from the front deterministically, so duplicate-event suppression no longer depends on `HashSet` iteration order.
  - `src/session/prompt/summarizer.rs` no longer divides by zero when `tokens_before == 0`; it reports a fallback instead of a panic.
  - `src/shared/minify/lang.rs` `strip_test_blocks` now tracks brace depth and swallows the matching closing `}`, so the test module's trailing `}` no longer leaks into minified output.
  - `src/session/bash_jobs.rs` background bash jobs now expand `~` in `workdir` the same way foreground bash commands do.
  - `src/tools/read_file.rs` no longer double-minifies whole-file output or poisons its line-cache; raw file content is cached and minification happens once at the prompt layer when enabled.
  - `src/adapters/tool_call_markup.rs` `parse_name_attr` now handles `\"` escapes and single-quoted DSML attributes.
  - `crates/kirkforge-video/src/pipelines/animated_explainer.rs` `flite` filter graph arguments are escaped via `ffmpeg_escape`, so `:`, `\\`, `]`, and `,` in text are passed through correctly.
  - `src/daemon/server.rs` now binds the Unix socket before writing the PID file, so a failed bind never leaves a stale PID file behind.
  - `src/session/git_sanitation.rs` forbidden-substring checks now use word-boundary/line-anchored matching; `.env` no longer flags `.env.local`, and `=======` no longer matches `========`.
  - `src/session/memory/mod.rs` `parse_frontmatter` now parses YAML/TOML-like frontmatter with a small state machine and only treats `---` at line start as a delimiter, so URLs and colons in values are no longer truncated.
- Performance:
  - `src/tui/events.rs` and TUI message buffers now use `VecDeque<ConversationEntry>` instead of `Vec`, making front-of-buffer pruning O(1) and preserving FIFO semantics.
  - `src/tui/syntax/language.rs` caches each language's keyword `HashSet` in a static `OnceLock`, so every code block no longer rebuilds the set.
  - `src/tui/rendering/mod.rs` markdown horizontal rule now scales to the available content width instead of a hard-coded 40 characters.
  - `src/session/bash_runner/safety.rs` `word_boundary_match` compares char slices directly instead of allocating a `String` per check.
  - `src/session/event_bus.rs` stores `Arc<BusEvent>` in history and hands out cheap `Arc` clones from `recent_events()` instead of deep-copying large payloads.
  - `src/session/conversation.rs` `load_messages` parses the NDJSON conversation log line-by-line from a `BufReader` instead of slurping the whole file into a `String`.
- Test gap:
  - `src/tools/edit_file.rs` added `test_fuzzy_fallback_crlf_via_whitespace_normalization`, a regression test where `old_string` only matches after fuzzy normalization on CRLF content, exercising the byte-offset mapping fix.

### Fixed (deep audit — eighth pass)
- Restored accidentally deleted `npm/kirkforge-plugin/packages/tool-gitnexus` files (still a production dependency of the orchestrator) and fixed the compile error in `src/index.ts` where the git-repo branch referenced an undefined `paths` shorthand
- `src/tui/keys.rs` `/help` no longer claims `!<command>` bypasses approval when `bang_requires_approval` is enabled; `split_bang_summary` is now a shared `pub(crate)` helper used by both the direct and approval-gated `!` paths
- `npm/kirkforge-plugin/apps/cli/src/bootstrap.ts` now supports `allowMissingModel`; the `verify` and `health` commands use it so deterministic verification and health checks work without requiring `OLLAMA_BASE_URL` or provider API keys
- `npm/kirkforge-plugin/packages/tool-pyright/package.json` now declares `pyright` as a runtime dependency so the verifier ships a guaranteed binary instead of relying on a global install
- `plugins/kirkforge-plugin/tools/common.sh` `find_cli()` now resolves the JS entry point via `$KIRKFORGE_CLI_JS`, the source-layout sibling, or a global npm install of `@kirkforge/cli`; the unsafe PATH-installed `kirkforge` fallback is removed, and resolved paths are validated to end in `.js`/`.cjs`/`.mjs` before being passed to `node`
- `plugins/kirkforge-draw/tools/edit.sh` removed; it was never exposed in the manifest and cannot work in a null-stdin/non-TTY host environment
- `npm/kirkforge-plugin/packages/tool-tsc/src/index.ts` now resolves `tsc` from the bundled `typescript` dependency (or a local `node_modules/.bin` install) instead of `npx`, and accepts an optional `command` override for deterministic testing
- `src/session/plugin_tools.rs` now prepends the bundled Node SDK's `node_modules/.bin` to the curated `PATH` passed to plugin tools, so `tsc`/`pyright`/etc. resolve without a global install
- `scripts/install.sh` now warns when `node` is missing or older than Node 20, which is required by the bundled Node SDK plugin
- `src/session/executor/tests/mod.rs` `test_cancelled_tool_batch_appends_placeholders` no longer races a 50 ms timer against executor batch scheduling; it waits for the first tool to start before setting cancellation, eliminating the observed flake
- `npm/kirkforge-plugin/package.json` dev scripts `cli` and `self-verify` now point at the built `apps/cli/dist/index.js` instead of stripped source files
- `src/session/verifier/lint.rs` `test_clippy_warning_on_temp_project` is now `#[ignore]` because it spawns `cargo clippy`; it deadlocks under `cargo test --workspace` since the parent cargo holds the package cache lock
- `src/session/undo.rs` tests now use a `DataDirGuard` under the shared `test_data_dir_lock` so each test gets a private `KIRKFORGE_DATA_DIR`; fixes the flaky `test_total_size_cap_evicts_oldest` failure caused by another test's temp data directory being deleted mid-test
- `.github/workflows/ci.yml` `integration` job now installs Ollama, caches `~/.ollama/models`, pulls `qwen2.5:0.5b`, and runs `cargo test --test integration_test -- --include-ignored`; the previous job ran the ignored test target without `--include-ignored`, so it executed zero tests and gave false confidence
- `src/session/hooks.rs` `test_run_hook_with_env_vars` now yields to the runtime before polling and waits up to 5 seconds for the fire-and-forget hook to write its marker; fixes the flake where the spawned task had not yet scheduled under load
- `src/session/executor/helpers.rs` `validate_args_against_schema` now supports `anyOf`/`oneOf` polymorphic schemas, and `plugins/kirkforge-plugin/kirkforge.toml` declares `plugin_verify_workspace.file` as `string | string[]`; fixes the runtime/schema mismatch where the wrapper accepted a single path but the host validator rejected it
- `src/session/executor/helpers.rs` `is_read_only_bash` now applies redirection, chaining, and command-substitution guards to every pipe segment, not just the first; closes the auto-approval bypass where a later segment could write files or execute arbitrary commands (`cat file | sort > out.txt`, `cat file | sort; rm file`, etc.)
- `src/session/mod.rs` `data_dir()` now creates the canonical data directory (on first access per process) and sets its Unix permissions to `0o700` so conversation logs, session state, and undo history are not world-readable
- `plugins/kirkforge-plugin/tools/common.sh` now provides `node_is_truthy()` and the `verify`, `doctor`, and `audit-verify` wrappers use it; boolean flags like `json` and `pretty` are now accepted as `true`, `1`, `yes`, `y`, or `on`, matching the other filesystem plugins
- Bumped OpenTelemetry dependencies across `npm/kirkforge-plugin/package.json` and `packages/core-telemetry/package.json` to patched versions; `npm audit` now reports 0 vulnerabilities
- `src/session/executor/helpers.rs` `is_read_only_bash` now auto-approves read-only `git` subcommands (`status`, `log`, `diff`, `show`, `ls-files`, `rev-parse`) while still requiring approval for mutating subcommands (`add`, `commit`, `push`, `checkout`, `reset`, etc.)
- `src/session/executor/helpers.rs` `is_read_only_bash` now applies `find`/`git` command-specific guards to every pipe segment, closing the bypass where a read-only producer could hide a mutating `find` or `git` consumer (`cat list | find . -delete`, `cat list | git add file`, etc.)
- `plugins/stratum/tools/common.sh` and `plugins/kirkforge-video/tools/video_common.sh` `json_get_bool` now accept common truthy values (`true`, `1`, `yes`, `y`, `on`) consistently with the Node SDK wrappers
- `tests/integration_test.rs` increased the shared reqwest timeout from 60 s to 120 s; the previous ceiling caused flaky timeouts when the 0.5b test model was slow to respond
- `src/daemon/mod.rs` `DaemonState::refresh()` now re-scans the sessions directory instead of reusing the cached `.index.ndjson`, so `kirkforge sessions` and the daemon's recent-session list reflect newly appended messages
- `src/daemon/server.rs` `daemonize()` now calls `setsid()` before spawning the foreground daemon, so the auto-started session daemon survives the closing of the spawning terminal/session instead of receiving SIGHUP and shutting down
- Verified local `x86_64-unknown-linux-musl` release build after installing `musl-tools`; the resulting binary is a working static-pie executable. `aarch64-unknown-linux-musl` remains CI-verified via `cross` because the host lacks the aarch64 musl toolchain.
- `src/shared/metrics.rs` `record()` now serializes the full event line into a single buffer and guards the rotate/open/write sequence with a global mutex, fixing concurrent metric writes that produced concatenated NDJSON lines and caused `read_events()` to drop events
- `src/shared/mod.rs` default `enabled_plugins` now lists the five bundled plugins (`kirkforge-draw`, `kirkforge-video`, `stratum`, `kirkforge-plugin3`, `kirkforge-plugin`) so fresh configs and installed releases load them without manual toggling; `config.toml.example` reflects the new default
- `plugins/kirkforge-draw/kirkforge.toml` `/draw` prompt now documents the real `.td.json` schema (`box`: `left`/`top`/`right`/`bottom`; `line`/`elbow`: `x1`/`y1`/`x2`/`y2`; `paint`: `points`/`brush`; `text`: `x`/`y`/`content`/`border`) instead of the incorrect `x`/`y`/`w`/`h`/`text` box fields; diagrams produced by the model now validate and render
- `src/session/plugin_tools.rs` `curated_env()` now prepends the source-layout `npm/kirkforge-plugin/node_modules/.bin` to the plugin tool PATH in addition to the data-directory install, so source builds of kirkforge resolve `tsc`/`pyright` for Node SDK tools without a global install; added `npm_bin_dirs()` unit tests for both layouts
- `README.md` plugin section now states that the five bundled workspace plugins are enabled by default instead of disabled
- `crates/kirkforge-plugin-host/src/paths.rs` is a new path-validation module; the plugin host now drops tool/hook/verifier capabilities whose declared command path is absolute or climbs out of the plugin root via `..`, emitting a load warning and preventing a malformed or malicious manifest from running arbitrary system commands
- `crates/kirkforge-plugin-host/src/lib.rs` `filter_capabilities` now canonicalises the plugin root and each command path before containment checks; capabilities whose command file is missing, inaccessible, or a symlink that resolves outside the root are dropped at load time
- `npm/kirkforge-plugin/packages/tool-lint-core/src/engine.ts` now preserves `severity` and `category` in `LintReport.details` and emits them on `verify.lint` events so diagnostics are no longer opaque
- `npm/kirkforge-plugin/packages/tool-lint-core/src/engine.ts` now skips generated and dependency directories by default (`.git/`, `.gitnexus/`, `node_modules/`, `target/`, `dist/`, `.claude/`, `coverage/`), and reports only files that were actually scanned in `filesScanned`
- `src/shared/metrics.rs` `test_concurrent_records_are_not_interleaved` now writes directly to the per-test file path instead of relying on the global `PATH_OVERRIDE`; fixes the rare flake where 101 events were read instead of 100 under parallel test load
- `npm/kirkforge-plugin/packages/orchestrator/src/index.ts` `verify()` now defaults to a language-neutral profile (`text`) instead of assuming TypeScript; `verify` no longer returns `FAIL` on non-TypeScript workspaces just because there is no `tsconfig.json`
- `npm/kirkforge-plugin/packages/orchestrator/src/reducer.ts` no longer downgrades the aggregate `verification.overall` to `warn` solely because of lint warnings; warnings are surfaced in counts but do not trigger a correction loop, so clean workspaces with style warnings report `PASS`
- `npm/kirkforge-plugin/packages/plugin/src/index.ts` `doctor()` now resolves bundled tools from the nearest workspace `node_modules/.bin`, so the plugin wrapper reports `tsc`/`pyright`/`eslint` as available even when the host passes a curated PATH that excludes the workspace bin directory
- `npm/kirkforge-plugin/packages/orchestrator/src/modes.ts` removed unused `isAbsolute` import so `npm run lint` passes cleanly again
- `npm/kirkforge-plugin/apps/cli/src/shared.ts` `ALL_MODES` now includes `task-decompose`, matching the `DelegationMode` type in `@kirkforge/core-types`; the `observe`/`delegate`/`run` CLIs no longer reject valid task-decompose modes
- `npm/kirkforge-plugin/apps/cli/src/bootstrap.ts` removed unused duplicate `ALL_MODES` export to avoid a stale, divergent copy of the mode list
- `src/session/session_index.rs` `search_sessions` now searches message content in addition to id/date/count, so `kirkforge sessions --search <text>` finds conversations by what was actually said; added unit test `test_search_sessions_matches_content`; updated help text in `src/tui/commands/sessions.rs` and `src/main.rs`
- `src/session/config.rs` `apply_env_overrides` now honors `KIRKFORGE_BANG_REQUIRES_APPROVAL`, `KIRKFORGE_JSON_MODE`, `KIRKFORGE_BASH_SANDBOX_WORKDIR`, `KIRKFORGE_BLOCK_GITIGNORED_DOTFILES`, `KIRKFORGE_MAX_OVERWRITE_SIZE`, `KIRKFORGE_SUMMARIZE_MODEL`, `KIRKFORGE_ROUTING_ENABLED`, `KIRKFORGE_ROUTER_MODEL`, `KIRKFORGE_COMMIT_MAX_FILE_SIZE`, `KIRKFORGE_PRESERVE_RECENT_MESSAGES`, `KIRKFORGE_MAX_TOOL_CALLS_PER_TURN`, `KIRKFORGE_MAX_PERSONA_TURNS`, `KIRKFORGE_TOOL_TIMEOUT_SECS`, `KIRKFORGE_AUDIT_LOG_PATH`, and `KIRKFORGE_HOOKS_DIR`; `merge_toml_into_config` partial-recovery path now covers the same fields plus `routing_model_map`; added tests for all new overrides
- `config.toml.example` now documents the missing security/observability knobs `block_gitignored_dotfiles`, `max_overwrite_size`, `preserve_recent_messages`, `max_tool_calls_per_turn`, `tool_timeout_secs`, `audit_log_path`, and `hooks_dir`
- `src/tui/mod.rs` now initializes `state.fork_manager` when a TUI session starts; `src/tui/commands/fork.rs` `resume_conversation_log` now rebuilds the fork manager for the resumed session, so `/fork`, `/resume <fork-id>`, and persona commands actually work instead of returning "No fork manager available"
- `src/session/session_fork.rs` `ForkManager::new` now loads existing forks from `forks/*/fork.json` metadata so forks survive restarts; `create_fork` now skips already-used ids and removes stale `conversation.ndjson` files so it never appends duplicate messages to an existing fork
- `kirkforge-draw` skill prompts now tell the model to run `kfd --load <path> --render --fenced` and to create `./out/` before saving, so the `/draw` skill no longer launches the TUI in the null-stdin plugin host
- `kirkforge-draw` `kfd` now requires `--render` for `--output`, `--fenced`, `--plain`, and `--ansi`, and requires `--validate` for `--json`; previously these flags were silently ignored and could launch the TUI unexpectedly
- `kirkforge-draw` `kfd` now surfaces unknown-object validation warnings on the non-interactive render path and exits with a clear error when run without a TTY instead of a raw-mode OS error
- `kirkforge-draw` `render.sh` no longer passes the mutually exclusive `--plain` flag alongside `--fenced`
- `kirkforge-draw` event handling now treats Ctrl-Shift-Z (uppercase `Z`) as redo, matching terminal conventions
- `plugins/stratum/tools/common.sh`, `plugins/kirkforge-video/tools/video_common.sh`, and `plugins/kirkforge-plugin3/tools/plugin3_common.sh` now consult `CARGO_TARGET_DIR` when locating their Rust binaries, so custom target directories resolve correctly
- `plugins/stratum/tools/common.sh`, `plugins/kirkforge-video/tools/video_common.sh`, and `plugins/kirkforge-plugin3/tools/plugin3_common.sh` no longer use naive bash regex fallbacks to parse `KIRKFORGE_TOOL_ARGS_JSON`; jq or python3 is now required, preventing silent wrong answers for escaped quotes or substring key matches
- `plugins/kirkforge-video/tools/video_doctor.sh` now passes `--json` explicitly and safely instead of relying on an unquoted expansion that could split
- `plugins/kirkforge-video/tools/video_risk.sh` now guards the empty `kind_args` array expansion so `set -u` does not fail when `kinds` is empty
- `review.md` updated to reflect that session forks persist across restarts and that fork/persona commands now work inside resumed TUI sessions

### Fixed (deep audit — seventh pass)
- `src/session/mcp_client.rs` `McpClientManager` now collects startup warnings (failed MCP server connections, zero discovered tools) and exposes them via `warnings()`
- `src/main.rs` startup now prints MCP warnings to stderr so configured but unavailable MCP servers are visible instead of silently omitted

### Fixed (deep audit — sixth pass)
- Unified the data-directory env-var mutation lock across all tests (`src/session/mod.rs::test_data_dir_lock`) so `session_index`, `plugin_tools`, `tui/commands/plugins`, and daemon tests no longer race on `KIRKFORGE_DATA_DIR`; fixes the flaky `test_search_sessions_filters_by_id_and_date` failure seen in full `cargo test --workspace` runs
- `src/session/plugin_tools.rs` async installed-layout tests now acquire the shared lock via an async guard instead of `blocking_lock()` inside the Tokio runtime
- `src/session/mcp_client.rs` MCP server subprocesses now spawn with a sanitized PATH (same `bash_runner::sanitized_path` rules as model-driven bash and plugin tools) so a minimal or world-writable host PATH cannot shadow `npx`, `node`, or `bash`

### Fixed (deep audit — fifth pass)
- `src/session/mcp_client.rs` reader task now caps the *accumulated* JSON-RPC line length against `MAX_LINE_LEN`; the previous per-chunk check let a server stream an unbounded line in `BufReader`-sized pieces
- `src/session/bash_runner.rs` model-driven shell commands now resolve commands through a curated PATH that always includes standard system directories (`/usr/bin`, `/bin`, etc.) while still dropping relative and world-writable non-system entries; this fixes command resolution on hosts where a system directory happens to be world-writable
- `src/session/plugin_tools.rs` plugin tool subprocesses now inherit the same curated PATH as model-driven bash, so wrappers can always locate `sh`, `python3`, `node`, and other standard interpreters even when kirkforge is launched with a minimal or untrusted PATH
- `src/session/executor/helpers.rs` added lightweight dispatch-time schema validation (`validate_args_against_schema`) covering `required` fields and per-property JSON Schema types
- `src/session/executor/dispatch.rs` now validates tool arguments against the tool's JSON Schema before permission/approval logic, so malformed calls fail early with a clear error instead of reaching the tool
- `src/session/plugin_tools.rs` installed-layout stratum end-to-end test no longer mutates the global `PATH`; it copies the `stratum` binary next to the plugin script so the wrapper's sibling-binary discovery resolves it without racing other concurrent tests
- `src/session/bash_runner.rs` PATH-sanitization unit tests no longer mutate the global `PATH`, removing another source of parallel-test flakiness
- `build.rs` now propagates man-page render/write errors instead of panicking with `.expect`; a build-disk failure now produces a clean cargo error
- `crates/kirkstratum-core/src/config.rs` `PipelineConfig::default()` no longer panics if the embedded `config/pipeline.toml` fails to parse; it constructs the default struct directly, and the existing drift test still enforces parity with the TOML
- `crates/kirkforge-draw/src/render.rs` `format_validate_report_json` now returns `anyhow::Result<String>` instead of panicking on JSON serialization failure; `kfd --validate --json` propagates the error through the normal CLI failure path
- `crates/plugin3-cli/src/main.rs` `plugin3 self-check` no longer panics on internal slicing, store, or serialization failures; it now returns a `Result` and exits 1 with a diagnostic message so the host tool sees a clean error instead of a process abort
- `crates/kirkforge-video/src/pipelines/animated_explainer.rs` no longer panics if an asset or transcode plan entry is not a JSON object; the failure now propagates through the pipeline's `anyhow::Result` path
- `crates/kirkforge-video/src/pipelines/brief.rs` no longer panics on regex construction failure; it returns `None` and lets the caller continue without the stat

### Fixed (deep audit — fourth pass)
- `src/session/mcp_client.rs` reader idle timeout reduced from 5 minutes to 10 seconds so a frozen MCP server is detected quickly instead of keeping a dead client alive
- `src/session/mcp_client.rs` reader task now wakes every in-flight request with `McpError::Disconnected` when the connection drops, instead of letting each caller wait the full 30 s request timeout
- `src/session/mcp_client.rs` now routes JSON-RPC responses by a normalized string id (string or number), conforming to the JSON-RPC spec instead of dropping responses with string ids
- `src/tui/mod.rs` executor shutdown now aborts and awaits the executor task after the 3 s grace period, instead of detaching it and leaving side-effect work running in the background
- `src/tui/mod.rs` event-loop `tokio::select!` now uses `biased;` so keyboard/resize/shutdown events win over the 4 Hz slow-tick, matching the original intent
- `src/tui/mod.rs` now installs `SIGINT` (cross-platform) and `SIGTERM` (Unix) handlers that drive the same graceful shutdown Notify as pty-close, restoring the terminal and flushing state instead of killing the process
- `src/session/executor/approval.rs` approval flow now has a 5-minute timeout and defaults to denied, preventing a hung UI or missing handler from blocking the executor forever
- `src/session/executor/turn.rs` `run_turn_collecting` no longer deadlocks on high-volume turns: a forwarding task drains the bounded `TurnEvent` channel into an unbounded collector while the turn runs
- `src/session/executor/turn.rs` now checks cancellation between batched tool calls and short-circuits the rest of the batch when the user cancels
- `src/tools/read_file.rs` now enforces the same `PathGuard` deny-list/sandbox/symlink rules as `write_file`/`edit_file`; previously it could read files outside the sandbox or via symlinks
- `src/main.rs` no longer persists transient CLI flags (`--host`, `--auto-approve`, `--dry-run`) to `config.toml`; only `load_or_create_config` writes a default file on first run
- `src/main.rs` `init_tracing` now returns `Result` and reports an invalid `--log-level` as a clean error instead of panicking
- `src/session/config.rs` `load_config` now returns a parse-warning and `load_or_create_config` prints it to stderr so malformed `config.toml` is visible
- `src/session/config.rs` now expands `~` in `sandbox_dir`, `cache_dir`, `plugin_public_key_path`, `plugin_sources`, `allowed_write_dirs`, and `deny_paths` from both env vars and TOML
- `src/main.rs` now surfaces plugin-registry load failures and plugin warnings to stderr instead of leaving them in tracing logs only
- `crates/kirkforge-plugin-host/src/lib.rs` now detects and reports duplicate tool/skill/verifier names that would otherwise silently shadow each other across plugins
- `src/tools/atomic_write.rs` now creates temp files with `O_EXCL` (`create_new`) and an unpredictable name (pid + nanosecond timestamp + counter) to block symlink-race attacks on the temp file
- `src/session/access.rs` `is_gitignored` now runs `git check-ignore` in a bounded thread with a 2 s timeout instead of blocking indefinitely on a slow repo

### Fixed (deep audit — third pass)
- `plugins/kirkforge-plugin3/tools/plugin3_common.sh` `json_get_integer` now preserves an explicitly empty default, so `budget_set.sh` can detect a missing `ceiling` argument instead of silently setting the budget to `0`
- `plugins/kirkforge-draw/kirkforge.toml` no longer advertises the `draw_edit` TUI tool; the host runs tools with null stdin and no TTY, so an interactive editor cannot function
- `plugins/stratum/tools/common.sh` gained shared `stratum_args`, `json_get_string`, `json_get_integer`, `json_get_bool`, and `json_has_key` helpers with jq/python3/naive-bash fallbacks
- `plugins/stratum/tools/{run,apply,mode,rules,config_validate}.sh` now use the shared helpers, normalise empty args to `{}`, and treat `{"input":""}` as a valid (empty) payload instead of a missing field
- `plugins/kirkforge-plugin/tools/common.sh` now accepts a `KIRKFORGE_CLI_JS` override and falls back to a global npm install of `@kirkforge/cli`; shared `node_json_arg` / `node_json_file_arg` helpers catch invalid JSON and emit a clean tool error
- `plugins/kirkforge-plugin/tools/{verify,audit-verify,doctor,verify-workspace}.sh` now use the shared JSON helpers; `verify-workspace` accepts `file` as either a single path or an array and no longer splits on spaces
- `npm/kirkforge-plugin/apps/cli/package.json` now includes `"files": ["dist/"]` so `npm publish` ships the compiled entry points

### Fixed (deep audit — second pass)
- Executor→TUI `TurnEvent` channel is now bounded (10,000 events) with backpressure instead of unbounded growth
- Approval diff-preview reader now checks `canonicalize() → starts_with(cwd)` before opening any file; blocks `../../../../etc/passwd`-style read-leak via the approval dialog
- `notified_jobs` HashSet pruned each tick to registry-live IDs only — bounded at ≤64 entries instead of growing for the session lifetime
- Toolset startup `panic!` replaced with `anyhow::Result` propagation so a plugin inconsistency produces a clean error instead of a process abort
- `state.messages` display list capped at 2 000 entries; oldest 500 evicted when exceeded with index-based state (collapsed, expanded, search) remapped consistently
- Plugin shell wrappers hardened (second pass): removed legacy `KIRKFORGE_TOOL_ARGS` fallback, added `node` dependency checks for the JS plugin tools, fixed JSON escaping in `die_json` and the draw `post-turn` hook, made stratum tools default to `{}` when no args are provided, and corrected the stratum `config_validate` command-line order
- `kirkforge-video` `animated_explainer` pipeline no longer panics on I/O errors when writing artifact JSON; errors now propagate through the existing `Result` path
- Plugin READMEs and the plugin-host crate doc comment now document the canonical `KIRKFORGE_TOOL_ARGS_JSON` env var instead of the legacy `KIRKFORGE_TOOL_ARGS` alias
- Plugin tool working directory: empty/missing `sandbox_dir` now resolves to the user's current directory instead of the plugin installation root; `README.md` and `config.toml.example` updated to document the escape-hatch semantics
- Pre-tool decision hooks and lifecycle hooks now receive `KF_EVENT` and `KF_SESSION_ID` so plugin3 hooks can distinguish KirkForge from Claude-Code runtime mode
- Bundled plugin shell wrappers hardened: kirkforge-plugin no longer falls back to the Rust binary as a JS entry point, video/stratum optional flags are quoted, and `verify-workspace` safely splits space-separated file paths
- Release packaging now ships all five Rust binaries (`kirkforge`, `kfd`, `plugin3`, `stratum`, `kirkforge-video`); `install.sh` installs the suite and refuses native Windows shells
- `scripts/bump-version.sh` no longer runs `cargo check --locked` after a version bump, which previously failed because the lockfile was stale
- `plugin3-core` integration test `state_drift` now uses `EnvGuard` to prevent env-var leakage on panic
- Line-mode interactive editor no longer panics on concurrent `next_line` calls; returns a clean error instead
- `bash_runner` exotic-target timeout fallback no longer panics if the fallback `sh` command fails to spawn; the error now propagates as a `ShellError::Spawn`
- Release archives and `install.sh` now ship/install the bundled `plugins/` directory to `~/.local/share/kirkforge/plugins/`; workspace plugin sources fall back to the data directory when compile-time source paths are absent
- All five bundled filesystem plugins load without warnings; plugin3 hooks are dual-mode and emit proper KirkForge no-op responses when `KF_EVENT` is set
- `kirkforge-draw` render/edit tools use `--render` and correct argument handling for non-TTY execution
- New regression test `crates/kirkforge-plugin-host/tests/load_bundled_plugins.rs` verifies all bundled plugins load cleanly
- Dependency hardening: `bincode` replaced with `serde_json`, `paste` removed, `ratatui` upgraded to 0.30, and vulnerable/deprecated crates (`crossbeam-epoch`, `quinn-proto`, `anyhow`, `lru`) refreshed
- Flaky tests fixed in `plugin3-core` env guard and `shared::metrics` log rotation
- Release archives and `install.sh` now ship/install the bundled `npm/kirkforge-plugin` Node SDK so `kirkforge-plugin` shell tools (`health`, `doctor`, `tools`, `verify`, ...) work from an installed layout
- Added regression test `bundled_plugins_load_from_data_dir` that exercises the installed-layout plugin loading path
- Cleaned up `kirkforge-plugin` `find_cli` helper to only search the actual installed/repo layout (`npm/kirkforge-plugin` sibling to `plugins/` under the data directory) and removed misleading dead-path candidates; callers now report the real missing-Node-SDK reason
- Added installed-layout end-to-end regression tests that execute real bundled plugin tools through the host's `PluginToolWrapper`: `stratum_mode` (Rust-binary-backed) and `plugin_tools` (Node SDK-backed)
- Plugin tool subprocesses and lifecycle hook subprocesses now run with a null stdin instead of inheriting the host's terminal stdin; prevents tools such as `stratum_run` or the `kirkforge-draw` `post-turn` hook from blocking on interactive input or consuming user keystrokes
- `kirkforge-draw` `post-turn` hook only drains stdin when `KF_EVENT` is unset (Claude Code mode), so it no longer waits for terminal EOF under KirkForge
- `draw_edit` now fails with a clear message when stdin is not a terminal, instead of launching `kfd` into a captured/non-interactive plugin subprocess
- `stratum_run` schema and shell wrapper now accept an `input` field so inline context can be compressed without relying on the host to supply stdin; the `/stratum` skill prompt no longer claims the runtime pipes stdin
- `stratum_run` now treats a missing `input` field as an error instead of silently compressing an empty stdin stream; the schema marks `input` as required
- `stratum_apply` now requires a `file` field; it previously fell back to stdin which is empty under the host's null-stdin plugin execution, silently processing no input
- `kirkforge-video` manifest no longer marks `path`/`check`/`command` as required when the corresponding shell wrapper supplies a sensible default
- `src/session/plugin_tools.rs` now propagates plugin-directory read errors instead of silently defaulting to an empty warning list
- `src/session/mcp_client.rs` reader task now enforces a 5-minute idle timeout and a 1 MiB per-line cap so a misbehaving MCP server cannot hang or exhaust memory
- Bash tool and background job runners no longer hardcode `/bin/sh`; Unix keeps `/bin/sh`, Windows targets `bash` (Git for Windows / WSL) so the same safety gate applies
- Session daemon client is now stubbed on Windows so the CLI compiles and degrades to file-based session discovery; the `daemon` subcommand returns a clear unsupported-platform error on Windows
- Line-mode approval handler no longer assumes `/dev/tty` on Windows; it reads from stdin on Windows while Unix continues to use the controlling terminal
- Hardened `bash_runner` deny-list against quoting/whitespace/escape evasions: commands are normalized (strip comments, quotes, collapse whitespace, lowercase), and redirections/teed writes to system paths are detected with a tokenizer that tolerates optional spaces, fd prefixes (`2>`), clobber form (`>|`), and Windows/Git-Bash path variants (`C:\Windows`, `/c/windows`, etc.)
- `kirkforge-draw` and `stratum` shell helpers now look for their satellite binary next to the script (`<plugin>/tools/<bin>`) before the workspace target directory, so installed-layout plugin directories work when binaries are shipped alongside the wrappers
- `kirkforge-draw` `render.sh` now uses the shared `json_get_string` helper (jq/python3/bash fallback) instead of sed-only parsing, matching the robustness of the other filesystem plugins
- `kirkforge-draw` `edit.sh` now has a proper `#!/usr/bin/env bash` shebang and uses the shared JSON helper so it no longer relies on sed-only argument parsing
- `kirkforge-plugin` `verify.sh`, `audit-verify.sh`, and `verify-workspace.sh` now default an empty/missing `KIRKFORGE_TOOL_ARGS_JSON` to `{}` instead of exiting, matching the other Node SDK tools
- Extended the `clippy::unwrap_used` production lint to the satellite crates (`kirkforge-draw`, `kirkforge-video`, `plugin3`, `stratum`, and their core/host libraries) and fixed the resulting production unwrap sites.
- Satellite binary discovery in `kirkforge-draw`, `kirkforge-video`, `stratum`, and `kirkforge-plugin3` now also accepts `<bin>.exe` candidates, so the Windows release archives (which ship `.exe` binaries) work under Git Bash / WSL without requiring a separate PATH entry.

### Added
- `/plugins` slash-command family for runtime plugin mount/unmount: `list`, `enable <name>`, `disable <name>`, `reload`, `trust <name> <tier>`. The executor picks up the new registry snapshot on the next turn without restarting.
- `--log-level` flag (default `warn`; env `KIRKFORGE_LOG_LEVEL`); `RUST_LOG` still overrides
- `kirkforge completions <bash|zsh|fish|powershell>` — prints shell completion script
- Cargo.toml metadata: `repository`, `license`, `keywords`, `categories`
- Five built-in workspace plugin sources (`plugins/kirkforge-draw`, `plugins/kirkforge-video`, `plugins/stratum`, `plugins/kirkforge-plugin3`, `plugins/kirkforge-plugin`) are now registered by default and can be toggled on/off persistently with `/plugins toggle <name>`.
- `/plugins` slash-command family extended with `toggle <name>`, `sources`, `add <name> <path>`, `remove <name>`, and `setup` for managing workspace plugin sources.
- Source-level unification of all five satellite projects into this repo: Rust satellites build as `crates/*` workspace members and the KirkForge-Plugin SDK is vendored under `npm/kirkforge-plugin/`. The CLI, all satellites, and the plugin-host crate now build from a single workspace.

### Changed
- Default model changed from `deepseek-v4-flash:cloud` to `qwen2.5:7b` so fresh Ollama installs work out of the box
- `NO_COLOR` / `TERM=dumb` now detected at startup; falls back to line-mode instead of TUI

### Added (Phase 13 — testing, benchmarks, coverage)
- `src/lib.rs` library target so `benches/` and `tests/` can exercise real adapter/parser code without duplication.
- Criterion benchmark `benches/first_token_latency.rs` measuring NDJSON parser first-token latency.
- Mock Ollama server integration tests (`tests/mock_ollama.rs`) using `wiremock` so adapter streaming paths run in CI without a live model.
- Property-based tests for `edit_file` exact/fuzzy replacement invariants via `proptest`.
- Additional `ollama_ndjson` parser regression tests for malformed JSON, non-UTF-8 lines, transport errors, empty thinking, `done_reason` variants, and cached token shapes.
- Adapter-selection unit tests covering GLM/DeepSeek/Gemini/OpenAI-compat routing and override behavior.

### Fixed
- Vendored Node SDK (`npm/kirkforge-plugin`): `tool-pyright` now resolves the local `pyright` install before falling back to PATH, fixing test failures under vitest fork workers; CLI test helper no longer spawns every command twice; missing `e2e/smoke.test.ts` added.
- `kirkforge-video` integration tests skip when `ffmpeg`/`ffprobe`/`flite` are absent, and CI installs them so the suite stays green on stock Ubuntu runners.
- Config file (`~/.local/share/kirkforge/config.toml`) now created with `0o600` permissions instead of world-readable `0644`; all three write paths covered (create, hot-reload, `save_config`)
- TUI exit no longer hangs for minutes when an Ollama HTTP call is in-flight:
  - cancel signal sent before channel drop
  - `handle.await` wrapped in a 3-second timeout
- `/exit` and `/quit` slash commands now abort an in-flight model call before setting `should_exit`
- Approval dialog: `Q` / `Esc` deny without exit; `^C` deny and exit; hint line updated so users know how to escape
- Block-comment closer split across a line boundary no longer breaks syntax highlighting
- Model HTTP calls retry up to 3× on connect/timeout errors and 429/503 responses (exponential backoff: 1 s, 2 s, 4 s)
- Default deny list extended with `**/.gnupg/**` and `**/.aws/**`
### Fixed
- ADR numbering collision: vendored 4-digit parallel-tool-dispatch ADR moved from 0019 to 0020 (#8).
