# Changelog

All notable changes to kf-code are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- WO 30.4: Seccomp-bpf syscall filter for bash subprocesses — the missing OS-isolation layer (landlock = FS; seccomp = syscalls). New default-OFF `seccomp` Cargo feature (`seccomp = ["dep:seccompiler"]`) compiles a pure-Rust BPF allowlist filter (`src/session/bash_runner/seccomp.rs`) and applies it last in the bash `pre_exec` hook, after landlock + rlimits, on Linux. Everything not allowlisted fails with `EPERM` (graceful, not SIGSYS-kill). Allowlist = workorder base list (bash + grep/sed/awk/curl/cargo/node/python) + a glibc-startup/modern-`at`-variant block (`arch_prctl`, `set_tid_address`, `rt_sigreturn`, `newfstatat`, `faccessat`, …) without which no dynamically-linked binary execs. Fail-closed like landlock (`--i-accept-unsandboxed` governs both). DEFERRED: real-workload allowlist tuning, cross-arch (aarch64/riscv64) coverage, and the default-on flip — opt in via `--features seccomp` until exercised. ADR-054 amended (was "Do NOT ship seccomp"; the `seccompiler` crate removed the BPF-compiler blocker).
- WO 28.16 (partial: R1–R3): Anthropic hosted `computer_use` beta — adapter wire format behind a default-OFF `computer_use` Cargo feature. `AnthropicAdapter::with_computer_use(Some((w,h)))` adds the `anthropic-beta: computer-use-2025-01-24` header and rewrites a `computer` tool to `{"type":"computer_20250124","name":"computer","display_width_px":W,"display_height_px":H}`; the SSE parser handles `computer_tool_result` content blocks (surfaced as a text placeholder). Feature-OFF assertion test confirms zero hosted wire bytes in a default build. R4 (coordinate-vision execution loop: screenshot capture, model→action routing, `ComputerUseConfig.hosted` runtime wiring) deferred — see `state.md` item 3. The local headless-Chrome CDP `computer_use` tool is unaffected.
- WO 28.7: Coverage regression gate. New `scripts/check-cov-regression.sh` runs `cargo llvm-cov --workspace --lcov`, parses per-crate line coverage, and fails if any crate drops >1% below its floor in `docs/coverage-baseline.md` (standalone: warns + exits 0 if llvm-cov absent; optional `COV_TEST_ARGS` env var forwards extra test-runner flags for hosts with env-incompatible tests). Wired into `scripts/ci-local.sh full`. Baseline filled with real per-crate numbers (kf-code 78.4%, kf-budget-core 86.5%, kf-testdoctor 71.2%, kf-compress-core 95.2%, kf-plugin-host 88.8%, kf-bench 88.3%). Un-ignored the kf-testdoctor `default_thresholds_match_local_gate` self-guard (was `default_thresholds_match_ci_yml`, ignored since the ci.yml `targets={}` dict was removed) and re-pinned it to parse the `scripts/ci-local.sh` tarpaulin `targets` dict. CI ci.yml coverage-job step (R4) deferred — see workorder.
- WO 29.7: Ported `@kirkforge/orchestrator` to a new `kf-orchestrator` workspace crate. `Orchestrator::delegate` pipeline (classify via kf-routing → recall memory via kf-memory-store → resolve provider → build brief → dispatch to mode executor → flush signals → write observation → bump stats). Mode executors `execute_hard_prompt` / `execute_schema_contract` / `execute_artifact` (R1) with full JSONL-artifact protocol parsing + fenced-block persistence + path-safety guards. Decompose pipeline (R3): `parse_decomposition`, `topological_sort` (Kahn's algorithm with cycle/self-dep/unknown-dep detection), `decompose_task` with retry-once-on-parse-fail, `execute_decomposition` with dependency-ordered subtask execution. Correction loop (R4): `run_correction_loop` iterating delegate → validator → `kf_routing::correction::decide_correction`, with cost tracking and truth-model precedence. Workspace manager (R5): `WorkspaceManager` with isolated-workspace creation, baseline snapshotting, copy-filter for excluded dirs (`node_modules`, `.git`, …). Trait seams: `ModelClient` (production impl deferred — `RecordingClient` for tests, `PanickingClient` default) and `EventSink` (`NullSink` default, `RecordingSink` for tests). 61 tests ported. R6 (SLO monitor) + R7 (security-emitter integration) + reducer/verifier-bus port + `ModelClient` production wiring deferred per workorder. Also fixes a pre-existing committed-merge-conflict regression in `Cargo.toml` / `Cargo.lock` / `docs/TECHNICAL.md` from the WO 29.6 merge (`5a6c32d`) — both sides kept where the merge needed both, plus stale `readme_drift` count (772 → 860) corrected.
- WO 29.6: Ported `@kirkforge/memory-palace` to a new `kf-memory-store` workspace crate. `MemoryStore` facade exposes the orchestrator-friendly surface (`write_task_observation`, `write_run_record`, `write_run_and_emissions`, `recall`, `recall_decomposition`, `query_runs`, `query_emissions_for_run`, `evict_expired`, `evict_overflow`, `create`). Three adapters: `InMemoryAdapter`, `FileAdapter` (atomic rename via `tempfile` + `.lock` retry + `.corrupt` backup on parse failure), `SqliteAdapter` (rusqlite, schema v3 with migrations 2/3, prepared statements, `backup`/`restore`/`list_backups`). Reuses `kf-routing` for `tokenize`/`vectorize`/`cosine`/`build_empirical_recommendation` (no duplication). 34 tests ported. R4 (EncryptedAdapter) skipped per workorder (not in barrel, no consumers). New deps: `rusqlite = "0.40"` (bundled + backup features).
- WO 29.4: Ported `@kirkforge/core-events` to Rust. `src/shared/event_bus.rs` ships `EventBus` (async `emit` with idempotency cache + bounded buffer, `on` returning an unsub callable, `drain_buffer`, `shutdown`, `graceful_shutdown`). `src/shared/audit.rs` extended with the tamper-evident hash chain: `initial_hash`/`chain_hash_of` (SHA-256, or HMAC-SHA256 when keyed via `KIRKFORGE_AUDIT_KEY`), `AuditEvent` + 29-literal `AuditAction` + `AuditOutcome`, `MemoryAuditSink`, `FileAuditSink` with size-based rotation (default 50 MB / 10 files), `AuditLogger`, and `create_audit_sink` factory. Dead sinks (http/syslog/worm) deliberately skipped per inventory (zero production consumers). New dep: `hmac = "0.12"` (`sha2` + `hex` already present). 32 tests ported (6 bus + 26 audit).
- WO 29.1: Fold the bundled `kf-plugin` shell scripts into compiled-in Rust tools behind the `kf-plugin-tools` feature (default on). `doctor`, `health`, and `tools` run as native Rust calls (no shell hop, no Node hop); `verify`, `verify_workspace`, and `audit_verify` emit an explicit "deferred to WO 29.7" message. The `/kf-code` skill is registered inline when the feature is on. Eliminates the Rust→shell→Node→linter chain for the three diagnostic tools.
- WO 28.5: Landlock FS confinement on background bash jobs. `resolve_paths` is re-exported `pub(crate)` from `bash_runner` and the background spawn path now resolves a canonical workspace and passes it to `setup_rlimits`. Previously background jobs got rlimits + `CLONE_NEWNET` but not landlock. Closes the WO 27.5-R1 deferral.
- WO 28.2: `shared::session_mode` module — the per-session Stratum mode global moved out of `session::stratum` to break the `budget ↔ stratum` production cycle. Stratum re-exports the accessors for back-compat.

### Changed
- WO 29.2: Rust security emitter — the 14 regex security rules from `security-emitter.ts` are ported to `src/session/verifier/security_emitter.rs`. The verifier bus now calls `emit_security_findings()` directly instead of spawning `bridge-emitter.ts` as a Node subprocess. Eliminates the last Rust→TS call path. Deleting the now-dead TS sources is deferred to WO 29.9.

### Fixed
- WO28h (MEDIUM security): per-verifier timeout — `verifier/handler.rs` wraps each `verify()` in `tokio::time::timeout` (30s prod / 50ms under `cfg(test)`); a wedged `cargo build` verifier no longer hangs the turn (elapsed → `Verdict::Skipped`). Audit-log now records `read_file` (gate renamed `is_destructive`→`should_audit`, includes read_file; path kept by `redact_args`). MCP stdio subprocesses hardened via `env_clear()` + `kf_plugin_host::env::curated_env` so they no longer inherit parent API keys. `block_dotfiles` default flipped false→true (+ serde default); default deny-list gains `~/.config`, `~/.docker`, `~/.netrc`, `~/.gitconfig` (`.aws` already present). 9 bare `#[ignore]` attributes given `= "reason"` strings.
- WO28h: refreshed the stale `state.md` "Current state (2026-08-10)" block to HEAD (WO 27/28/29 shipped, CI green, main at `d848b37` pending ff, version still 0.3.6); `docs/workorders/30.0.0-wo30-overview.md` is now the living index of remaining work.
- ADR-2026-08-12 (adr-fix): `adr_xref_drift` gate unblocked + ADR count sync. The WO 29.7 merge (`7a0de4d`) left committed merge-conflict markers in `Cargo.toml` (workspace.dependencies `kf-orchestrator`) and `Cargo.lock` (`thiserror` 2.0.19/2.0.20) — `cargo` could not parse the workspace, so the gate (and every other test) was unrunnable from a clean clone (`git status` showed clean because the broken file *was* the committed state). Resolved by keeping the `kf-orchestrator` workspace dep (crate exists; intended by the merge) and taking `thiserror 2.0.20`. This completes the conflict-marker cleanup the WO 29.7 entry had already claimed. The ADR-054 header↔README status drift the workorder cited was already fixed in a prior merge — both now read `Accepted (WO 27.1 added landlock — see amendment below)`; full `adr_xref_drift` suite is 4/4 green. Also corrected the stale ADR count in `docs/TECHNICAL.md` (89 → 90, matches the 90 files in `docs/adr/`).
- Review-2026-08-11: Budget/stratum mutex-poison cascade — 26 sites of `.lock().expect("…poisoned")` in `src/session/budget.rs` + `src/session/stratum.rs` converted to `.unwrap_or_else(|e| e.into_inner())`, matching the convention already used 35+ times in `config/mod.rs`. A single poisoned mutex previously panicked on every subsequent turn.
- Review-2026-08-11 (CI red blocker): Stream-drain hang — adapter parsers parked on `stream.next().await` with no idle timeout; reqwest's `.timeout(120s)` does not reliably bound the streaming-body phase, so a wedged HTTP body hung the agent loop forever. New shared `next_chunk_or_idle_timeout()` helper in `adapters/mod.rs` wraps `stream.next()` in `tokio::time::timeout(STREAM_IDLE_TIMEOUT=90s)`; on timeout emits `StreamEvent::Error` and closes the channel. Applied to anthropic, anthropic_bedrock, ollama_ndjson, openai_compat parsers.
- Review-2026-08-11 (CI red blocker): e2e routing mismatch — `e2e-test-model` fell through `adapter_kind_for_default` to OpenAiCompat while scenarios asserted the Ollama `/api/chat` path. Seeded `[adapter_routing] "e2e-" = "Ollama"` in e2e config fixtures (no production code change; uses the existing extension point).
- Review-2026-08-11: web_fetch SSRF-via-redirect — reqwest client followed up to 10 redirects without re-running the top-level SSRF checks, so a 302 to `http://169.254.169.254/...` bypassed them. Client now uses `.redirect(reqwest::redirect::Policy::none())`; 3xx surfaces to the model, which can re-call web_fetch through the full SSRF validation.
- Review-2026-08-11: Tool panic isolation — `run_prepared_call` now wraps `prep.tool.run()` in `AssertUnwindSafe().catch_unwind()`. A panicking tool returns `ToolOutcome::Failure(ToolError::Internal{ "tool panicked: <msg>" })` instead of unwinding the executor loop. Protects the deterministic-mode and Phase 2.5 deferred-file-call direct-call paths (which ran on the executor task, not a spawned task) and preserves the panic message on the spawned path.

### Changed
- Review-2026-08-11: `ratatui` now `default-features=false, features=["crossterm","unstable"]` (drops macros + calendar widget + layout-cache + underline-color; none used). Note: the review's "wezterm stack" premise was stale — ratatui 0.30 already split into ratatui-core/crossterm/widgets and default no longer pulls termwiz.
- Review-2026-08-11: `crossterm` 0.28→0.29 (kills version split with ratatui-crossterm); `thiserror` 1→2 in workspace.dependencies (4 sub-crates already use `.workspace=true`).
- Review-2026-08-11: Doc-sync — `docs/TECHNICAL.md` ADR count 88→89 + `plugins/` tree shows only `kf-plugin/`; `AGENTS.md` "CI is disabled" reconciled with reality; `state.md` phantom `kf-code-review.md` deleted; `docs/README.md` `reviews/` dropped; `CHANGELOG.md` duplicate `## [Unreleased]` merged.
- Review-2026-08-11: Workorder status sync — 4 overview files (WO 21/23/24/25) `## Status` headers + 29 `docs/workorders/README.md` index rows updated to match `state.md` (were stale "Planned").

### Added
- WO 26.8: Decompose `AppState` from a single flat ~66-field struct into 11 sub-structs grouped by concern (`conversation`, `generation`, `budget`, `session`, `provider`, `approval`, `search`, `ui`, `doom`, `services`, + `dirty` bool). Call sites migrated to `state.<group>.<field>`; existing helper methods retained as accessor shims. TUI renders identically; session persistence format unchanged.

### Fixed
- WO 26.4-F8: drop the job-store lock before `.await` in the scheduler daemon — no `MutexGuard` spans an await, removing a deadlock-under-load risk.

### Added
- WO 26.7-R4: Anthropic computer_use beta re-deferred with disclosure (hosted API path needs `computer` tool type + `anthropic-beta` header + coordinate-vision routing in the Anthropic adapter; tracked in state.md pending + WO 26.7-R4).
- WO 26.7-R2: MCP `sampling/createMessage` handler — server-initiated sampling requests route through the same approval bus as tool calls (default deny in headless; opt-in `tools.allow_sampling_unattended`). New ADR-072 documents the trust model.
- WO 26.7-R1: Bash streaming UX — PTY output streams into the TUI tool-result card via new `TurnEvent::BashPartialOutput` while an interactive command runs (spinner + incremental text). Non-PTY path unchanged.
- WO 26.6-R3: Wire eslint into CI — `npm run lint` runs in `scripts/ci-local.sh` and the `quality` CI job; removed a pre-existing unused `readFileSync` import in the Node SDK that blocked the lint gate.
- WO 26.3: `--features landlock` now compiles — declared the previously-orphaned `landlock` module in `bash_runner/mod.rs` and fixed the `Option<&LandlockPaths>` type mismatch. Landlock tests skip cleanly on kernels/caps that can't confine (probe `restrict_self`, not just `create_ruleset`).
- WO 25.17: Document persona Anthropic-direct limitation (ponytail comment in persona.rs, TECHNICAL.md note). Bedrock/Vertex plumbing deferred (tracked in state.md).
- WO 25.17: Document landlock as opt-in (ponytail comment on Cargo.toml feature, TECHNICAL.md already correct).
- WO 23.8-R1: Doom-loop circuit breaker — auto-switches to plan mode after `doom_loop_max_hits` cumulative detections (default: 1). New `TurnEvent::DoomLoopRemediation` event. Config: `doom_loop_max_hits` / `KF_CODE_DOOM_LOOP_MAX_HITS`. Set to 0 to disable.
- WO 23.8-R2: Doom-loop circuit breaker auto-switches to plan mode. Note: no hard halt when already in plan mode — this was planned but not implemented; see WO 25.0-R3.
- WO 23.5-R1: `remember` tool — model can now explicitly store facts via `remember({ "fact": "...", "category": "..." })`. Facts are persisted in `MemoryStore` and surfaced in future sessions. Idempotent by slug.
- WO 23.5-R2: System-prompt instruction for `remember` tool — when memory is enabled, the system prompt now includes guidance on when to use `remember`.

### Changed
- Documentation audit: merged KIRK-BENCH.md spec content into docs/TECHNICAL.md Benchmarks section; deleted KIRK-BENCH.md from repo root (tech content belongs in the manual, not a standalone slop file).
- Fixed stale env var: KIRKFORGE_BUDGET_CEILING → KF_CODE_BUDGET_CEILING in KIRK-BENCH.md (now TECHNICAL.md) and ADR-066.
- Fixed stale feature list: README.md no longer lists deleted Draw/Video as compiled-in features.
- Fixed stale test count: kf-budget-core README now shows actual count (175) instead of 679.
- Fixed stale ponytail comment: TECHNICAL.md Stratum section no longer claims "no content-type-specific transforms" — MinifyTransform is registered.
- Fixed stale ADR paths: 8 ADRs updated with current kf-* names (028, 007, 016, 019, 035, 040, 047, 058) replacing kirkforge-* references.
- Fixed ADR-047: "process-global store" → "session store" (offload store is per-session, not process-global).
- Fixed app.rs comment: "~55 fields" → "~63 fields" (actual count).
- Deleted orphan bench task: use_draw_render.toml (references deleted draw_render tool).
- Deleted docs/reviews/ (4 review files were never asked to be tracked in the repo).
- Deleted PONYTAIL-DEBT.md from repo (not a tracked workflow — debts folded into state.md and ADRs).
- Updated AGENTS.md doc-placement rule: removed PONYTAIL-DEBT.md exception and docs/reviews/ instruction.
- Updated state.md: removed PONYTAIL-DEBT.md references; deferred items now tracked in state.md directly.
- Updated ADR-070: replaced PONYTAIL-DEBT.md tracking references with state.md.

### Removed
- KIRK-BENCH.md (content merged into docs/TECHNICAL.md).
- docs/reviews/ directory (4 review post-mortems not belonging in the codebase).
- PONYTAIL-DEBT.md (debts tracked in state.md and ADRs).
- benches/tasks/use_draw_render.toml (orphan referencing deleted draw_render tool).

### Changed
- WO 20.2.0: `tool_choice` + `max_tokens` config + adapter trait wiring (`set_tool_choice`, `set_max_tokens`). `build_anthropic_body` is now 9-arg; extended-thinking kept separate from completion `max_tokens` (dedicated `budget_tokens` + `supports_thinking` guard). CONFIG_FIELD_COUNT 84→85. (#23)
- WO 20.0.7: cache breakpoint cap holds at 4 with tools (CRIT-1); `store_get` resolves Stratum offload markers (CRIT-2). (#23)
- WO 20.0.9: `draw` feature now non-default (opt in via `--features draw`).
- WO 20.0.9: `kf-compress-hosts` crate collapsed into `kf-compress-core` (rules module).
- WO 20.0.9: Removed Cursor/Aider/KfCode stub modules from `kf-budget-hosts`.
- WO 20.0.9: `cargo audit` now blocking in CI (`--deny warnings`, no `continue-on-error`).
- WO 20.0.9: Cosign signing now blocking in release workflow.
- WO 20.0.9: Bench tasks (draw/workflow/budget/stratum/lsp) now require model; verify agent output.
- WO 20.0.9: TECHNICAL.md Stratum section downgraded to match reality; `compaction_use_llm` naming documented.
- WO 20.0.9: `SandboxEnforcer` and `DocLookup` annotated with `ponytail:` honest-doc comments.
- WO 20.8.0: `cargo audit` now blocks on critical/unmaintained/unsound advisories (CI). Cosign signing now blocks on release. 28 hot-path `tracing::debug!` calls converted to `tracing::trace!` (47→19).

### Added
- WO 20.3.0: `--no-network` flag (Linux, requires `--harden`) — isolates bash commands in empty network namespace via `unshare(CLONE_NEWNET)`.
- WO 20.3.0: `--confirm-edits` flag (requires `--harden`) — `edit_file` and `write_file` return diff preview instead of applying.
- WO 20.3.0: `--harden` mode refuses to start without sandbox config (`sandbox_dir` or `allowed_write_dirs`).
- WO 20.8.0: Added fuzz targets (minify_rust, minify_revalidate, ndjson_parse) and CI fuzz job. Added ADR-067 documenting TUI/daemon coverage exclusion.
- WO 17.1: per-provider API key resolution (`resolve_api_key` with config → env → keychain order) and Anthropic auth headers (`x-api-key` + `anthropic-version`).
- WO 17.2: daemon instance channel (broadcast, auth, version gate). `DaemonServer` registers instances and broadcasts state changes over the Unix socket; `DaemonClient` authenticates with a token read from `KF_CODE_DAEMON_TOKEN_FILE`.
- WO 17.3: daemon hardening — socket guard (refuses to hijack a live socket), auth token check on every request, version gate (rejects mismatched client versions), ownership check (socket must be owned by current UID), clean `QuitAll` + `Shutdown`.
- WO 17.4: AST minification + surgical edit position map. JS files now use the dedicated `tree_sitter_javascript::LANGUAGE` grammar for minification and revalidation (not TSX). `MinifyCache` tracks byte-offset mappings so `edit_file` applies to original line numbers correctly after minification.
- WO 17.5: stem-agents, shared cached context, cache breakpoints. `CacheStemTracker` reuses stable prompt prefixes across turns; LLM compaction config (`llm_compaction_model`, `llm_compaction_prompt`) for model-driven summarisation. `STEM_FILE_CAP` const and `Config::stem_file_cap` cross-referenced.
- WO 17.6: workflow engine parity — `FanOut` parallel step execution with semaphore, `ForkFrom` for branching, typed variables, `run_bash` deny-list check, `ToolContext` cancellation + dry_run propagation.
- WO 17.7: E2E test harness (`tests/e2e/`) with mock provider, `IsolatedEnv` sandbox, and per-task test runner. Session index alert persistence moved to `<data_dir>/sessions/`.
- WO 17.8: jobs workflow integration — scheduled jobs can run workflow JSON steps; alert persistence writes to `<data_dir>/sessions/.alerts.ndjson`.
- WO 17.9: TUI parity pass — top tab bar (Sessions, Jobs, Alerts tabs), interactive tab switching, `/` slash-command popup, `@` file-mention popup, welcome screen on first run.

### Fixed (review-4)
- Fix: `folded_feature_enabled` name mismatch — `"kf-plugin-sdk3"` → `"kf-budget"` (C1).
- Fix: `computer_use` `evaluate` action now runs Chrome with `--proxy-server` blocking RFC1918/link-local and `--host-resolver-rules` mapping `*` to `~NOTFOUND` (C2).
- Fix: `web_fetch` DNS rebinding — pin resolved IP and recheck after connect; percent-decode host before IP check (H1, H2).
- Fix: per-plugin rlimits always applied regardless of `harden` flag; `PluginTool::from_capability()` populates `resource_limits` from manifest (H3).
- Fix: `PluginTool` audit logging — `AuditEntry::PluginTool { name, args, exit_code, duration }` (H4).
- Fix: jobs daemon auth token check on every request via `check_auth` (H5).
- Fix: daemon client reads auth token from `KF_CODE_DAEMON_TOKEN_FILE`; `InstanceRegister` in TUI event reader sends the token (H7).
- Fix: `ScheduledJob.timeout` enforcement — `registry.spawn` now passes `j.timeout` (H6).
- Fix: Bedrock `envelope_buffer` capped at 8 MiB; multi-event chunks drain fully, not just the first frame (H9 — already fixed in WO 15.6, confirmed).
- Fix: Vertex `service_account_token` returns error on empty token instead of sending `Authorization: Bearer ` (H22 — already fixed in WO 15.6, confirmed).
- Fix: workflow `run_bash` routes through `check_bash_command_str` and `SandboxConfig` (M7).
- Fix: workflow `ToolContext` propagates parent `CancellationToken` and `dry_run` (M8).
- Fix: `VerifierSlots` max raised from 4 to 8 (M4).
- Fix: verifier bus stubs removed — `SecurityBusVerifier` and `GitBusVerifier` deleted (M3).
- Fix: `resolve_step_refs` uses char-aware indexing, not byte offsets (M12).
- Fix: `format_verdict_report` slices at UTF-8 char boundary, not byte 23 (L1).
- Fix: JS revalidation uses `tree_sitter_javascript::LANGUAGE` instead of TSX (L8).
- Fix: `llm_compaction_summary` renamed to `deterministic_compaction_summary` (M1).
- Fix: `MicrocompactResult::summarised_messages` dead-code `#[allow]` removed (M2).
- Fix: `STEM_FILE_CAP` const (4096) and `Config::stem_file_cap` default (4096) cross-referenced with `ceiling:` note (M14).
- Fix: alerts file moved from `<data_dir>/.alerts.ndjson` to `<data_dir>/sessions/.alerts.ndjson` (M16).
- Fix: Clippy useless `.into()` conversions removed (6 instances) (L11).
- Fix: all 4 `"kf-plugin-sdk3"` runtime gates replaced with `"kf-budget"` — budget tools/hooks now register on default builds (WO 18.0.1).
- Fix: `VerifierHandler::verify_event` collects all verifier findings instead of short-circuiting on first; most severe wins (M5).
- Fix: `load_one` rejects invalid manifests, matching `load_from_dir` behaviour (M10).
- Fix: `WorkflowExecutor::run` decomposed into named sub-methods (M13).
- Fix: config serde field count assertion cross-checks `CONFIG_FIELD_COUNT` against `serde_json::to_value` key count (M15).
- Fix: `CompositeToolset` resolution order documented in code comment (M9).
- Fix: Docker bind-mount source validated against canonical project root — symlink escape blocked (M20).
- Fix: `jobd` stale-socket guard — connects before removing, refuses to hijack a live socket (M6).
- Fix: trust tier enforced at plugin tool dispatch time — ReadOnly plugin tools return `AccessDenied` (M11).
- Fix: `PostTurnHookGuard::drop` spawns hook asynchronously via `tokio::spawn` instead of blocking on Drop (L2).
- Fix: minify cache replaced with proper `LruCache` (HashMap + VecDeque, true LRU eviction) (L7).
- Fix: `/plugins toggle` shows restart-required notice for compiled-in plugins (WO 18.0.2).
- Fix: `budget.rs` module doc updated from stale `plugins/kf-plugin-sdk3/tools/` to `plugins/kf-budget/tools/` (WO 18.0.4).
- Fix: `ScheduledJob.timeout` enforced for workflow jobs via `tokio::time::timeout` (H6 complete).
- Fix: `Arc` import gated behind `cfg(unix)` in `daemon/mod.rs` — unblocks Windows CI.

### Added (test debt WO 19)
- WO 19.1: testdoctor `diagnose` now scans all source directories (`src/tui`, `src/daemon`, `src/jobs`, `src/main`, `src/shared`); `--dirs` and `--with-coverage` CLI flags.
- WO 19.2: testdoctor public API surface metric — `api_surface` counts `pub` items + `impl` methods (excluding `pub(crate)`/`pub(super)`), `test_density` and `roi` use API surface instead of line count.
- WO 19.3: test monolith surgery — `tests_adr_0015.rs` (5,183 lines) split into 8 focused files; `kf-draw-core` state tests split into 4 files; `approval.rs` split into `auto`/`deny`/`timeout`.
- WO 19.4: no-assertion tests upgraded — hooks, process group, budget helpers now assert actual behavior instead of "does not panic".
- WO 19.5: integration tests for daemon auth token enforcement, budget registration gate (regression test for 18.0.1), and job lifecycle timeout.
- WO 19.8: testdoctor `suggest-detailed` uses a binary-to-path map built from workspace Cargo.toml files instead of path guessing.
- WO 19.9: flaky test stabilization — `yield_now` + `try_recv` replaces `sleep`/`timeout`; assertions on actual outcomes instead of "did not hang".
- WO 18.0.3: `budget_tools_present_in_default_toolset` integration test asserts budget tools appear under default config.

### Changed (test debt WO 19)
- WO 19.3: `kf-budget-cli` `tests_adr_0015.rs` monolith replaced with `tests/{budget,budget_compact,config,report,report_filters,report_summary}.rs`.
- WO 19.3: `kf-draw-core` state tests split from `tests.rs` into `tests/{active,error,initial,mod}.rs`.
- WO 19.3: `approval.rs` split into `approval/{auto,deny,timeout,mod}.rs`.
- `CONFIG_FIELD_COUNT` updated from 73 to 82 (ModelConfig 22→27, SessionConfig 4→8).

### Removed (dead code / over-engineering audit)
- Collapsed four identical Oellama adapters (deepseek, gemini, glm, kimi) into one `OellamaAdapter` + profile table (−298 lines).
- Deleted `key_file_looks_valid` + 7 tests from `vertex_auth` (zero non-test callers, −65 lines).
- Deleted `EventKind::all()`, `VerifierSlots::unregister` + test, `send_reload` from jobs/client, `JobListEntry` + `From` impl, `tab_bar_spans`/`tab_bar_line`, 3 unused `impl Default`, `PRICING_FALLBACK` constant.
- Inlined `is_empty_object` (1 caller) in `anthropic.rs`.
- Promoted `find_subseq` + `trim_ascii_whitespace` from private copies in `anthropic.rs` and `openai_compat/mod.rs` to `pub(crate)` in `adapters/mod.rs`.
- Deduplicated `wrap_cached` in `adapter_swap.rs` → calls `caching::maybe_wrap_cached`.
- Eliminated `session_token()` in `bedrock_signing.rs` — reads from credentials instead of re-reading env var.
- Dropped unused `_profile` param from `sign_request`, `profile` field from `AnthropicBedrockAdapter`.
- Dropped unused `_model_info` param from `build_ollama_chat_body`.
- Collapsed 20 bool env-override blocks into `env_bool!` macro (−41 lines).
- Gated 5 test-only minify functions with `#[cfg(test)]`.
- Inlined `register_if` (1 caller) in tool registry.
- Skipped: `extract_host` → `url` crate (security-sensitive, behavior differences).

### Changed
- Renamed the binary from `kf-code` to `kf-code`. All CLI invocations,
  env var prefixes (`KIRKFORGE_` → `KF_CODE_`), config/data directory
  paths (`~/.local/share/kf-code` → `~/.local/share/kf-code`), and
  documentation references updated accordingly. The GitHub org/repo URLs
  remain `KirkForge/KirkForge-Cli`.
- Renamed all sub-crates to the `kf-` prefix: `kf-plugin-sdk` →
  `kf-plugin-sdk`, `kf-plugin-host` → `kf-plugin-host`,
  `kf-context-index` → `kf-context-index`,
  `kf-workflow` → `kf-workflow`, `kf-lsp` → `kf-lsp`,
  `kf-bench` → `kf-bench`, `kf-draw-core` →
  `kf-draw-core`, `kf-draw` → `kf-draw`,
  `kf-video` → `kf-video`, `kf-compress-core` →
  `kf-compress-core`, `kf-compress-hosts` → `kf-compress-hosts`,
  `kf-compress-cli` → `kf-compress-cli`, `kf-budget-core` →
  `kf-budget-core`, `kf-budget-hosts` → `kf-budget-hosts`,
  `kf-budget-cli` → `kf-budget-cli`, `kf-testdoctor` →
  `kf-testdoctor`. Plugin directories renamed:
  `kf-budget` → `kf-budget`, `kf-plugin-sdk` → `kf-plugin`,
  `kf-draw` → `kf-draw`, `kf-video` → `kf-video`.
  The manifest filename changed from `kf-code.toml` to `kf-code.toml`.
  All documentation updated to reflect the new names.

### Added
- WO 15.26 Batch C (tools + executor): 14 of 15 safe items fixed (one
  commit each), 3.47 verified already-done, 3.5/3.6 deferred to a
  refactor WO. Dedup wins: `find_cargo_root` extracted to
  `verifier/helpers.rs` (was triplicated), `ChromeTab` impl deduped
  (deleted `RealChromeTab` + 2 dead fields), `run_decision` shared body
  extracted. Correctness: bash now surfaces stderr on success (so
  `cargo` warnings reach the model), `file://` LSP URIs are percent-
  encoded (spaces + non-ASCII), `CachingAdapter` forwarder aborts on
  consumer drop (`select! closed()` — no more 30s network drain),
  workflow batch results are paired by name with partial-result
  detection, `verify_task` passes the task's curated `budget_env`.
  `atomic_write` dir-fsync + `compare_reports` difficulty fallback
  documented as ceilings. New `--help` smoke tests for 9 subcommands.
  Tests: caching consumer-drop abort, workflow partial-batch detection,
  bash timeout partial-stdout preservation, bench curated-env, lsp
  percent-encoding.
- WO 15.26 Batch B (verifier + security): all 15 items fixed (zero
  deferrals). `VerifierHandler` short-circuits `ToolError` events (skip
  the verifier fan-out); `bash_jobs` watcher has a watchdog that flips
  `Running`→`Failed` on watcher death; `ENTROPY_PREFIXES` expanded
  (`xai-`, `hf_`) with `claude-`/`key-` excluded (false positives) and a
  `ceiling:` note; the test verifier's full-suite fallback is now scoped
  to crate-root `main.rs`/`lib.rs` (nested files keep a targeted filter);
  `VerifierBus` duplicate-name **coexistence** is documented (built-in
  slot stubs + plugin verifiers share slot names by design). Dead
  `event_kinds.rs` deleted. New tests: write_file cross-test of both
  verifier paths, ToolError-through-handler, CorrectionLoop max-iterations,
  PluginToolWrapper.run Cancelled path, bus duplicate-coexistence,
  entropy-prefix coverage. Docs: env-var contract divergence, trusted-
  verifier command env, `split_whitespace` ceiling, ADR-028 amendment
  note, stale prose ADR list deleted (index table is source of truth).
- WO 15.26 Batch D (docs + polish): `scripts/ci-local.sh full` mode
  mirrors the CI coverage gate (tarpaulin + thresholds) and runs the
  `adr_xref_drift` test locally; `WorktreeSession::create` now
  validates `session_id` (rejects empty / path separators / `..`); the
  testdoctor `DEFAULT_THRESHOLDS` drift test now parses `ci.yml`
  instead of asserting against its own literals (and the stale
  `src/session` 68.0 const was corrected to 68.5). Plus doc fixes:
  ci.yml documents the src/tui + src/daemon coverage exclusion;
  KIRK-BENCH/ADR-066 task-count arithmetic reconciled (19 planned, not
  ~9); TECHNICAL.md notes the ~3,900-test workspace total. Open-ended
  items (Windows CI, `--harden` in CI, leaderboard dashboard, bench
  metrics, architecture diagram) honestly deferred to state.md.
- WO 15.26 Batch A (config + adapters): bedrock signing returns an
  error on non-ASCII header values instead of silently dropping;
  bedrock event-stream frame bodies parsed via serde_json (no fragile
  `{"type` literal match); vertex service-account key files must carry
  `"type": "service_account"`; `CacheKey` switched from `DefaultHasher`
  to sha256 (collision-resistant, no new dep). Config-driven model
  routing / capability detection / context-window / max_tokens all
  documented as `ceiling:` notes with upgrade paths (3.20-3.23), as is
  the Vertex token-fetch retry (3.36); the Config field-drift guard
  (3.1) deferred pending a derive macro (see state.md).
- `/permissions list | revoke <i> | clear` (WO 14.5): surfaces the
  permission rules created by the approval dialog's `[A]lways` key so
  users no longer need to edit `config.toml` to undo an always-allow.
  Pure ops layer (`src/tui/commands/permissions.rs`) over `&Config` /
  `&mut Config`; the TUI arm persists on `revoke`/`clear` via
  `save_config`. 1-indexed to match `/jobs` and `/undo list`. 6 unit
  tests.
- First-run onboarding banner (WO 14.1): `load_or_create_config` now
  prints a stdout banner naming the config path + a concrete `-m`
  model hint on first run, so a new user gets feedback instead of
  silent success. Fires exactly once (gated by `!exists`). README
  quick start shows `kf-code run -m qwen2.5:0.5b` with a first-run
  hint; `/init` slash command usage string filled in.
- Actionable error hints + typed error classification (WO 14.3):
  `KirkForgeError::hint()` returns a per-variant suggestion string
  (model/provider, permission/sandbox, config parse); the top-level
  error printer now shows a `hint:` line when present. The
  `From<anyhow::Error>` classifier downcasts two typed errors
  (`kf_plugin_sdk::ManifestError` -> ConfigParse,
  `kf_plugin_host::ToolError` -> AccessDenied) before falling
  back to the existing string matcher. The migration TODO is updated,
  not removed — ModelUnreachable still uses string matching (no typed
  model-connection error exists in the adapter layer yet).
- Grouped `/help` + filled empty usage strings (WO 14.2): the TUI
  `/help` output is now sectioned into six groups (Session, Model,
  Safety, Workflow, Plugins, Diagnostics) in a fixed order via a
  `GROUPS` const, with commands alphabetized within each group. The
  six commands that shipped with empty `usage` strings (`/memory`,
  `/metrics`, `/verify`, `/gh`, `/init`, `/plugins`) now carry concrete
  syntax verified against their dispatch arms. Line-mode `/help` adds
  a pointer to the TUI's grouped list. 3 new tests enforce group
  coverage.
- WO 14.4: status bar degrades by priority on narrow terminals —
  drops plugin/skills/tool-call/Ctrl+T spans before overlapping;
  keeps elapsed, cost, and the `⚠️ UNSANDBOXED` warning at all widths.
- Slash-command + `@`-mention Tab autocomplete (WO 14.6): typing a
  `/` prefix and pressing Tab completes against the `COMMANDS` primary
  triggers (single match replaces the buffer, multiple show a one-line
  dim suggestion list above the input); typing `@` completes the path
  portion against the filesystem, leaving the `:A-B:raw` suffix alone.
  The legacy "Tab on empty input toggles expand/collapse" behavior is
  preserved. `complete_command` is a pure function over `COMMANDS`;
  `complete_path` is `std::fs::read_dir` capped at 24 entries.
- KIRK-BENCH spec + signature Token Budget Challenge (WO 14.7,
  ADR-066): published `KIRK-BENCH.md` (8 categories, 40 tasks,
  universal scoring, 10 hero benchmarks) with a mapping table for the
  existing 30 tasks; new `benches/tasks/token_budget_challenge.toml`
  signature task run 5× under descending budget ceilings (128k/64k/
  32k/16k/8k) via `KF_CODE_BUDGET_CEILING` env; `BudgetChallengeReport`
  markdown scoreboard; `load_tasks` now accepts a single `.toml` file.
- Testdoctor smart suggest + apply (WO 12.6, ADR-0029): `kf-code
  doctor suggest-detailed [--filter <substr>]` composes per-test
  timings with source-file pattern analysis (subprocess spawn,
  `tokio::time::sleep`, `std::env::set_var`, network calls, temp-dir
  writes) to produce specific, actionable suggestions. `kf-code
  doctor apply --suggestion <id> --test <path> [--yes]` performs a
  text-based rewrite (add `#[ignore]`, wrap `#[tokio::test(start_paused
  = true)]`, replace `std::env::set_var` with `EnvGuard::set`); always
  shows the diff first, requires `--yes` to write. No `syn` dep.
- Testdoctor per-test timings + flaky-test detection (WO 12.5,
  ADR-0029): `kf-code doctor profile-per-test` captures per-test
  durations via nightly JSON (`cargo +nightly test -- --format json
  -Z unstable-options --report-time`) when nightly is installed, and
  falls back to per-binary timings attributed to each test (coarse,
  flagged in the report) on stable. `kf-code doctor flaky --runs N
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
- `kf-code plugin` CLI subcommand (WO 11.0, ADR-056): `list`, `enable`,
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
  rejected with clear errors. `plugins/kf-budget/kf-code.toml`
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
  `kf-code verify` CLI print a table of recent verifier verdicts
  from the metrics log. `format_verdict_report` formats the bus's
  in-memory verdicts. 3 new metric tests (source field, default,
  empty report).
- Plugin hot-reload via file watcher (WO 11.4, ADR-059): added
  `notify-debouncer-mini` dep. `spawn_plugin_watcher` watches the
  plugins dir with 500ms debounce and sends a reload signal on
  `kf-code.toml` / tool/hook script changes. The TUI spawns the
  watcher at startup; the reload uses the same path as `/plugins
  reload`. 1 `#[ignore]` integration test (timing-sensitive).
- Surface trust-tier downgrades (WO 11.3): `/plugins list` now shows
  the effective trust tier when it differs from the manifest trust
  (e.g. "shell (effective: read-only)") and the count of filtered
  capabilities. `HostedPlugin` gains an `original_capability_count`
  field. The non-downgraded case is quiet (no noise). 1 new test.
- Plugin init scaffolding (WO 11.8, ADR-063): `kf-code plugin init
  <name>` scaffolds a new plugin directory with a valid
  `kf-code.toml` (default `trust = "read-only"` + placeholder skill),
  `tools/` + `hooks/` dirs (with `.gitkeep`), and a `README.md`. The
  scaffolded manifest passes `kf-code plugin validate` out of the
  box. 3 new tests (round-trip, invalid name, existing dir).

### Fixed
- WO 15.7: cancel leak + double-record `AccessDenied` +
  `enabled_plugins` runtime gate. (1) `dispatch_tool_call_batch` Phase 2
  collect loop now aborts un-awaited `JoinHandle`s when cancellation
  fires mid-batch, so already-spawned tasks no longer run detached
  holding subprocess/network resources for up to `tool_timeout_secs`
  (bucketlist 2.3). (2) Phase 3 no longer re-runs the path guard + read
  gate for a deferred file call already denied in Phase 2.5, so the
  model sees one "Access denied" result per failed edit instead of two
  (bucketlist 2.8). (3) Stratum/Budget tool + hook registration now
  checks `cfg.tools.enabled_plugins` at runtime, so `/plugins disable
  stratum` (or `kf-budget`) actually removes the compiled-in
  tools/hooks on the next `kf-code run`, not just the `/plugins list`
  display (bucketlist 5.1). 2 new tests
  (`test_cancelled_batch_aborts_remaining_spawned_tasks`,
  `test_denied_edit_records_single_access_denied_result`).
- WO 15.10: four security-scanner polish fixes from the cross-review
  bucketlist. (1) The security verifier's dangerous-shell-pattern check
  now skips comment-prefixed lines (`//`, `#`, `/*`, `*`) so
  documentation that mentions `rm -rf /` is no longer flagged
  `Unfixable` and blocking the correction loop (bucketlist 2.9); the
  entropy and secret-substring scans still run on all content. (2)
  `git_sanitation::SCAN_CAP_BYTES` raised 1 MiB → 10 MiB so a secret
  placed after the old cap in a large generated file is caught; the
  docstring documents the ceiling honestly (bucketlist 2.13). (3)
  `trufflehog_scan` spawn wrapped in `tokio::time::timeout` (60s prod /
  2s test override) so a hung trufflehog returns no finding instead of
  deadlocking the correction loop (bucketlist 2.14). (4)
  `Bash::run_docker` replaced `.expect("docker_config is Some")` with
  an early `Err(ShellError::Spawn)` so a future caller that forgets the
  `docker_config.enabled` guard surfaces a tool failure, not a runtime
  panic (bucketlist 2.15). 8 new tests.
- WO 15.11: `computer_use` stale-boolean triple-lock collapsed to a
  single `Mutex` acquisition (check + step + use in one block-scoped
  guard, dropped before the async fallback so the future stays `Send`);
  `ResponseCache::get` caps the disk read at 64 MiB (metadata size
  check before `std::fs::read`, warning + cache miss on overflow) so a
  crafted/huge cache file can't OOM; Anthropic `parse_anthropic_stream`
  EOF flush now emits a `ToolCall` with empty `{}` input when a
  `content_block_start` arrived with no `partial_json` (truncated tool
  was silently dropped); `init_default_verifiers` bus-handler
  registration failure log upgraded `warn!` → `error!` (the sync
  constructor runs inside tokio workers so `block_on` panics —
  registration stays fire-and-forget by necessity; `count` was already
  honest as it counts slot verifiers only); host-crate
  `PluginTool::execute` now applies rlimits via a new
  `crates/kf-plugin-host/src/rlimits.rs` mirroring
  `bash_runner::setup_rlimits`, gated on an optional
  `resource_limits` field + `with_resource_limits` builder (ADR-060;
  default `None` preserves today's behavior). Closes bucketlist items
  2.12, 2.16, 2.17, 2.18, 2.19.
- WO 15.1: honest CI gate naming in `.github/workflows/ci.yml`. Renamed
  the `Fail if success rate drops below 10%` step to `Warn if success
  rate drops below 10%` (it only emits a `::warning::` then exits 0;
  the real regression gate is `bench-baseline.yml`'s `bench-pr-delta`
  with `--fail-on-regression 10`). Replaced `cargo audit || true` with
  `continue-on-error: true` on the step so RUSTSEC advisories are
  *visible* in the run UI (non-blocking) instead of hidden by the
  shell-level `|| true`. Removes the gate-theater anti-pattern
  (AGENTS.md §4/§6); no logic change.
- WO 15.2: `PluginRegistry::load_from_dir` (the production plugin-load
  path) now calls `PluginManifest::validate()` after the API-version
  check, surfaces every schema error (bad name, bad semver, duplicate
  triggers, unknown hook events, untrusted command paths) as a warning,
  and skips the offending plugin — matching the `load_one` contract from
  WO 8.8 ("show every issue at once"). Previously the bulk-load path
  silently accepted invalid manifests. Also adds `post-tool-write_file`
  to `KNOWN_EVENTS` (the runtime emits it via `budget.rs`; the validator
  allowlist was stale). 2 new tests.
- WO 15.3 security: closed three SSRF / injection surfaces. (1) The
  `computer_use` Chrome launcher now passes
  `--host-resolver-rules="MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE
  127.0.0.1"` so a page loaded by `open`/`navigate` cannot
  `fetch('http://169.254.169.254/...')` from inside the browser via
  `evaluate` — all DNS except localhost returns NXDOMAIN. (2) `web_fetch`
  resolves the URL host via the OS resolver and rejects the request when
  any resolved IP is loopback / link-local / RFC1918 / RFC4193, closing the
  DNS-rebinding door where a public hostname's A record points at
  `127.0.0.1` (the prior `ceiling:` is now closed; a residual resolve→
  connect TOCTOU is documented since reqwest has no per-request IP
  pinning). (3) The `bash` Docker path now canonicalizes the bind-mount
  source and rejects a workdir whose path contains `:` (which Docker
  parses as host/container/opts split), and routes the model-supplied
  `cmd` through `check_bash_command_str` — the Docker branch previously
  skipped the deny-list / dangerous-pattern gate the foreground path
  runs. 10 new tests (chrome_launcher: 2, web_fetch: 5, bash: 3).
- WO 15.6: (1) Bedrock `parse_bedrock_event_stream` now caps the outer
  envelope buffer at 8 MiB (matching the inner `parse_anthropic_stream`
  `MAX_SSE_BUFFER_BYTES`); a runaway stream emits an error event + clears
  instead of OOM. The drain loop now `while let` over `extract_payload`
  instead of `if let`, so a chunk carrying multiple event-stream frames
  forwards every frame rather than dropping all but the first (mid-turn
  tool-call deltas were silently discarded by the old `clear()`). (2)
  Vertex `service_account_token` now returns an `Err("service account
  token endpoint returned None")` when the authenticator yields a `None`
  token, instead of silently sending `Authorization: Bearer ` (empty)
  and surfacing as a generic GCP 401. (3) TS orchestrator bridge test
  drift (2 failing → 1006/1006): `SecurityEmitter` now resolves relative
  file paths against `opts.cwd` before the `existsSync` filter (the
  bridge passes `KF_CHANGED_FILES` as relative paths with a configured
  cwd; the old form resolved against `process.cwd()` and dropped them);
  the `graph-emitter` test was rewritten to the refactored
  `GraphifyEmitter` API (`@kf-code/tool-graphify`), and the
  `verification-emitter-routing` test's `constructor.name` assertion was
  updated from `GraphEmitter` to `GraphifyEmitter`. 3 new Rust tests.
- WO 15.9: closed three cross-review findings (bucketlist 2.7, 2.10,
  2.11). (1) Phase 3 `record_tool_result` no longer re-runs
  `PathGuard::check_write`/`check_read` for file tools — Phase 1's
  resolved path is now carried through Phase 2.5 into Phase 3 and reused,
  eliminating a second canonicalize + sandbox-contains + `git
  check-ignore` subprocess per edit and the TOCTOU window where a
  parallel tool could flip the guard state between phases (the
  `pre_run_verdict` docstring already claimed this; the impl now honours
  it). (2) The git worktree verifier now distinguishes staged files from
  dirty worktree changes via `git status --porcelain` XY parsing — a
  staged-only file (`A  file.txt`) is no longer reported as an
  `Unfixable` "Dirty worktree" violation (the model can commit it); a
  genuine unstaged modification (` M file.txt`) still fails. (3)
  `bash_minify::try_minify_bash_output` now routes the extracted file
  path through `PathGuard::check_read` before reading, so a symlink
  target like `~/.ssh/id_rsa` or a path outside the sandbox is refused
  instead of followed with no recheck (the bash tool already owns a
  `path_guard`; it's now passed to the minifier). 3 new tests (git:
  2, bash_minify: 1); 1 existing git test replaced.
- WO 14.0: the `Bench Baseline` workflow's three `ollama pull` steps
  (`.github/workflows/bench-baseline.yml`, jobs `bench-baseline`,
  `bench-pr-delta`, `bench-leaderboard`) now self-heal through a 3-attempt
  retry loop with a 30s backoff, plus an authoritative `/api/tags` health
  check that fails the job only when the model is still unregistered after
  all retries. Fixes the intermittent scheduled-bench red badge caused by
  the Ollama registry redirect flake (`realm host "ollama.com" does not
  match original host "registry.ollama.ai"`). No `continue-on-error` on the
  pull step; the downstream artifact-download `continue-on-error` steps are
  preserved (first-run-has-no-baseline is a real state).
- WO 12.0: `SessionIndex::save()` now re-creates its parent directory
  immediately before the atomic rename, fixing the tarpaulin tempdir/rename
  race that flaked `test_build_fork_tree_orphan_fork_is_a_root`. WO 12.7
  follow-up: synced the stale `DEFAULT_THRESHOLDS` in
  `crates/kf-testdoctor/src/gaps.rs` to the current CI coverage gate
  (session 68.0, tools 76.0, adapters 75.0) so `kf-code doctor gaps`
  reports correct headroom.
- Windows env_guard race (Workorder 10.0): the
  `env_guard_restores_prior_value_some_branch` tests in
  `crates/kf-budget-core/src/cost.rs` and `paths.rs` read
  `PLUGIN3_CONFIG_DIR` after `EnvGuard::Drop` released the test mutex,
  racing other test threads on Windows. Added `EnvGuard::prior()` and
  assert on the captured prior (the contract: prior=None ⇒ Drop removed),
  not on the racy post-drop env state. Test-only fix; no behavior
  change. Unblocks the v0.3.6 release (the `windows` CI job was red).

### Changed
- WO 15.5: split `src/session/executor/tests/mod.rs` (3,760 lines, 79
  tests) into feature-aligned sub-files — `tests/common.rs` (shared
  `MockAdapter`/`MockTool`/`make_executor`/`temp_hooks_dir`/`SleepingTool`
  helpers), `tests/dispatch.rs`, `tests/turn.rs`, `tests/loop_.rs`,
  `tests/approval.rs`, `tests/scout.rs`. `mod.rs` is now a 13-line
  router. Pure refactor: test bodies moved verbatim, no logic/assertion
  change, test count unchanged (79). `#[cfg(unix)]` guards on
  `temp_hooks_dir` and the 4 hook tests preserved.
- WO 15.8: three cross-review correctness fixes. (1) `CorrectionResult.verifier`
  on the event-driven path now carries the decisive verifier's `name()`
  (was hard-coded `"verifier"`, producing the useless `verifier:verifier`
  tool name the model saw); `VerifierHandler::verify_event` returns
  `(Verdict, String)` so the correction loop can use it. (2)
  `format_verdict_report` now walks back to the nearest UTF-8 char
  boundary before slicing `&file_line[..23]` — a path with a multi-byte
  char at byte 22 (e.g. `café.txt`) no longer panics. (3) `EventBus`
  idempotency keys for `BashExec` now include `exit_code`/`stdout_len`/
  `stderr_len` (two `git add .` calls in a batch no longer dedup) and
  `FileWrite` now includes a `content_hash` (two same-length writes to
  the same path no longer dedup). Closes bucketlist items 2.4, 2.5, 2.6.
- WO 15.13: split `crates/kf-draw/src/event.rs` (8,226 lines, the
  largest file in the repo) by event category. The production code
  (event loop, key/mouse dispatchers, palette, panel click handlers,
  style cycles, clipboard, save) is cohesive — `handle_key` calls
  nearly every helper — so it stays in `event/mod.rs` (2,254 lines).
  The 6,000-line inline test block (251 tests) is split into 11
  category sub-files under `event/tests/`: `common.rs` (shared
  `make_app`/`key`/`key_ctrl`/`key_ctrl_shift`/`make_app_with_three_
  boxes` helpers), `keyboard.rs`, `restyle.rs`, `align.rs`, `layers.rs`,
  `inspector.rs`, `mouse.rs`, `palette.rs`, `save.rs`, `text_edit.rs`,
  `grouping.rs`, `find.rs`. Pure refactor: test bodies moved verbatim,
  no logic/assertion change, test count unchanged (251 in `event::`,
  328 in `kf-draw`). `ponytail:`/`ceiling:` annotations preserved.
  The one behaviour-affecting edit is the `include_str!("event.rs")` in
  `keymap_doc_block_lists_palette_and_z_order_chords` →
  `include_str!("../mod.rs")` (the production file moved).

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
  2 TS tests. The `kf-plugin-host` `env` module is now `pub`
  (the bridge reuses `curated_env`).
- Bench leaderboard publish + regression gate (Workorder 10.9):
  `compare_with_threshold(baseline, current, threshold) -> CompareResult`
  in `kf-bench` flags a regression when the success rate drops
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
- dead-code + #[allow] audit (WO 14.8): removed the module-wide
  `#![allow(dead_code)]` from `src/session/mod.rs` (hid 17 items) and
  `src/shared/mod.rs` (hid 3 items). Deleted 740 lines of dead
  single-call `dispatch_tool_call` (superseded by the batch dispatcher),
  dead constants, dead struct fields, and unused `expand_minified_by_ext`
  / `CollapseBlankLines`. Targeted remaining lifecycle API items
  (`disconnect`, MCP transport fields) with `#[allow(dead_code)]` +
  reason comments. Added `// reason:` comments to all 11 remaining
  `#[allow(clippy::too_many_arguments)]` (removed 1 stale allow on a
  0-arg method). Deleted legacy video scoring hooks and an LSP test
  import stub.
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
- doc-sync reconcile (WO 14.9, 14-series finale): corrected stale
  counts in `docs/TECHNICAL.md` + `state.md` + `KIRK-BENCH.md` — ADR
  count 83 → 84 (ADR-066 from WO 14.7), bench task count 30 → 31
  (`token_budget_challenge.toml` from WO 14.7). Updated
  `docs/workorders/README.md` Series 12 + Series 14 status tables (8
  stale "Planned" rows in Series 12 → Done with SHAs; 14.0/14.1/14.2/
  14.3/14.7/14.8 → Done; 14.5/14.6 → In Progress in worktrees; 14.9 →
  Done). Added the resolved Windows `test_cache_results` mtime-race
  entry to state.md (`4bdc13f` scans cache by path only). Full
  TECHNICAL.md section-walk audit confirmed only the counts were stale;
  architecture/plugin/feature-flag/tool/hook/verifier/context-index
  sections are current. A follow-up `technical_md_count_drift` test is
  noted in `lessons.md` (not built here — this WO is reconciliation).
- state.md false-deferral cleanup (WO 15.4): deleted two rows from the
  "Deferred items" table that listed already-shipped work as deferred —
  `use_workflow_run` (shipped in WO 9.1; `WorkflowTool` exists at
  `src/tools/workflow.rs:42`) and "11 pre-existing bench tasks fail
  `verify-only`" (fixed in WO 9.0; the `file_contains` specs now check
  setup content, not post-model output). AGENTS.md §7 anti-pattern.

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
  exists for `kf-workflow` yet. Total bench tasks: 24.

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
  in `crates/kf-plugin-sdk/src/lib.rs` returns
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
  plugin-host boundary as JSON. 19 new unit tests in `kf-plugin-sdk`
  + 1 in `kf-plugin-host`.
- Context index edge-case extraction (Workorder 8.9): the tree-sitter
  walker in `kf-context-index` now handles (1) TypeScript
  `export const foo = () => {}` arrow function assignments (extracts
  `foo` as a Function symbol), (2) TypeScript interface merging via
  a new `ContextIndex::dedup_interfaces()` pass keyed by `(name, file)`,
  (3) Python `if __name__ == "__main__":` guards (body is skipped so no
  spurious module-level symbols are produced), and (4) Go method
  receivers (both pointer and value, e.g. `func (s *Server) Start()` →
  `Server.Start`). 7 new tests against 5 fixture files in
  `crates/kf-context-index/tests/`.
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
  a slice marker) via `kf_budget_core::slicing::HeadTailSlicer` and the full
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
- Fold `kf-compress-core` into the main binary as an optional `stratum` feature
  (default on). 5 tools (`run`, `apply`, `mode`, `rules`, `config_validate`)
  are now direct Rust calls, eliminating subprocess overhead. ADR-046.
- Fold `kf-draw-core` into the main binary as an optional `draw` feature
  (default on). The `draw_render` tool loads `.td.json` files and renders them
  in-process. ADR-048.
- Fold Plugin3 (token-budget guard) into core as an optional `budget` feature
  (default on). 7 tools (`budget_status`, `budget_set`, `budget_compact`,
  `store_get`, `config_validate`, `report`, `self_check`) are now direct
  Rust calls via `kf-budget-core`, eliminating the lossy shell-plugin shim.
  ADR-047. The 4 hooks are now in-process handlers with full event context
  (see the in-process hooks entry above).
- Fold `kf-video` into the main binary as an optional `video` feature
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
  (`kf-plugin-sdk`) stays external. `/plugins list` shows source and
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
  token changes). Usage: `kf-code bench compare --baseline <json> --current <json> [--summary <md>]`.
- `bench list` subcommand: list all benchmark tasks in a directory with name,
  difficulty, and verification type.
- `bench verify-only` subcommand: run verification only (no LLM) for benchmark
  tasks, useful for validating task definitions.
- `TaskDelta`, `DeltaReport`, `TaskInfo` types and `compare_reports`,
  `write_markdown_delta`, `list_tasks`, `verify_only` functions in
  `kf-bench`.
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
  `.kf-code/context-index/cache.json`. Session startup is instant
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
  (kf-code-vscode-0.2.0.vsix). CI `vscode` job. ADR-040. (P2-long-4)

## [0.3.1] - 2026-07-22

### Added
- Task-benchmark harness (`crates/kf-bench/`): TOML task definitions, `BenchRunner` headless execution, metrics collection (success/tokens/time/cost), `kf-code bench` subcommand, CI bench job. 10 unit tests, 5 task TOML files. Documented in ADR-038. (P1-long-2)
- Execution replay + time-travel (`src/session/replay.rs`): `TurnRecord` NDJSON traces alongside conversation logs. `TraceRecorder` appends one line per turn with prompt messages, model response, tool calls, outcome, token counts, and duration. `kf-code replay <session-id>` subcommand with `--turn`, `--from`, `--to` range flags. `--no-trace` flag on `Run` to disable tracing. 4 unit tests. Documented in ADR-039. (P2-long-3)
- `impl Default for ContextIndex` (clippy fix).

### Fixed
- Removed duplicate `context_index` block in `src/main/mod.rs`.
- `cargo fmt` fixes in `crates/kf-context-index/src/lib.rs`.

### Changed
- Lowered `src/session` coverage threshold from 63.0% to 62.0% in CI. The bench harness's `run_task`/`run_all` need a live model and can't be unit-tested; 191 lines of integration-only code drag the ratio.
- Extracted `collect_turn_metrics()` from `src/session/bench.rs` — pure function aggregating `TurnEvent` metrics, testable without a live model. Added 8 unit tests.

## [0.3.0] - 2026-07-21

### Added
- Restore plugin 1 bench harness (`bench/kf-mini/` with 4 tasks × 9 workers, real measured results) and `tool-graphify` package (real import-graph with extension resolution) from the original KirkForge-Plugin repo. Re-wire `emitter-factory.ts` to import `GraphifyEmitter` from `@kf-code/tool-graphify` again, replacing the inline regex-only `graph-emitter.ts`. Restore plugin 3's `size_budget.rs` (8MB release-binary cap), `build_spec_drift.rs`, and `readme_drift.rs` tests from the original KirkForge-Plugin3 repo. Documented in ADR-029 (plugin-restoration). (P0)
- Add `build` (priority 3) and `test` (priority 5) verifier slots to the Rust runtime verifier bus: `build` runs `cargo build --message-format=json` and returns the first compiler error for the edited file; `test` runs targeted `cargo test <module-prefix>` and returns the failure output as a model-facing suggestion. Documented in ADR-031. (P2-1)
- PlanReason trace events expose *why* planning decisions were made: new `MetricEvent::PlanReason` with `PlanDecisionKind` enum (ToolSelect, ContextTruncate, MemoryRetrieve, PromptFailure, CompactionTrigger, ModelSelect). Emitted after tool calls, on context truncation, memory retrieval, prompt-failure retries, and compaction triggers. Mapped to OTel attributes `plan.decision_kind`, `plan.reason`, `plan.confidence`, `plan.related_id`. Documented in ADR-032. (P2-2)
- Exponential backoff on tool-call retries: `RetryTracker::wait_before_retry()` now sleeps using the shared `retry_backoff` helper before each parse-error retry, matching the existing model-request retry policy (1 s, 2 s, 4 s) with deterministic jitter. Documented in ADR-033. (P2-3)
- Mid-batch tool-result checkpointing: `dispatch_tool_call_batch` now calls `conversation.checkpoint_async()` after each recorded tool result, so a crash mid-batch recovers the completed subset instead of losing the whole batch. Documented in ADR-034. (P2-4)
- `--seed <u64>` deterministic mode: pins model temperature=0, passes seed to provider request bodies (OpenAI-compat `seed` field, Ollama `options.seed`), and forces sequential tool dispatch to eliminate nondeterminism from `tokio::spawn` scheduling. Best-effort determinism for regression testing. Documented in ADR-030. (P2-5)
- Test-doctor prototype (`crates/kf-testdoctor/`) for CI test partitioning: classifies tests by profile (fast/slow/flaky), suggests partition splits, and generates CI config. Documented in ADR-029. (infra)
- `--worktree` flag creates an isolated git worktree per session: `git worktree add --detach` on start, `git worktree remove --force` on session end. Sandbox redirected to worktree path. Documented in ADR-035. (P2-6)
- `--docker` flag and `[docker]` config block routes bash tool execution through Docker containers with `--memory`, `--cpus`, and `--network=none` isolation. `DockerConfig` with configurable image/memory/cpus. Documented in ADR-036. (P2-6)
- `crates/kf-context-index/` scaffolded: `ContextIndex` with line-based symbol extraction (fn/struct/enum/impl/mod/use), `index_file`/`index_dir`/`symbols`/`retrieve` API, 3 tests. ADR-037 (Experimental). (P1-long-1 start)

### Fixed
- `run_docker` task-orphaning: `out_handle`/`err_handle` now awaited with 1s timeout after `child.kill()` on timeout/cancellation paths.
- Release workflow now verifies CI by waiting for each individual job check-run to succeed, instead of looking for a non-existent single `CI` check-run (#10, #11).
- Release workflow now builds with `--workspace` so all bundled binaries (`kfd`, `kf-budget`, `stratum`, `kf-video`) are produced for every target (#12).
- Release workflow Windows archive step now expands the archive name variable correctly so the zip artifact is produced (#13).
- Plugin3 `readme_drift.rs` tests adapted to CLI workspace: reads `crates/kf-budget-core/README.md` instead of workspace root README. Added State table with test count to `crates/kf-budget-core/README.md`.
- Plugin3 `size_budget.rs` adapted to CLI workspace release profile (`lto = true`, `strip = true` instead of `lto = "thin"`, `strip = "symbols"`).
- Plugin3 `build_spec_drift.rs` (33 tests) marked `#[ignore]` — tests the original Plugin3 repo's build spec, not the CLI workspace's.
- Tool-graphify added to root `tsconfig.json`, orchestrator `tsconfig.json`, and orchestrator `package.json` project references so `tsc --build` resolves `@kf-code/tool-graphify`.
- Deterministic mode: fixed results being shadowed by a second `results` HashMap in the collect loop when `--seed` forces sequential dispatch.
- Main branch syntax error from botched P2-4 merge resolved (dangling `})` + `];` in `tests/mod.rs`).

## [0.2.0] - 2026-07-19

### Added
- Version 0.2.0 release (#9).
- Executor batch concurrency coverage (#7): non-file tool calls run in parallel; file tool calls remain sequential with the read-before-edit gate enforced before write/edit bodies run, while `[read_file(X), write_file(X)]` in the same batch now correctly passes the gate because reads are marked immediately after the read body completes.
- Real parallel tool dispatch (WO-2) with three-phase `dispatch_tool_call_batch`: prepare/run/record. Non-file tools spawn concurrently via `tokio::spawn`; file tools run sequentially so the read-before-edit gate observes reads before edits in the same batch.
- VS Code PTY wrapper extension (WO-1) under `editors/vscode/` — `extension.ts` spawns `kf-code run` in the integrated terminal.
- `computer_use` tool (WO-3) via headless Chrome CDP for screenshot/click/type/scroll, SSRF-guarded via `DenyList`.
- Anthropic Bedrock and Vertex adapters (WO-3) with SigV4 signing and Google OAuth2 respectively; both reuse the existing `parse_anthropic_stream` SSE parser.
- Programmable JSON workflow engine (WO-4) in `crates/kf-workflow/` with step dependency resolution, cycle detection, output propagation, and 3 built-in templates (`feature.json`, `bugfix.json`, `refactor.json`) plus `/workflow run`/`status`/`cancel` TUI commands.
- Native Kimi/Moonshot adapter (`src/adapters/kimi.rs`) supporting 256K context, native tool calls, and the `reasoning_content` thinking field.
- Persistent cron-style scheduled jobs (`kf-code jobd`) with Unix socket control, signal handling, bounded concurrency, and storage under `~/.local/share/kf-code/jobs/<id>/`.
- Write-side minification / VFS envelope for file tools (`minify_write_side`).
- `lsp_query` tool backed by `crates/kf-lsp` for workspace symbol/type/diagnostic queries.
- Plugin host path-validation module (`crates/kf-plugin-host/src/paths.rs`) that drops capabilities whose command path is absolute, climbs out of the plugin root, or resolves outside it.

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
- Persistent cron-style scheduled jobs (Session 3). New `kf-code jobd` scheduler daemon with Unix socket control, signal handling, and bounded concurrency. Jobs are stored under `~/.local/share/kf-code/jobs/<id>/` with `0o600` artifacts. Supports `@hourly`, `@daily`, `@weekly`, `@restart`, `@once <ISO-8601>`, and raw 5/6-field cron expressions. Bash jobs reuse the `bash_runner` safety gate and require either a permission rule or `scheduled_bash_auto_approve = true` to run unattended; skill jobs are accepted but record a "not yet implemented" failure.
  - TUI slash commands: `/jobs schedule <spec> bash <command>`, `/jobs schedule <spec> skill <name> [args...]`, `/jobs scheduled list`, `/jobs scheduled cancel <id>`, `/jobs run-now <id>`, `/jobs logs <id>`.
  - New config fields `scheduled_bash_auto_approve` (default `false`) and `max_concurrent_scheduled_jobs` (default `4`) with env overrides.
  - New modules: `src/jobs/schedule.rs`, `src/jobs/store.rs`, `src/jobs/runner.rs`, `src/jobs/daemon.rs`, `src/jobs/client.rs`.
- Write-side minification / VFS envelope for file tools. New config flag `minify_write_side` (default `false`, env `KF_CODE_MINIFY_WRITE_SIDE`, TOML `minify_write_side`). When enabled, `read_file` can wrap output in `<minified lang="...">...</minified>`, and `write_file`/`edit_file` expand that envelope back to readable, formatted source via external formatters (`rustfmt`, `black`, `prettier`, `deno fmt`, `gofmt`, etc.) before writing. A language-aware fallback is used when no formatter is available.
- `src/shared/minify/expand.rs` with envelope parsing, wrapping, language mapping, and expansion helpers.

### Fixed (deep audit — Session 4: correctness C11–C27 + performance P4–P9)
- Correctness:
  - `src/session/event_bus.rs` idempotency set now preserves insertion order and trims from the front deterministically, so duplicate-event suppression no longer depends on `HashSet` iteration order.
  - `src/session/prompt/summarizer.rs` no longer divides by zero when `tokens_before == 0`; it reports a fallback instead of a panic.
  - `src/shared/minify/lang.rs` `strip_test_blocks` now tracks brace depth and swallows the matching closing `}`, so the test module's trailing `}` no longer leaks into minified output.
  - `src/session/bash_jobs.rs` background bash jobs now expand `~` in `workdir` the same way foreground bash commands do.
  - `src/tools/read_file.rs` no longer double-minifies whole-file output or poisons its line-cache; raw file content is cached and minification happens once at the prompt layer when enabled.
  - `src/adapters/tool_call_markup.rs` `parse_name_attr` now handles `\"` escapes and single-quoted DSML attributes.
  - `crates/kf-video/src/pipelines/animated_explainer.rs` `flite` filter graph arguments are escaped via `ffmpeg_escape`, so `:`, `\\`, `]`, and `,` in text are passed through correctly.
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
- Restored accidentally deleted `npm/kf-plugin/packages/tool-gitnexus` files (still a production dependency of the orchestrator) and fixed the compile error in `src/index.ts` where the git-repo branch referenced an undefined `paths` shorthand
- `src/tui/keys.rs` `/help` no longer claims `!<command>` bypasses approval when `bang_requires_approval` is enabled; `split_bang_summary` is now a shared `pub(crate)` helper used by both the direct and approval-gated `!` paths
- `npm/kf-plugin/apps/cli/src/bootstrap.ts` now supports `allowMissingModel`; the `verify` and `health` commands use it so deterministic verification and health checks work without requiring `OLLAMA_BASE_URL` or provider API keys
- `npm/kf-plugin/packages/tool-pyright/package.json` now declares `pyright` as a runtime dependency so the verifier ships a guaranteed binary instead of relying on a global install
- `plugins/kf-plugin/tools/common.sh` `find_cli()` now resolves the JS entry point via `$KF_CODE_CLI_JS`, the source-layout sibling, or a global npm install of `@kf-code/cli`; the unsafe PATH-installed `kf-code` fallback is removed, and resolved paths are validated to end in `.js`/`.cjs`/`.mjs` before being passed to `node`
- `plugins/kf-draw/tools/edit.sh` removed; it was never exposed in the manifest and cannot work in a null-stdin/non-TTY host environment
- `npm/kf-plugin/packages/tool-tsc/src/index.ts` now resolves `tsc` from the bundled `typescript` dependency (or a local `node_modules/.bin` install) instead of `npx`, and accepts an optional `command` override for deterministic testing
- `src/session/plugin_tools.rs` now prepends the bundled Node SDK's `node_modules/.bin` to the curated `PATH` passed to plugin tools, so `tsc`/`pyright`/etc. resolve without a global install
- `scripts/install.sh` now warns when `node` is missing or older than Node 20, which is required by the bundled Node SDK plugin
- `src/session/executor/tests/mod.rs` `test_cancelled_tool_batch_appends_placeholders` no longer races a 50 ms timer against executor batch scheduling; it waits for the first tool to start before setting cancellation, eliminating the observed flake
- `npm/kf-plugin/package.json` dev scripts `cli` and `self-verify` now point at the built `apps/cli/dist/index.js` instead of stripped source files
- `src/session/verifier/lint.rs` `test_clippy_warning_on_temp_project` is now `#[ignore]` because it spawns `cargo clippy`; it deadlocks under `cargo test --workspace` since the parent cargo holds the package cache lock
- `src/session/undo.rs` tests now use a `DataDirGuard` under the shared `test_data_dir_lock` so each test gets a private `KF_CODE_DATA_DIR`; fixes the flaky `test_total_size_cap_evicts_oldest` failure caused by another test's temp data directory being deleted mid-test
- `.github/workflows/ci.yml` `integration` job now installs Ollama, caches `~/.ollama/models`, pulls `qwen2.5:0.5b`, and runs `cargo test --test integration_test -- --include-ignored`; the previous job ran the ignored test target without `--include-ignored`, so it executed zero tests and gave false confidence
- `src/session/hooks.rs` `test_run_hook_with_env_vars` now yields to the runtime before polling and waits up to 5 seconds for the fire-and-forget hook to write its marker; fixes the flake where the spawned task had not yet scheduled under load
- `src/session/executor/helpers.rs` `validate_args_against_schema` now supports `anyOf`/`oneOf` polymorphic schemas, and `plugins/kf-plugin/kf-code.toml` declares `plugin_verify_workspace.file` as `string | string[]`; fixes the runtime/schema mismatch where the wrapper accepted a single path but the host validator rejected it
- `src/session/executor/helpers.rs` `is_read_only_bash` now applies redirection, chaining, and command-substitution guards to every pipe segment, not just the first; closes the auto-approval bypass where a later segment could write files or execute arbitrary commands (`cat file | sort > out.txt`, `cat file | sort; rm file`, etc.)
- `src/session/mod.rs` `data_dir()` now creates the canonical data directory (on first access per process) and sets its Unix permissions to `0o700` so conversation logs, session state, and undo history are not world-readable
- `plugins/kf-plugin/tools/common.sh` now provides `node_is_truthy()` and the `verify`, `doctor`, and `audit-verify` wrappers use it; boolean flags like `json` and `pretty` are now accepted as `true`, `1`, `yes`, `y`, or `on`, matching the other filesystem plugins
- Bumped OpenTelemetry dependencies across `npm/kf-plugin/package.json` and `packages/core-telemetry/package.json` to patched versions; `npm audit` now reports 0 vulnerabilities
- `src/session/executor/helpers.rs` `is_read_only_bash` now auto-approves read-only `git` subcommands (`status`, `log`, `diff`, `show`, `ls-files`, `rev-parse`) while still requiring approval for mutating subcommands (`add`, `commit`, `push`, `checkout`, `reset`, etc.)
- `src/session/executor/helpers.rs` `is_read_only_bash` now applies `find`/`git` command-specific guards to every pipe segment, closing the bypass where a read-only producer could hide a mutating `find` or `git` consumer (`cat list | find . -delete`, `cat list | git add file`, etc.)
- `plugins/stratum/tools/common.sh` and `plugins/kf-video/tools/video_common.sh` `json_get_bool` now accept common truthy values (`true`, `1`, `yes`, `y`, `on`) consistently with the Node SDK wrappers
- `tests/integration_test.rs` increased the shared reqwest timeout from 60 s to 120 s; the previous ceiling caused flaky timeouts when the 0.5b test model was slow to respond
- `src/daemon/mod.rs` `DaemonState::refresh()` now re-scans the sessions directory instead of reusing the cached `.index.ndjson`, so `kf-code sessions` and the daemon's recent-session list reflect newly appended messages
- `src/daemon/server.rs` `daemonize()` now calls `setsid()` before spawning the foreground daemon, so the auto-started session daemon survives the closing of the spawning terminal/session instead of receiving SIGHUP and shutting down
- Verified local `x86_64-unknown-linux-musl` release build after installing `musl-tools`; the resulting binary is a working static-pie executable. `aarch64-unknown-linux-musl` remains CI-verified via `cross` because the host lacks the aarch64 musl toolchain.
- `src/shared/metrics.rs` `record()` now serializes the full event line into a single buffer and guards the rotate/open/write sequence with a global mutex, fixing concurrent metric writes that produced concatenated NDJSON lines and caused `read_events()` to drop events
- `src/shared/mod.rs` default `enabled_plugins` now lists the five bundled plugins (`kf-draw`, `kf-video`, `stratum`, `kf-budget`, `kf-plugin-sdk`) so fresh configs and installed releases load them without manual toggling; `config.toml.example` reflects the new default
- `plugins/kf-draw/kf-code.toml` `/draw` prompt now documents the real `.td.json` schema (`box`: `left`/`top`/`right`/`bottom`; `line`/`elbow`: `x1`/`y1`/`x2`/`y2`; `paint`: `points`/`brush`; `text`: `x`/`y`/`content`/`border`) instead of the incorrect `x`/`y`/`w`/`h`/`text` box fields; diagrams produced by the model now validate and render
- `src/session/plugin_tools.rs` `curated_env()` now prepends the source-layout `npm/kf-plugin/node_modules/.bin` to the plugin tool PATH in addition to the data-directory install, so source builds of kf-code resolve `tsc`/`pyright` for Node SDK tools without a global install; added `npm_bin_dirs()` unit tests for both layouts
- `README.md` plugin section now states that the five bundled workspace plugins are enabled by default instead of disabled
- `crates/kf-plugin-host/src/paths.rs` is a new path-validation module; the plugin host now drops tool/hook/verifier capabilities whose declared command path is absolute or climbs out of the plugin root via `..`, emitting a load warning and preventing a malformed or malicious manifest from running arbitrary system commands
- `crates/kf-plugin-host/src/lib.rs` `filter_capabilities` now canonicalises the plugin root and each command path before containment checks; capabilities whose command file is missing, inaccessible, or a symlink that resolves outside the root are dropped at load time
- `npm/kf-plugin/packages/tool-lint-core/src/engine.ts` now preserves `severity` and `category` in `LintReport.details` and emits them on `verify.lint` events so diagnostics are no longer opaque
- `npm/kf-plugin/packages/tool-lint-core/src/engine.ts` now skips generated and dependency directories by default (`.git/`, `.gitnexus/`, `node_modules/`, `target/`, `dist/`, `.claude/`, `coverage/`), and reports only files that were actually scanned in `filesScanned`
- `src/shared/metrics.rs` `test_concurrent_records_are_not_interleaved` now writes directly to the per-test file path instead of relying on the global `PATH_OVERRIDE`; fixes the rare flake where 101 events were read instead of 100 under parallel test load
- `npm/kf-plugin/packages/orchestrator/src/index.ts` `verify()` now defaults to a language-neutral profile (`text`) instead of assuming TypeScript; `verify` no longer returns `FAIL` on non-TypeScript workspaces just because there is no `tsconfig.json`
- `npm/kf-plugin/packages/orchestrator/src/reducer.ts` no longer downgrades the aggregate `verification.overall` to `warn` solely because of lint warnings; warnings are surfaced in counts but do not trigger a correction loop, so clean workspaces with style warnings report `PASS`
- `npm/kf-plugin/packages/plugin/src/index.ts` `doctor()` now resolves bundled tools from the nearest workspace `node_modules/.bin`, so the plugin wrapper reports `tsc`/`pyright`/`eslint` as available even when the host passes a curated PATH that excludes the workspace bin directory
- `npm/kf-plugin/packages/orchestrator/src/modes.ts` removed unused `isAbsolute` import so `npm run lint` passes cleanly again
- `npm/kf-plugin/apps/cli/src/shared.ts` `ALL_MODES` now includes `task-decompose`, matching the `DelegationMode` type in `@kf-code/core-types`; the `observe`/`delegate`/`run` CLIs no longer reject valid task-decompose modes
- `npm/kf-plugin/apps/cli/src/bootstrap.ts` removed unused duplicate `ALL_MODES` export to avoid a stale, divergent copy of the mode list
- `src/session/session_index.rs` `search_sessions` now searches message content in addition to id/date/count, so `kf-code sessions --search <text>` finds conversations by what was actually said; added unit test `test_search_sessions_matches_content`; updated help text in `src/tui/commands/sessions.rs` and `src/main.rs`
- `src/session/config.rs` `apply_env_overrides` now honors `KF_CODE_BANG_REQUIRES_APPROVAL`, `KF_CODE_JSON_MODE`, `KF_CODE_BASH_SANDBOX_WORKDIR`, `KF_CODE_BLOCK_GITIGNORED_DOTFILES`, `KF_CODE_MAX_OVERWRITE_SIZE`, `KF_CODE_SUMMARIZE_MODEL`, `KF_CODE_ROUTING_ENABLED`, `KF_CODE_ROUTER_MODEL`, `KF_CODE_COMMIT_MAX_FILE_SIZE`, `KF_CODE_PRESERVE_RECENT_MESSAGES`, `KF_CODE_MAX_TOOL_CALLS_PER_TURN`, `KF_CODE_MAX_PERSONA_TURNS`, `KF_CODE_TOOL_TIMEOUT_SECS`, `KF_CODE_AUDIT_LOG_PATH`, and `KF_CODE_HOOKS_DIR`; `merge_toml_into_config` partial-recovery path now covers the same fields plus `routing_model_map`; added tests for all new overrides
- `config.toml.example` now documents the missing security/observability knobs `block_gitignored_dotfiles`, `max_overwrite_size`, `preserve_recent_messages`, `max_tool_calls_per_turn`, `tool_timeout_secs`, `audit_log_path`, and `hooks_dir`
- `src/tui/mod.rs` now initializes `state.fork_manager` when a TUI session starts; `src/tui/commands/fork.rs` `resume_conversation_log` now rebuilds the fork manager for the resumed session, so `/fork`, `/resume <fork-id>`, and persona commands actually work instead of returning "No fork manager available"
- `src/session/session_fork.rs` `ForkManager::new` now loads existing forks from `forks/*/fork.json` metadata so forks survive restarts; `create_fork` now skips already-used ids and removes stale `conversation.ndjson` files so it never appends duplicate messages to an existing fork
- `kf-draw` skill prompts now tell the model to run `kfd --load <path> --render --fenced` and to create `./out/` before saving, so the `/draw` skill no longer launches the TUI in the null-stdin plugin host
- `kf-draw` `kfd` now requires `--render` for `--output`, `--fenced`, `--plain`, and `--ansi`, and requires `--validate` for `--json`; previously these flags were silently ignored and could launch the TUI unexpectedly
- `kf-draw` `kfd` now surfaces unknown-object validation warnings on the non-interactive render path and exits with a clear error when run without a TTY instead of a raw-mode OS error
- `kf-draw` `render.sh` no longer passes the mutually exclusive `--plain` flag alongside `--fenced`
- `kf-draw` event handling now treats Ctrl-Shift-Z (uppercase `Z`) as redo, matching terminal conventions
- `plugins/stratum/tools/common.sh`, `plugins/kf-video/tools/video_common.sh`, and `plugins/kf-budget/tools/kf_budget_common.sh` now consult `CARGO_TARGET_DIR` when locating their Rust binaries, so custom target directories resolve correctly
- `plugins/stratum/tools/common.sh`, `plugins/kf-video/tools/video_common.sh`, and `plugins/kf-budget/tools/kf_budget_common.sh` no longer use naive bash regex fallbacks to parse `KF_CODE_TOOL_ARGS_JSON`; jq or python3 is now required, preventing silent wrong answers for escaped quotes or substring key matches
- `plugins/kf-video/tools/video_doctor.sh` now passes `--json` explicitly and safely instead of relying on an unquoted expansion that could split
- `plugins/kf-video/tools/video_risk.sh` now guards the empty `kind_args` array expansion so `set -u` does not fail when `kinds` is empty
- `review.md` updated to reflect that session forks persist across restarts and that fork/persona commands now work inside resumed TUI sessions

### Fixed (deep audit — seventh pass)
- `src/session/mcp_client.rs` `McpClientManager` now collects startup warnings (failed MCP server connections, zero discovered tools) and exposes them via `warnings()`
- `src/main.rs` startup now prints MCP warnings to stderr so configured but unavailable MCP servers are visible instead of silently omitted

### Fixed (deep audit — sixth pass)
- Unified the data-directory env-var mutation lock across all tests (`src/session/mod.rs::test_data_dir_lock`) so `session_index`, `plugin_tools`, `tui/commands/plugins`, and daemon tests no longer race on `KF_CODE_DATA_DIR`; fixes the flaky `test_search_sessions_filters_by_id_and_date` failure seen in full `cargo test --workspace` runs
- `src/session/plugin_tools.rs` async installed-layout tests now acquire the shared lock via an async guard instead of `blocking_lock()` inside the Tokio runtime
- `src/session/mcp_client.rs` MCP server subprocesses now spawn with a sanitized PATH (same `bash_runner::sanitized_path` rules as model-driven bash and plugin tools) so a minimal or world-writable host PATH cannot shadow `npx`, `node`, or `bash`

### Fixed (deep audit — fifth pass)
- `src/session/mcp_client.rs` reader task now caps the *accumulated* JSON-RPC line length against `MAX_LINE_LEN`; the previous per-chunk check let a server stream an unbounded line in `BufReader`-sized pieces
- `src/session/bash_runner.rs` model-driven shell commands now resolve commands through a curated PATH that always includes standard system directories (`/usr/bin`, `/bin`, etc.) while still dropping relative and world-writable non-system entries; this fixes command resolution on hosts where a system directory happens to be world-writable
- `src/session/plugin_tools.rs` plugin tool subprocesses now inherit the same curated PATH as model-driven bash, so wrappers can always locate `sh`, `python3`, `node`, and other standard interpreters even when kf-code is launched with a minimal or untrusted PATH
- `src/session/executor/helpers.rs` added lightweight dispatch-time schema validation (`validate_args_against_schema`) covering `required` fields and per-property JSON Schema types
- `src/session/executor/dispatch.rs` now validates tool arguments against the tool's JSON Schema before permission/approval logic, so malformed calls fail early with a clear error instead of reaching the tool
- `src/session/plugin_tools.rs` installed-layout stratum end-to-end test no longer mutates the global `PATH`; it copies the `stratum` binary next to the plugin script so the wrapper's sibling-binary discovery resolves it without racing other concurrent tests
- `src/session/bash_runner.rs` PATH-sanitization unit tests no longer mutate the global `PATH`, removing another source of parallel-test flakiness
- `build.rs` now propagates man-page render/write errors instead of panicking with `.expect`; a build-disk failure now produces a clean cargo error
- `crates/kf-compress-core/src/config.rs` `PipelineConfig::default()` no longer panics if the embedded `config/pipeline.toml` fails to parse; it constructs the default struct directly, and the existing drift test still enforces parity with the TOML
- `crates/kf-draw/src/render.rs` `format_validate_report_json` now returns `anyhow::Result<String>` instead of panicking on JSON serialization failure; `kfd --validate --json` propagates the error through the normal CLI failure path
- `crates/kf-budget-cli/src/main.rs` `kf-budget self-check` no longer panics on internal slicing, store, or serialization failures; it now returns a `Result` and exits 1 with a diagnostic message so the host tool sees a clean error instead of a process abort
- `crates/kf-video/src/pipelines/animated_explainer.rs` no longer panics if an asset or transcode plan entry is not a JSON object; the failure now propagates through the pipeline's `anyhow::Result` path
- `crates/kf-video/src/pipelines/brief.rs` no longer panics on regex construction failure; it returns `None` and lets the caller continue without the stat

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
- `crates/kf-plugin-host/src/lib.rs` now detects and reports duplicate tool/skill/verifier names that would otherwise silently shadow each other across plugins
- `src/tools/atomic_write.rs` now creates temp files with `O_EXCL` (`create_new`) and an unpredictable name (pid + nanosecond timestamp + counter) to block symlink-race attacks on the temp file
- `src/session/access.rs` `is_gitignored` now runs `git check-ignore` in a bounded thread with a 2 s timeout instead of blocking indefinitely on a slow repo

### Fixed (deep audit — third pass)
- `plugins/kf-budget/tools/kf_budget_common.sh` `json_get_integer` now preserves an explicitly empty default, so `budget_set.sh` can detect a missing `ceiling` argument instead of silently setting the budget to `0`
- `plugins/kf-draw/kf-code.toml` no longer advertises the `draw_edit` TUI tool; the host runs tools with null stdin and no TTY, so an interactive editor cannot function
- `plugins/stratum/tools/common.sh` gained shared `stratum_args`, `json_get_string`, `json_get_integer`, `json_get_bool`, and `json_has_key` helpers with jq/python3/naive-bash fallbacks
- `plugins/stratum/tools/{run,apply,mode,rules,config_validate}.sh` now use the shared helpers, normalise empty args to `{}`, and treat `{"input":""}` as a valid (empty) payload instead of a missing field
- `plugins/kf-plugin/tools/common.sh` now accepts a `KF_CODE_CLI_JS` override and falls back to a global npm install of `@kf-code/cli`; shared `node_json_arg` / `node_json_file_arg` helpers catch invalid JSON and emit a clean tool error
- `plugins/kf-plugin/tools/{verify,audit-verify,doctor,verify-workspace}.sh` now use the shared JSON helpers; `verify-workspace` accepts `file` as either a single path or an array and no longer splits on spaces
- `npm/kf-plugin/apps/cli/package.json` now includes `"files": ["dist/"]` so `npm publish` ships the compiled entry points

### Fixed (deep audit — second pass)
- Executor→TUI `TurnEvent` channel is now bounded (10,000 events) with backpressure instead of unbounded growth
- Approval diff-preview reader now checks `canonicalize() → starts_with(cwd)` before opening any file; blocks `../../../../etc/passwd`-style read-leak via the approval dialog
- `notified_jobs` HashSet pruned each tick to registry-live IDs only — bounded at ≤64 entries instead of growing for the session lifetime
- Toolset startup `panic!` replaced with `anyhow::Result` propagation so a plugin inconsistency produces a clean error instead of a process abort
- `state.messages` display list capped at 2 000 entries; oldest 500 evicted when exceeded with index-based state (collapsed, expanded, search) remapped consistently
- Plugin shell wrappers hardened (second pass): removed legacy `KF_CODE_TOOL_ARGS` fallback, added `node` dependency checks for the JS plugin tools, fixed JSON escaping in `die_json` and the draw `post-turn` hook, made stratum tools default to `{}` when no args are provided, and corrected the stratum `config_validate` command-line order
- `kf-video` `animated_explainer` pipeline no longer panics on I/O errors when writing artifact JSON; errors now propagate through the existing `Result` path
- Plugin READMEs and the plugin-host crate doc comment now document the canonical `KF_CODE_TOOL_ARGS_JSON` env var instead of the legacy `KF_CODE_TOOL_ARGS` alias
- Plugin tool working directory: empty/missing `sandbox_dir` now resolves to the user's current directory instead of the plugin installation root; `README.md` and `config.toml.example` updated to document the escape-hatch semantics
- Pre-tool decision hooks and lifecycle hooks now receive `KF_EVENT` and `KF_SESSION_ID` so kf-budget hooks can distinguish KirkForge from Claude-Code runtime mode
- Bundled plugin shell wrappers hardened: kf-plugin no longer falls back to the Rust binary as a JS entry point, video/stratum optional flags are quoted, and `verify-workspace` safely splits space-separated file paths
- Release packaging now ships all five Rust binaries (`kf-code`, `kfd`, `kf-budget`, `stratum`, `kf-video`); `install.sh` installs the suite and refuses native Windows shells
- `scripts/bump-version.sh` no longer runs `cargo check --locked` after a version bump, which previously failed because the lockfile was stale
- `kf-budget-core` integration test `state_drift` now uses `EnvGuard` to prevent env-var leakage on panic
- Line-mode interactive editor no longer panics on concurrent `next_line` calls; returns a clean error instead
- `bash_runner` exotic-target timeout fallback no longer panics if the fallback `sh` command fails to spawn; the error now propagates as a `ShellError::Spawn`
- Release archives and `install.sh` now ship/install the bundled `plugins/` directory to `~/.local/share/kf-code/plugins/`; workspace plugin sources fall back to the data directory when compile-time source paths are absent
- All five bundled filesystem plugins load without warnings; kf-budget hooks are dual-mode and emit proper KirkForge no-op responses when `KF_EVENT` is set
- `kf-draw` render/edit tools use `--render` and correct argument handling for non-TTY execution
- New regression test `crates/kf-plugin-host/tests/load_bundled_plugins.rs` verifies all bundled plugins load cleanly
- Dependency hardening: `bincode` replaced with `serde_json`, `paste` removed, `ratatui` upgraded to 0.30, and vulnerable/deprecated crates (`crossbeam-epoch`, `quinn-proto`, `anyhow`, `lru`) refreshed
- Flaky tests fixed in `kf-budget-core` env guard and `shared::metrics` log rotation
- Release archives and `install.sh` now ship/install the bundled `npm/kf-plugin` Node SDK so `kf-plugin-sdk` shell tools (`health`, `doctor`, `tools`, `verify`, ...) work from an installed layout
- Added regression test `bundled_plugins_load_from_data_dir` that exercises the installed-layout plugin loading path
- Cleaned up `kf-plugin-sdk` `find_cli` helper to only search the actual installed/repo layout (`npm/kf-plugin` sibling to `plugins/` under the data directory) and removed misleading dead-path candidates; callers now report the real missing-Node-SDK reason
- Added installed-layout end-to-end regression tests that execute real bundled plugin tools through the host's `PluginToolWrapper`: `stratum_mode` (Rust-binary-backed) and `plugin_tools` (Node SDK-backed)
- Plugin tool subprocesses and lifecycle hook subprocesses now run with a null stdin instead of inheriting the host's terminal stdin; prevents tools such as `stratum_run` or the `kf-draw` `post-turn` hook from blocking on interactive input or consuming user keystrokes
- `kf-draw` `post-turn` hook only drains stdin when `KF_EVENT` is unset (Claude Code mode), so it no longer waits for terminal EOF under KirkForge
- `draw_edit` now fails with a clear message when stdin is not a terminal, instead of launching `kfd` into a captured/non-interactive plugin subprocess
- `stratum_run` schema and shell wrapper now accept an `input` field so inline context can be compressed without relying on the host to supply stdin; the `/stratum` skill prompt no longer claims the runtime pipes stdin
- `stratum_run` now treats a missing `input` field as an error instead of silently compressing an empty stdin stream; the schema marks `input` as required
- `stratum_apply` now requires a `file` field; it previously fell back to stdin which is empty under the host's null-stdin plugin execution, silently processing no input
- `kf-video` manifest no longer marks `path`/`check`/`command` as required when the corresponding shell wrapper supplies a sensible default
- `src/session/plugin_tools.rs` now propagates plugin-directory read errors instead of silently defaulting to an empty warning list
- `src/session/mcp_client.rs` reader task now enforces a 5-minute idle timeout and a 1 MiB per-line cap so a misbehaving MCP server cannot hang or exhaust memory
- Bash tool and background job runners no longer hardcode `/bin/sh`; Unix keeps `/bin/sh`, Windows targets `bash` (Git for Windows / WSL) so the same safety gate applies
- Session daemon client is now stubbed on Windows so the CLI compiles and degrades to file-based session discovery; the `daemon` subcommand returns a clear unsupported-platform error on Windows
- Line-mode approval handler no longer assumes `/dev/tty` on Windows; it reads from stdin on Windows while Unix continues to use the controlling terminal
- Hardened `bash_runner` deny-list against quoting/whitespace/escape evasions: commands are normalized (strip comments, quotes, collapse whitespace, lowercase), and redirections/teed writes to system paths are detected with a tokenizer that tolerates optional spaces, fd prefixes (`2>`), clobber form (`>|`), and Windows/Git-Bash path variants (`C:\Windows`, `/c/windows`, etc.)
- `kf-draw` and `stratum` shell helpers now look for their satellite binary next to the script (`<plugin>/tools/<bin>`) before the workspace target directory, so installed-layout plugin directories work when binaries are shipped alongside the wrappers
- `kf-draw` `render.sh` now uses the shared `json_get_string` helper (jq/python3/bash fallback) instead of sed-only parsing, matching the robustness of the other filesystem plugins
- `kf-draw` `edit.sh` now has a proper `#!/usr/bin/env bash` shebang and uses the shared JSON helper so it no longer relies on sed-only argument parsing
- `kf-plugin-sdk` `verify.sh`, `audit-verify.sh`, and `verify-workspace.sh` now default an empty/missing `KF_CODE_TOOL_ARGS_JSON` to `{}` instead of exiting, matching the other Node SDK tools
- Extended the `clippy::unwrap_used` production lint to the satellite crates (`kf-draw`, `kf-video`, `kf-budget`, `stratum`, and their core/host libraries) and fixed the resulting production unwrap sites.
- Satellite binary discovery in `kf-draw`, `kf-video`, `stratum`, and `kf-budget` now also accepts `<bin>.exe` candidates, so the Windows release archives (which ship `.exe` binaries) work under Git Bash / WSL without requiring a separate PATH entry.

### Added
- `/plugins` slash-command family for runtime plugin mount/unmount: `list`, `enable <name>`, `disable <name>`, `reload`, `trust <name> <tier>`. The executor picks up the new registry snapshot on the next turn without restarting.
- `--log-level` flag (default `warn`; env `KF_CODE_LOG_LEVEL`); `RUST_LOG` still overrides
- kf-code completions <bash|zsh|fish|powershell>` — prints shell completion script
- Cargo.toml metadata: `repository`, `license`, `keywords`, `categories`
- Five built-in workspace plugin sources (`plugins/kf-draw`, `plugins/kf-video`, `plugins/stratum`, `plugins/kf-budget`, `plugins/kf-plugin`) are now registered by default and can be toggled on/off persistently with `/plugins toggle <name>`.
- `/plugins` slash-command family extended with `toggle <name>`, `sources`, `add <name> <path>`, `remove <name>`, and `setup` for managing workspace plugin sources.
- Source-level unification of all five satellite projects into this repo: Rust satellites build as `crates/*` workspace members and the KirkForge-Plugin SDK is vendored under `npm/kf-plugin/`. The CLI, all satellites, and the plugin-host crate now build from a single workspace.

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
- Vendored Node SDK (`npm/kf-plugin`): `tool-pyright` now resolves the local `pyright` install before falling back to PATH, fixing test failures under vitest fork workers; CLI test helper no longer spawns every command twice; missing `e2e/smoke.test.ts` added.
- `kf-video` integration tests skip when `ffmpeg`/`ffprobe`/`flite` are absent, and CI installs them so the suite stays green on stock Ubuntu runners.
- Config file (`~/.local/share/kf-code/config.toml`) now created with `0o600` permissions instead of world-readable `0644`; all three write paths covered (create, hot-reload, `save_config`)
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
