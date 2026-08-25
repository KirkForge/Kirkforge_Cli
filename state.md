# kf-code Repo State

*Current state only. History lives in `git log` and WO files.*

## Shipped (closed this session)

- **WO 43 SERIES COMPLETE** (all 43.x Done). Closed this session:
  - **WO 43.22**: adapter transport robustness — Bedrock forwards `[DONE]`
    only after terminal `message_stop` (mid-turn drop → Done{Error}, not
    laundered Done{Stop}); send_with_retry honors `Retry-After` (seconds);
    retry_backoff jitter wall-clock-seeded (no synchronized retry waves);
    turn.rs estimates tokens from the 42.12 cache when Done arrives
    usage-less (logged, not yet flagged — see pending); disclosure comments
    at SSE/openai_compat silent drops; connect_timeout(10s) on the model
    client; vertex caches OAuth tokens (vertex_auth returns AccessToken).
  - **WO 43.23**: subprocess lifecycle — `PR_SET_PDEATHSIG` in
    setup_process_group pre_exec (Linux) so abort/SIGKILL can't orphan
    children; `sweep_on_session_exit` cancels running bash jobs (TUI +
    line-mode, persists 43.10 exit summaries first); MCP reader idle
    timeout armed only while requests are pending (idle server stays
    connected; 300ms under cfg(test)); kf-plugin-host tool.rs/hook.rs got
    the verifier watchdog (killpg + deadline; hook fails open, pinned by
    module doc).
  - **WO 43.24**: test assertion quality — named weak tests now assert
    observable state (client roundtrip, hook marker, captured tracing,
    pending-untouched, glob determinism); 2 can't-fail noop tests deleted;
    stale kf-rbac ignore reason repointed at state.md pending.
  - **WO 43.20** (finished after interruption): handlebars replaced by a
    ~100-line stand-alone-tag-faithful mini renderer (golden-tested against
    real handlebars 6; fixed latent `{{!` comment leak poisoning every
    system prompt); arboard default-features off; aws stack refreshed to
    sigv4 1.3.8 (MSRV-pinned set); rustyline 16; `computer_use` now a real
    feature gating headless_chrome (default builds lose local Chrome
    execution — DEFERRED by design, see pending); lock graph 572 → 549
    packages; binary 17,212,536 → 17,200,248 B.
- **WO 44 series created** (25 workorders + 44.0 overview): full WO 43
  regression audit (36 WOs verified — 34 clean, 2 findings) + five-area
  fresh sweep (adapters/shared, session, tui/main/daemon, tools/crates,
  tests/CI). Start at [44.0](docs/workorders/44.0-wo44-overview.md).
- **README drift fix**: 17 stale WO 43 status rows synced to file-header
  truth (left red by the interrupted prior session).

- **WO 43.16**: Done. No-throw dispatch hub — eliminated 3 remaining panic
  sites in dispatch-reachable code (dispatch.rs:421 deferred-file `expect` →
  guarded `Failure(Internal)`; mod.rs:380 `stratum_store` `expect` →
  `unwrap_or_else` + `tracing::error!` + skip; mod.rs:494 RwLock `unwrap` →
  poison pattern `into_inner`). Added grep gate in `scripts/ci-local.sh`
  rejecting new non-test `unwrap`/`expect`/`panic!` in `dispatch.rs`. Pinned
  the catch_unwind contract with `test_panicking_tool_yields_failure_internal`.
  Refactor-on-touch policy for top-5 long functions added to AGENTS.md §7.
- **WO 43.25-43.39**: ALL Done (15 workorders, round-4 full-coverage segment
  sweep). UTF-8 byte-slice panic hardening (43.25); unguarded subprocess
  spawns (43.26); atomic-write permissions + undo ordering (43.27);
  background/PTY bash secret scrub (43.28); prompt-compaction OOB panic
  (43.29); file-tool hook veto (43.30); doom-loop banner selection (43.31);
  config env-override dead layer (43.32); jobd stop auth/timeout (43.33);
  context-index retrieval smear (43.34); memory-store stale-lock (43.35);
  compress-core Lite no-op (43.36); verifier dead-queue + MCP leak (43.37);
  async blocking + glob redirection (43.38); bench markdown-delta rate
  (43.39).
- **WO 43.1**: Done (ollama migrated to typed `AdapterError`; other adapters
  deferred — see pending). Typed `AdapterError`
  (Unreachable/ModelNotFound/Denied/Other) in `src/adapters/error.rs`;
  ollama's `stream()` wraps via `classify_transport_error`;
  `KirkForgeError::from` downcasts before string-probe fallback (fallback kept
  for unmigrated adapters).
- **WO 43.3**: Done. Audit-log redaction — `scrub_free_text` strips
  credential-shaped `NAME=value` tokens + token literals (Bearer, sk-, ghp_,
  AKIA, xox[bp]-) from bash command, plugin args, hook reason free-text
  fields. Shares `SECRET_ENV_SUFFIXES`/`SECRET_ENV_EXACT` consts with
  `bash_runner/mod.rs` (single source of truth).
- **WO 43.4**: Done. Property-based tests for `kf-routing` path-safety
  (proptest suite: traversal, absolute injection, no-panic, NFC/NFD, symlink
  fixtures) covering 5 branches that had zero tests.
- **WO 41.0-41.9**: ALL Done (series complete).
- **WO 42.0-42.12**: ALL Done (series complete). 42.0 overview closed.
- **WO 38.9**: items 1-6 all done (was 30%, now 100%). Closed.
- **WO 32.15**: Done (landlock FS confinement integration test).
- **WO 40.4**: Done (test sleeps eliminated; residual documented).
- **Stale statuses fixed**: 14.6, 27.0-27.7, 28.6, 31.0, 32.3, 32.4, 33.0, 33.4,
  33.9 — all marked Done in their WO files + README index (WO status drift test
  enforces the two-source-of-truth agreement).

- **WO 44 series COMPLETE** (25 workorders, 4 waves, all Done + merged):
  Wave 1: 44.28, 44.22, 44.36, 44.52, 44.31, 44.46. Wave 2: 44.20, 44.21,
  44.23, 44.24, 44.1, 44.29. Wave 3: 44.30, 44.2, 44.37, 44.38, 44.39,
  44.45. Wave 4: 44.47, 44.48, 44.44, 44.53, 44.54, 44.55, 44.56.
- **WO 45 series created** (20 audit workorders): external review verification
  + docs-honesty audit. See [45.0 overview TBD]. Key findings: unified
  execution identity gap confirmed (45.1), module sizes verified (45.16-45.20),
  config knob count refuted (140 not 400), context economics stack confirmed
  strong, Claude compat partially ships but undocumented (45.31).

## Pending / Deferred (open)

- **WO 45 series (20 items)**: architecture convergence + docs fixes from
  the external-review verification. Priority: 45.1 (AgentRun identity),
  45.16-45.20 (module splits), 45.31 (Claude compat docs), 45.51/45.52
  (stale docs fixes). Full list in docs/workorders/45.*.md.
- **WO 43.20 (deferred tail)**: http 0.2/http-body 0.4 dedup NOT achievable —
  aws `sign-http` itself needs http 0.2 and the newer crate set needs rustc
  ≥1.91 (toolchain is 1.88). Remaining: revisit at toolchain ≥1.94. Also
  base64 0.22 copy persists transitively (hyper-util + jsonwebtoken).
  Wayland clipboard path (arboard without image-data) unverified — manual
  check: Wayland session → TUI → select → Ctrl+Shift+C → `wl-paste`.
- **WO 43.22 (deferred tail)**: (a) `estimated: bool` on TurnEvent::CostStats
  for the usage-less fallback (interim: comment + tracing line); (b) unit
  tests for the usage fallback + vertex token cache (need executor harness /
  Authenticator injection — pre-existing `ponytail:` ceiling in vertex_auth).
- **kf-lsp PDEATHSIG gap**: `crates/kf-lsp/src/lib.rs:1059` has its own
  `setup_process_group` duplicate without the new PDEATHSIG call. Remaining:
  one prctl line or dedupe onto the session helper.
- **WO 43.24 (deferred tail)**: kf-testdoctor assert-free-body heuristic —
  needs a source-scan pass in suggest.rs (~150+ lines), not the cheap
  version hoped for.

- **WO 43.1 (deferred tail)**: migrate the remaining model adapters to return
  typed `AdapterError` from their `stream()` error paths so the string-probe
  fallback in `src/main/error.rs` can be deleted. Deferred because 43.1 scoped
  to ollama only (it owns "model not found"); remaining: openai_compat,
  anthropic, anthropic_bedrock, anthropic_vertex. Exact remaining work: add
  `.map_err(super::classify_transport_error)` to each adapter's
  `send_with_retry(...).await?` call, then remove the `contains()` block in
  `error.rs:49-73` once all producers are typed. Tracked in
  [43.1](docs/workorders/43.1-typed-adapter-errors.md).
- **WO 43.2, 43.5-43.19, 43.21**: honest-assessment backlog (rounds 1-4) —
  ALL Done (verified by the WO 44 regression audit; stale "Planned" claims
  corrected). Series closed.
- **WO 43.26 DEFERRED**: the bus-path blocking-on-async-worker concern
  (`dispatch.rs:185` holds `Mutex<VerifierBus>` while calling sync
  `verify()` → `PluginVerifier::run()` on the tokio worker, blocking it
  up to 5s) is real but out of scope for 43.26. Fixing it requires either
  an async `BusVerifier` trait (AGENTS.md §7 forbids the unification) or
  a `spawn_blocking` per verifier inside `VerifierBus::run` (changes the
  bus's sync contract + the `catch_unwind` resilience model). Remaining
  work: decide the contract change, then either make `BusVerifier::verify`
  async or wrap each verifier in `spawn_blocking` inside `VerifierBus::run`.
  Tracked here (pending) — no separate WO yet.
- **WO 39.4**: Claude compat phase 3 (hook stdin-JSON contract + generic
  pre/post-tool events). Deferred — lowest wild frequency of the artifact
  classes. Tracked in [39.4](docs/workorders/39.4-claude-compat-phase3.md).
- **WO 39.1 Phase 3-4**: external runner for `claude -p`/`codex exec`/`opencode
  run` + same-model LiteLLM gateway. Phase 1-2 done. Tracked in
  [39.1](docs/workorders/39.1-bench-cross-tool.md).
- **WO 38.10 P2s**: `--read-stdin-full` flag, JSON error-object emission,
  session.id in summary, replay/Ctrl-C/CLI polish. P0+P1 done. Tracked in
  [38.10](docs/workorders/38.10-cli-first-run.md).
- **WO 19.11**: plugin production hardening (partial). Tracked in
  [19.11](docs/workorders/19.11-plugin-production-hardening.md).
- **WO 21.0.14**: deferred tracker (tracking — this is the ledger of all
  deferrals, kept open by design). Tracked in
  [21.0.14](docs/workorders/21.0.14-deferred-tracker.md).
- **WO 29.1**: fold bundled plugin into compiled-in Rust tools — Phase 1
  shipped; verify tools deferred to 29.7. Tracked in
  [29.1](docs/workorders/29.1-fold-bundled-plugin.md).

## Known flakes (pre-existing, not introduced this session)

- `same_ms_double_spawn_gets_distinct_{temp_dirs,worktrees}` — real-concurrency git tests, `#[cfg(unix)]` gated, flake under extreme parallel load. Pass in isolation.

## Architecture notes (load-bearing, not in WOs)

- `VerifierHandler::verify_event` caches verdicts keyed by `(file_path, content_hash)`.
  Only `Clean`/`Skipped` verdicts are cached — `Fixable`/`Unfixable` are not (the
  correction loop re-verifies after applying a fix; disk content changed, so a
  cached verdict would be stale). After a fix is applied, `CorrectionLoop::run`
  calls `invalidate_cache(path)` to drop the stale entry. `content_hash == 0`
  events never hit the cache (old events / producers without hash). WO 42.11.

- `Message.token_count` is populated at append time (`ConversationLog::append`/`append_async`). Estimators (`estimate_message_tokens` in `prompt/mod.rs`) return the cached value when `Some`, falling back to BPE counting when `None`. Content mutation sites (`truncate_tool_results`, `dedup_adjacent_tool_results`, `minify_old_messages`, `stub_old_tool_results`, compaction stub/condense) clear `token_count = None` to avoid stale cache. WO 42.12.

- `panic = "abort"` in release — panic hook (WO 38.2) restores terminal before abort. Keep abort (binary size); don't switch to unwind without measuring.
- Budget guard wired in production (WO 38.8) — `set_budget_stores` + `set_stratum_store` called from `run_session.rs`. Listener registry is session-keyed `HashMap`, not the old append-only Vec.
- Windows cross-compile gate in `scripts/ci-local.sh` — `cargo clippy --target x86_64-pc-windows-gnu` runs before every push. AGENTS.md §4 enforces it. This is the structural fix for the 25+ `fix(windows)` commit pattern.
- WO drift test in `kf-budget-core/tests/adr_xref_drift.rs` — enforces WO file header ↔ README index agreement. Prevents future status drift. `wo_status_headers_match_readme_index` is one of its 5 checks.
- `.config/nextest.toml` profiles: `ci-fast` (30s, fail-fast), `ci-full` (60s), `nightly` (600s). CI references by name, no inline `--config`. Per-test override for `run_bash_stuck_step_times_out` (60s budget — the 30s workflow step timeout exceeds the 30s ci-fast slow-timeout).

## CI / branch state

- **CI: GREEN on origin/dev** — GitHub run `32675304923`, all five jobs
  (static, clippy, windows, e2e, full-tests). First fully green dev run
  since `32571059308` (2026-08-22); the 41-commit pile in between never
  had a valid green signal.
- **main == dev == origin/main == origin/dev** at `aec47bfe`. Flow:
  now on (user directive): push to dev → CI green → fast-forward main.
- **Windows CI debt cleared** (all inherited from the dead session, fixed
  this session): kf-budget-core README test count 923; complete_path +
  complete_mention drive-colon split (one shared `split_range_suffix`);
  kf-memory-store real Windows PID probe (OpenProcess/GetExitCodeProcess,
  windows-sys target-gated); kf-routing deny-path separator normalization;
  set_times write-handle in the age-fallback test.
- **DEFERRED to WO 44.44 item 4**: `run_bash_stuck_step_times_out` is
  `#[cfg(unix)]` — on Windows the whole test future deadlocks past its own
  inner timeout (msys sh + kill_on_drop orphan grandchildren; fix = Job
  Objects). Un-gate the test when 44.44 lands.
- **Cleanup**: 60 worktrees → 3 (main, dev-integration, user's external);
  all merged local + remote wo/* branches deleted (49 remote + 6 stale
  wo30* + locals); remote is just dev + main.
- Last full local gate on `aec47bfe`: test-full 4953 passed / clippy
  (unix + windows cross) / fmt / check --locked all clean.