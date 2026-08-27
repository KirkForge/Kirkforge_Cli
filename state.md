# kf-code Repo State

*Current state only. History lives in `git log` and WO files.*

## Shipped (closed this session)

- **WO 47.4**: Done. Lean-KirkForge crate fold, one commit series on
  `wo/wo47.4` (not merged/pushed — parallel-wave worktree). Workspace
  13 → 10 members: kf-routing folded into kf-orchestrator as the
  `routing` module (inner routing.rs renamed engine.rs — clippy
  module_inception, matches the routing-engine.ts port), kf-memory-store
  folded in as the `memory` module (its integration tests now live at
  crates/kf-orchestrator/tests/memory_store.rs; rusqlite/libc/windows-sys
  deps moved with identical target gating), kf-plugin-sdk folded into
  kf-plugin-host as the `sdk` module with the SDK surface re-exported at
  the host root. WO premise correction: kf-plugin-sdk DID have external
  consumers (~60 `kf_plugin_sdk::` refs in 14 src/ files); a cargo dep-
  rename alias was rejected by cargo (same package under two names), so
  the refs were mechanically renamed to `kf_plugin_host::` in the same
  commit. Public orchestrator surface unchanged (lib.rs re-exports
  repointed internally). Cross-layer: changed-packages.sh reverse-dep
  table now empty, run-mutants.sh + ci-nightly.yml mutants paths,
  TECHNICAL.md diagram/tables. Gate: workspace check clean, clippy -D
  warnings on the 3 touched crates clean, 330/330 orchestrator+host
  tests, adr_xref_drift green, fmt clean.
- **WO 47.27**: Done. Memory subsystem, three defects, one gated commit
  each on `wo/wo47.27` (not merged/pushed — parallel-wave worktree).
  (1) `make_slug` slugified raw user text, so 'I prefer
  ANTHROPIC_API_KEY=sk-abc123' put the key in a memory/ FILENAME and the
  literal secret in the persisted body. Slug input now strips URL-/
  path-/KEY=VALUE-shaped tokens (`strip_slug_hazards`); body/description
  run through the EXISTING `shared::audit::scrub_free_text` (pub, same
  lib crate — secret shapes single-sourced, zero shared-file edits) via
  a `new_fact` helper used by all three extractors. (2) mm-H21:
  extract_user_preferences/extract_corrections inserted one fact per
  matching keyword pattern; every match is a tail of the same message,
  so nested matches ("make sure to always use X") duplicated the same
  fact — now only the earliest (most complete) match is kept. Note: one
  fact per extractor per message is the resulting semantic; a message
  with two distinct prefs keeps the earliest tail, whose body still
  contains both statements. (3) kf-memory-store sqlite tag query
  interpolated % and _ as LIKE wildcards ('prod%' matched 'prod-build')
  — now `ESCAPE '\'` + escaping `\`/`%`/`_`, matched against the
  JSON-escaped form because the column stores the tags array as JSON.
  7 new tests across the three sites. Phase-B impact: extract_facts
  nominal HIGH (15 direct callers, 14 in-file tests; sole production
  caller `Executor::run_turn` memory hook; signature unchanged);
  sqlite query trait-internal LOW. detect_changes vs branch point:
  6 symbols / 2 files, risk low, 0 processes. Gate (head ba5c7ef7):
  test-fast 4814/4814 (16 skipped), clippy -D warnings clean, fmt
  clean, kf-memory-store 45/45, workspace check clean.
- **WO 47.20**: Done. Response cache two-defect fix
  (src/adapters/caching.rs only). (1) CacheKey hashed only (model,
  messages, tools, response_format) — seed/max_tokens/
  extended_thinking/budget_tokens/computer_use dims reached the inner
  adapter via set_* through the wrapper (which forwarded without
  capturing) and provider/endpoint routing was nowhere in the key, so
  different generation config replayed another request's cached
  response and two providers sharing a model name shared entries.
  CachingAdapter now captures the knobs at set_* time +
  set_request_scope() pinned by maybe_wrap_cached from Config;
  request_fingerprint() (scope+model+knobs, \0-separated) is the
  hashed key material. Wrap-vs-set ordering verified at all 4
  maybe_wrap_cached call sites (wrap first, executor pushes set_* on
  the wrapper). (2) cache.get() did fs::metadata+read+from_slice sync
  inside async stream() (64MiB cap) — disk tier extracted to
  read_disk/write_disk; new get_async/put_async run it via
  spawn_blocking (put's fs::write on the forwarder task was the same
  class — fixed too); memory tier stays sync. 4 new tests (knobs
  change fingerprint; different seed/scope don't replay; async disk
  round-trip); 4 existing skip-asserts re-keyed to the wrapper
  fingerprint (bare model name would have made them vacuous). Gate
  (head 31ad3188): clippy/fmt/check green; test-fast no-fail-fast
  4789/4792 — 3 documented load flakes (WO 46.28 bash-cancel + 2
  edit_file 30s-edge, load 20-39), all isolation-green; 38/38 caching
  tests green. Branch wo/wo47.20 NOT merged/pushed (per WO rules).
- **WO 47.26**: Done. Verifier-panel perf, three items, one commit each.
  (1) `verify_event` runs verifiers concurrently via futures-util
  `buffer_unordered(4)` — worst case drops from sum(14 verifiers × 30s cap)
  to ceil(14/4) × 30s; results re-sorted to registration order so the
  Unfixable>Fixable first-in-priority tie-break stays deterministic. Gotcha:
  building the futures through a stream `.map` closure (even calling a named
  async fn) trips rustc's higher-ranked Send-inference limitation — 5
  downstream spawn sites (tui, executor_adapter, task_spawner, persona) went
  red with "implementation of `Send` is not general enough"; the fix is a
  plain loop collecting futures into a Vec, then `stream::iter(vec)`.
  (2) `build_stream_preamble` is async; top-file reads+minify run as one
  `spawn_blocking` batch. (3) `verdict_cache` → bounded `VerdictCache`
  (map + insertion VecDeque, FIFO past 256, mirrors WO 46.34), resolves the
  `ponytail: unbounded` annotation; new test
  `verdict_cache_is_bounded_with_fifo_eviction`. Note: the `Verifier` trait
  is async (not sync `BusVerifier` as the WO assumed), so bounded futures
  — not spawn_blocking-per-verifier — is the right shape. dispatch.rs:185
  bus-under-mutex stays deferred (WO 43.26 tail). Files:
  verifier/handler.rs, verifier/mod.rs (stale truth-model doc fixed),
  verifier/tests.rs, executor/stream.rs, executor/turn.rs, TECHNICAL.md.
- **WO 47.29**: Done. Four adapter wire-format defects, one gated commit
  each on `wo/wo47.29` (not merged/pushed — parallel-wave worktree):
  (1) Bedrock SigV4 signs `content-length` — the header is added to the
  signable request before signing, so the signed and sent values are
  identical (mm-H13; the wiremock matcher hid this from real AWS).
  (2) SSE frame scan line-anchored in BOTH parsers (`anthropic/sse.rs`,
  `openai_compat/mod.rs`) via a local `find_data_frame_start` — an
  occurrence qualifies only at buffer start or right after `\n`/`\r`,
  so a `data: ` substring inside a non-data line/payload is not a frame
  (mm-H14; helper duplicated per file because `adapters/mod.rs` is a
  shared file outside this WO's scope). (3) Both
  `OpenAiCompatAdapter` ctors trim slashes then `strip_suffix` exactly
  one `/v1` — `with_base_url_and_key` de-duped nothing and `new`'s
  `trim_end_matches("/v1")` erased legitimate `/v1/v1` bases
  (mm-H15/H16). (4) Vertex `endpoint()` percent-encodes the
  project/region/model path segments with the existing `percent-encoding`
  dep (RFC 3986 pchar set; `:streamRawPredict` stays literal) — a model
  id with `/` or `?` no longer corrupts the URL (mm-H17). 7 new tests
  across the four sites; phase-B impact: parse_anthropic_stream HIGH
  (28 direct callers, ~25 are the test suite — all 211 anthropic +
  openai_compat tests green post-change), rest LOW. detect_changes
  (worktree-aware): 21 symbols / 4 files, all in scope, 0 processes.
- **WO 47.16**: Done. jobd auth timing oracle + world-connectable socket
  (the disclosed WO 46.32 deferral). Extracted the session daemon's
  SHA-256-then-ct_eq logic into `pub fn check_auth_ct(supplied, expected)`
  (src/daemon/mod.rs) — `DaemonState::check_auth` delegates (signature
  unchanged), and jobd's private `check_auth` now routes through it
  instead of raw `ct_eq` on token bytes (length-leaking). jobd's
  `UnixListener::bind` is followed by fail-closed
  `set_permissions(0o600)` (mirrors session-daemon server.rs; jobs
  module already cfg(unix)). New regression test
  `jobd_socket_is_owner_only` (jobs/daemon.rs tests). Files:
  src/daemon/mod.rs, src/jobs/daemon.rs. Gate: clippy/fmt/check green;
  test-fast red only on the documented load flakes — machine at load
  17.9-24.1/8 cores (parallel worktree agents): runs A+B failed only
  the WO 46.28 flake `attached_cancel_token_kills_inflight_bash_promptly`
  (isolation green, 7.95s); run C (--no-fail-fast, identical scope)
  4780/4782 passed with the flake GREEN and 2 edit_file 30s-timeout-edge
  tests timing out (both isolation green, 13.2s/16.5s). Zero anomalies
  touch daemon/jobs.
- **WO 46.30**: Done. `bench run_task` env-var leak on error paths —
  `KF_CODE_BUDGET_CEILING` was set at the top of `run_task` but only
  removed on the success path; any `?` between set and cleanup
  (`create_dir_all`, `ConversationLog::open`,
  `Executor::with_log_and_undo`) leaked the ceiling into every later
  task in the same `bench run` (the Token Budget Challenge runs 5).
  Fixed with a private RAII `BudgetEnvGuard` in bench.rs (set on
  construction, Drop restores the prior value or unsets) — the shared
  `test_util::EnvGuard` is `#[cfg(test)]`-only, production needed its
  own. Restore-prior also stops a bench run from clobbering a
  user-set global ceiling (old code blanket-removed). New test
  `budget_env_guard_unsets_on_drop_and_restores_prior_value` (throwaway
  var name — the real var is mutated by config tests under a
  module-local ENV_LOCK). One-file change (`src/session/bench.rs`).
  Gate: clippy/fmt/check green; test-fast could not go fully green on
  this box — 3 sibling worktrees ran ~250 rustc threads (load 17-27)
  and starved 30s-budget tests: run A failed only the documented WO
  46.28 flake (isolation green, 8.79s), run B (--no-fail-fast) 4763/
  4768 passed with 5 starvation anomalies — all 5 re-passed unstarved
  immediately (edit_file 54/54, flake, loop_), run C flake green and
  one different near-budget test (25.79s solo) timed out. Zero
  anomalies touch bench.rs.
- **WO 46.34**: Done. `InMemoryOffloadStore::evict_if_over_cap`
  (kf-budget-core) promised FIFO but took `guard.keys().take(excess)` on a
  `HashMap` — arbitrary order, could evict a just-returned key. Mirrored
  the kf-compress-core WO 42.7 fix: `Mutex<StoreData>` = map + insertion
  `VecDeque`; re-put of a live key doesn't grow the order; eviction
  pop_fronts until under cap. No new deps, no API change. 2 new tests
  (`evict_if_over_cap_is_fifo`, `duplicate_put_does_not_grow_order`);
  README Tests row 931 → 935 (2 new + 2 pre-existing drift caught by the
  readme_drift gate — the row was already 2 stale before this WO).
  Gate: kf-budget-core green, clippy/fmt/check clean. test-fast.sh ran
  red 3x on 4 known/borderline load flakes (machine load 25-32 from
  parallel worktree agents; `attached_cancel_token_kills_inflight_bash_promptly`
  = the WO 46.28-documented flake, plus 3 tests sitting at the 30s
  ci-fast slow-timeout edge) — all 4 proven passing in isolation;
  full no-fail-fast run of the identical scope: 4765/4769 passed, the
  same 4 flakes. Not caused by this change (kf-budget-core only).

- **WO 46.28**: Done. `prune_oldest_in_dir` deleted the wrong sessions:
  entries are sorted newest-first, and `entries[keep..keep+delete_count]`
  deleted the N sessions immediately *after* the keep window, leaving the
  absolute oldest on disk — contradicting the documented "delete the
  oldest N, keep K most recent" contract (4 doc sites agree). Fixed the
  slice to `&entries[len - delete_count..]` (the tail = the N oldest);
  the existing guard `len > keep + delete_count` guarantees the delete
  window never overlaps keep. Rejected the workorder's proposed
  `entries[..keep]` ("delete everything beyond keep") — that would make
  `/sessions prune` (defaults N=5, K=10) erase 85 of 100 sessions, a
  data-loss surprise. Corrected `test_prune_oldest_deletes_oldest`
  (its name said "oldest" but its assertion matched the bug); added
  `test_prune_oldest_in_dir_deletes_oldest_not_just_beyond_keep` with
  the workorder's exact params (keep=3, delete=2, len=6). One-file
  change (`src/session/session_index.rs`). Gate: 8/8 prune tests pass,
  clippy/fmt/check clean; one pre-existing concurrency flake
  (`attached_cancel_token_kills_inflight_bash_promptly`) failed in the
  parallel gate run under load-18-42 contention, passes in isolation
  (5.52s) — not caused by this change.

- **WO 46.25**: Done. `scripts/ci-local.sh` `run_step` returned non-zero
  on a failing step; with `set -euo pipefail` active, that killed the
  whole script on the first failure — remaining gates never ran and the
  `failures[]` summary was dead code. Removed the `return 1` on the
  failure path; the failure is still recorded in `failures[]`, the script
  continues, runs every gate, and the final summary reports all failures
  and exits non-zero. `failures[]` is now live. Scripts-only change, no
  Rust touched. Gate: `bash -n` + `scripts/test-fast.sh` (4763 passed) +
  `cargo fmt --check` + `adr_xref_drift` (6/6) all exit 0.

- **WO 46.24**: Done. TOCTOU symlink-race fix — 10 atomic-write sites
  migrated to the shared `tools::atomic_write::atomic_write` helper
  (O_EXCL + random tmp name + fsync + rename). Sites: carryover save,
  config save, conversation checkpoint + replace, undo push + pop,
  session-index save, jobs store save + record_run, task persist. No
  new helper, no new deps (reused the existing correct pattern). Tests
  updated where they asserted on the old fixed `.tmp` names. Two
  append-mode sites (audit log, tracing log) deferred — different fix
  shape (O_NOFOLLOW), see pending.
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

- **WO 47.4 cosmetic tail**: src/ imports SDK types via the
  `kf_plugin_host` root re-exports; an optional future sweep could import
  from `kf_plugin_host::sdk` explicitly for provenance. Zero behavior
  difference. Tracked in
  [47.4](docs/workorders/47.4-fold-routing-memory-crates.md).

- **WO 47.21 residual (follow-up needed)**: `same_ms_double_spawn_gets_distinct_temp_dirs`
  and `same_ms_double_spawn_gets_distinct_worktrees` do NOT route through
  `ensure_private_data_dir` (their temp dirs/worktrees come from
  `std::env::temp_dir()` directly, and nextest isolates each test in its own
  process). The OnceLock fix does not cover their residual flake: a 10s
  inner readiness deadline starved when ~50 parallel test processes
  oversubscribe the 8 cores (load avg 43 with sibling worktree agents'
  cargos). Observed passing solo + full-suite run 1 (4781/4781); failing
  under peak load. Remaining work: bump the readiness deadline or add a
  nextest per-test slow-timeout override (like
  `run_bash_stuck_step_times_out`). Tracked here; no WO filed yet.

- **WO 46.24 (deferred tail)**: the two append-mode sites use
  `OpenOptions::new().append(true).create(true).open()` without
  `O_NOFOLLOW`, so an attacker who pre-creates the target as a symlink
  makes appends follow the link (write INTO the target, no truncation).
  Sites: `src/shared/audit.rs:143` (`AuditLog::new`) and
  `src/main/cli_dispatch.rs:73` (`init_tracing`). The fix is
  `O_NOFOLLOW` on the open via `std::os::unix::fs::OpenOptionsExt` on
  Unix (Windows needs `FILE_FLAG_OPEN_REPARSE_POINT` or an explicit
  acceptance note). Deferred because: (a) it's a different fix shape
  from the tmp+rename migration the workorder scoped; (b) `O_NOFOLLOW`
  is Unix-only and both sites have Windows callers; (c) the threat model
  is "tamper with own audit/debug trail", not "clobber an arbitrary
  file" (append never truncates, and the attacker already needs write
  access to `~/.local/share/kf-code/`). Remaining work: add `O_NOFOLLOW`
  to both opens on Unix + decide the Windows path. Tracked in
  [46.24](docs/workorders/46.24-predictable-tmp-filenames-toctou.md).

- **WO 45 series COMPLETE** (27 workorders, 4 waves, all Done + merged):
  45.1 (AgentRun identity), 45.10 (typed event bus), 45.11 (sandbox seam),
  45.12 (MCP sandbox), 45.16-45.20 (5 module splits), 45.21 (shadow detection),
  45.31 (Claude compat), 45.32 (declarative agent docs), 45.36 (verifier
  outcome type), 45.37 (artifact policy enum), 45.41-45.43 (audit WOs),
  45.46-45.47 (invariant tests), 45.51-45.54 (docs + params-struct),
  45.59 (nightly subprocess tests), 45.61 (plugin signature hole),
  45.62 (Anthropic thinking detection), 45.63 (pricing table).
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
- **main == dev == origin/main == origin/dev** at `84825137`. Flow:
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
- Last full local gate on `aec47bfe`: test-full 5007 passed / clippy
  (unix + windows cross) / fmt / check --locked all clean.