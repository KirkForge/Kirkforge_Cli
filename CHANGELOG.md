# Changelog

All notable changes to kf-code are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Each entry links to its workorder for full details. WO files live in
`docs/workorders/` and are the single source of truth for what changed,
why, and the gate evidence.

## [Unreleased]
- WO 47.5 — bench + testdoctor gated behind default-off `devtools` feature; the release binary compiles neither. [47.5](docs/workorders/47.5-devtools-out-of-binary.md)

### Changed

- WO 47.4 — workspace folded 13 → 10 crates: kf-routing + kf-memory-store
  folded into kf-orchestrator (`routing`/`memory` modules; public
  orchestrator API unchanged), kf-plugin-sdk folded into kf-plugin-host
  (`sdk` module, surface re-exported at host root; src/ refs renamed
  `kf_plugin_sdk::` → `kf_plugin_host::`). No behavior change.
- WO 47.7 — MCP client dedup: `McpClient` is now a `Box<dyn McpTransport>`
  wrapper (stdio + HTTP impls of the transport primitives); every MCP
  operation and the initialize handshake exist once instead of 2-3×
  (−310 lines). HTTP `tools/call` now renders non-text content blocks as
  placeholders (image/audio/resource), matching the stdio transport.

### Fixed

- WO 48.9 — `--no-default-features` build (ADR-0017 minimal build) was
  broken: 3 un-gated `kf_budget_core::estimate_tokens` refs. The gate now
  lives in one choke point (`prompt::count_tokens`, bytes/4 fallback when
  `budget` is off); the TUI estimator routes through it. CI combo job
  deferred (see WO). [48.9](docs/workorders/48.9-no-default-features-broken.md)
- WO 48.2 — pre-tool hooks fired twice per file-tool call: the Phase-3
  recorder re-ran `pre-tool-{name}` after the write/edit had already hit
  disk. The single evaluation now happens in the Phase-1 pre-gate with the
  resolved path substituted into the hook args; a hook deny always lands
  before the mutation. Regression test: a counting hook sees exactly one
  invocation per file-tool call. [48.2](docs/workorders/48.2-pre-tool-hook-double-run.md)
- WO 48.3 — Sessions tab no longer permanently shows "No recent sessions"
  in the default (daemon-less) config: every tab-switch site (direct
  shortcut, command palette, F-key) now trips the cold-start refresh for
  Sessions exactly as it already did for Jobs (one shared
  `prime_overlay_cold_data` helper), so the tab lists from the on-disk
  session index via the WO 47.12 fallback.
  [48.3](docs/workorders/48.3-sessions-tab-empty-without-daemon.md)
- WO 48.4 — daemon `ThreadsChanged` pushes no longer open the full-screen
  "Resume a recent session" modal at every TUI startup (Enter could silently
  resume a different session). Data refresh (`recent_sessions`, feeds the
  Sessions tab + welcome screen) is split from modal presentation (now opened
  only by explicit `/resume`). Also fixed: the picker's advertised k/j/↑/↓ nav
  keys no longer drop the modal (the key handler took the picker and never
  restored it on non-commit keys).
  [48.4](docs/workorders/48.4-session-picker-modal-pops-at-startup.md)
- Known-flake stabilization — the four wall-clock test families that
  caused red/noise across the WO 47 campaign
  (`attached_cancel_token_kills_inflight_bash_promptly`,
  `same_ms_double_spawn_gets_distinct_{temp_dirs,worktrees}`,
  `edit_file_{fuzzy_preserves_unmatched_lines,roundtrip_reverses_replacement}`)
  got realistic budgets: per-test ci-fast slow-timeout overrides in
  `.config/nextest.toml` + raised in-test deadlines. Assertions unchanged
  (the bash-kill bound still sits below the 30s sleep a no-kill
  regression would run).
- WO 47.35 — prompt-injection defense: web_fetch/web_search/read_file now wrap
  untrusted bodies in `<untrusted_content>` delimiters + a system-prompt
  data-not-instructions rule; template `{{var}}` substitutions capped at 1024
  chars with control chars rejected.
- WO 47.28 — MCP HTTP transport sent `McpServerConfig.bearer_token` nowhere:
  it is now attached as `Authorization: Bearer <token>` on both the SSE GET
  and every POST (omitted when empty). `EventBus::emit` removed the in-flight
  buffer entry by `(sequence, kind)` — two streams sharing sequence+kind
  could remove each other's entry; removal now matches the full identity
  hash used for idempotency.
  [47.28](docs/workorders/47.28-mcp-bearer-token-and-event-identity.md)

### Changed

- WO 47.9 — archived the completed workorder corpus: series 6-45 (450
  files) moved to `docs/archive/workorders/` with the historical index;
  `docs/workorders/README.md` now indexes only the live 46/47 series.
  [47.9](docs/workorders/47.9-archive-workorder-corpus.md)
- WO 47.12 — session daemon is opt-in: `kf-code` no longer auto-spawns the
  background daemon; auto-start requires `KF_CODE_DAEMON_AUTOSTART=1`, and
  `--attach`/`--auto-resume`/startup-picker fall back to the on-disk session
  index when no daemon runs (explicit `kf-code daemon` unchanged).
  [47.12](docs/workorders/47.12-daemon-opt-in.md)
- WO 47.6 — compression layers deduped with zero behavior drift: the
  tool-result stub marker is single-sourced (`compaction::TOOL_RESULT_STUB`
  + shared `stub_tool_result()`), the anchor/region split is shared
  (`compaction::anchor_len()`), and the microcompaction/summarizer
  token-estimator delegate copies are deleted. The structural 6→2 fold
  (MiddleStrategy enum) is deferred — see the WO.
  [47.6](docs/workorders/47.6-compression-layers-6-to-2.md)
- WO 47.10 — new `emit!` macro replaces the 46-site `send_or_warn!`
  ceremony around async `TurnEvent` sends (2-arg form covers the 41
  identical messages; the 3-arg form preserves custom ones). No behavior
  change, net −100 lines. [47.10](docs/workorders/47.10-send-or-warn-ceremony.md)
- WO 47.11 — computer_use FROZEN, not deleted: the default-off feature
  already excludes the Chrome dep from default builds; a
  `ceiling: FROZEN (WO 47.11)` pin in Cargo.toml documents the unfreeze
  condition and delete precondition. [47.11](docs/workorders/47.11-computer-use-freeze.md)
- WO 47.13 — TUI command diet via gates, not cuts: `/gh` `/route`
  `/metrics` `/carryover`, the persona switches, the `/jobs` cron surface,
  and `@`-mention expansion are hidden behind `[display] extra_commands`
  (default off; gated commands answer with an enable hint).
  [47.13](docs/workorders/47.13-tui-command-diet.md)
- WO 47.14 — plugin verifiers are bus-only: `PluginVerifierAdapter` and
  the dual slots-side registration are deleted; `BusVerifier` is the
  designated surviving trait (first consumer migration of the
  unification; remaining consumers tracked in the WO).
  [47.14](docs/workorders/47.14-unify-verifier-traits.md)
- WO 47.27 — memory subsystem security/dupe fixes: memory-fact slugs strip
  URL/path/KEY=VALUE spans (secrets no longer reach filenames under
  `memory/`), fact bodies/descriptions are scrubbed via the audit-log
  secret shapes, nested keyword matches dedup to one fact per extractor,
  and sqlite tag queries escape LIKE wildcards.
  [47.27](docs/workorders/47.27-memory-slug-secrets-and-dupes.md)
- WO 47.26 — verifier panel perf: `VerifierHandler::verify_event` runs
  its independent verifiers concurrently (bounded, 4 at a time) instead
  of sequentially, with deterministic aggregate verdict preserved;
  `build_stream_preamble` reads its top-N stem files in one
  `spawn_blocking` batch instead of on the tokio worker; the verdict
  cache is bounded (256 entries, FIFO eviction) instead of unbounded.
  [47.26](docs/workorders/47.26-verifier-parallel-execution.md)
- WO 47.31 — ci-local.sh integrity: the tarpaulin coverage-threshold
  gate is routed through `run_step` (a below-threshold failure lands in
  `failures[]` and the summary runs, instead of a bare `exit 1`), and
  `run_step` disables errexit around `"$@"` so a compound command shape
  can't abort the script before `failures[]` records it (WO 46.25
  residual); 31 WO 46.x CHANGELOG entries lost in merge resolutions
  backfilled.
  [47.31](docs/workorders/47.31-ci-local-integrity-and-changelog.md)

- WO 46.1 — `FileAuditSink::flush` no longer advances the hash chain
  before the write succeeds; tamper-evidence is preserved on partial
  failure. [46.1](docs/workorders/46.1-audit-flush-partial-failure-breaks-tamper-evidence.md)
- WO 46.2 — `apply_patch_to_parent` sets up a process group so a
  grandchild doesn't survive the timeout.
  [46.2](docs/workorders/46.2-parallel-orchestrator-patch-missing-process-group.md)
- WO 46.3 — daemon concurrency semaphore no longer starved by
  long-lived instance push channels.
  [46.3](docs/workorders/46.3-daemon-semaphore-starved-by-instance-channels.md)
- WO 46.4 — kf-memory-store eviction enforces TTL and max_entries, not
  just count.
  [46.4](docs/workorders/46.4-memory-store-eviction-count-only-limits-nonfunctional.md)
- WO 46.5 — `minify_write_side` serde default aligned across config
  construction paths (was true on one path, `Default::false` on the
  other). [46.5](docs/workorders/46.5-config-minify-write-side-serde-default-divergence.md)
- WO 46.6 — EventBus inflight counter decrements when an emit future is
  dropped; `graceful_shutdown` no longer always times out.
  [46.6](docs/workorders/46.6-event-bus-inflight-leak-on-cancelled-emit.md)
- WO 46.7 — `WorkflowExecutor::run_fan_out` honors cancellation
  mid-fan-out. [46.7](docs/workorders/46.7-workflow-fan-out-no-cancellation-mid-batch.md)
- WO 46.8 — grep/glob `spawn_blocking` tasks are cancellable by the
  tool timeout (no more hung-subprocess leaks).
  [46.8](docs/workorders/46.8-grep-glob-spawn-blocking-not-cancellable.md)
- WO 46.9 — bench compare `--fail-on-regression` runs telemetry
  shutdown before `exit(1)`.
  [46.9](docs/workorders/46.9-bench-compare-bypasses-telemetry-shutdown.md)
- WO 46.10 — context-index `mtime_rebuild` picks up new files that are
  absent from the cache.
  [46.10](docs/workorders/46.10-context-index-cache-mtime-misses-new-files.md)
- WO 46.11 — ci-merge.yml validates bench TOML `[verify].type`
  (parity with ci-pr.yml).
  [46.11](docs/workorders/46.11-ci-merge-missing-bench-toml-validation.md)
- WO 46.12 — check-artifact-consistency.sh `grep -c || echo 0` no
  longer produces double output / a false failure under `set -e`.
  [46.12](docs/workorders/46.12-check-artifact-consistency-grep-double-output.md)
- WO 46.13 — `plugin_consent_ledger` defaults on, so the WO 45.61
  consent fix applies without opt-in.
  [46.13](docs/workorders/46.13-plugin-consent-ledger-default-off.md)
- WO 46.14 — `run_id`/`parent_run_id` threaded through workflow and
  scheduled jobs (WO 45.1 identity gap).
  [46.14](docs/workorders/46.14-run-id-threading-incomplete-workflow-jobs.md)
- WO 46.15 — kf-memory-store self-deadlock in the
  `write_run_and_emissions` fallback fixed.
  [46.15](docs/workorders/46.15-memory-store-self-deadlock-write-run-and-emissions.md)
- WO 46.16 — plugin hot-reload watcher is kept alive at the call site
  (was dropped — silently dead).
  [46.16](docs/workorders/46.16-plugin-hot-reload-watcher-dropped.md)
- WO 46.17 — process group set on 3 more subprocess spawn sites
  (children die with the parent).
  [46.17](docs/workorders/46.17-process-group-missing-three-more-spawn-sites.md)
- WO 46.18 — kf-orchestrator observation outcome stores the verdict,
  not the routing mode — the empirical router can actually learn.
  [46.18](docs/workorders/46.18-orchestrator-outcome-stores-mode-not-verdict.md)
- WO 46.19 — daemon state mutex no longer held across
  `write_response` `.await` (one wedged client can't stall the daemon).
  [46.19](docs/workorders/46.19-daemon-state-mutex-held-across-await.md)
- WO 46.20 — a never-ending bash job no longer blocks the scheduler
  and graceful shutdown.
  [46.20](docs/workorders/46.20-never-ending-job-blocks-scheduler-shutdown.md)
- WO 46.23 — `ResponseCache::put` no longer holds the sync Mutex
  across the disk write; the in-memory cache is bounded.
  [46.23](docs/workorders/46.23-response-cache-mutex-across-disk-write.md)
- WO 46.25 — ci-local.sh failing steps record into `failures[]` and
  the summary still runs (set -e no longer kills the script first).
  [46.25](docs/workorders/46.25-ci-local-set-e-defeats-gate-summary.md)
- WO 46.27 — context-index `mtime_rebuild`/`incremental_rebuild`
  preserve cached embeddings instead of silently dropping them.
  [46.27](docs/workorders/46.27-context-index-rebuild-drops-cached-embeddings.md)
- WO 46.29 — `start_daemon` reaps its child (no zombie per
  invocation). [46.29](docs/workorders/46.29-start-daemon-zombie-child.md)
- WO 46.31 — `apply_budget_slice` re-locks before slicing (TOCTOU
  between state read and slice fixed).
  [46.31](docs/workorders/46.31-apply-budget-slice-toctou.md)
- WO 46.32 — daemon socket perms + constant-time auth token compare —
  fixed via WO 47.16 (disclosed deferral; see its entry above).
  [46.32](docs/workorders/46.32-daemon-socket-no-perms-token-leak.md)
- WO 46.33 — openai_compat tolerates DONE-before-`finish_reason`
  ordering (no spurious Error at stream end).
  [46.33](docs/workorders/46.33-openai-compat-spurious-error-done-ordering.md)
- WO 46.35 — parallel tool batches leave no ghost streaming card;
  result entries pair correctly with their calls.
  [46.35](docs/workorders/46.35-parallel-tool-batches-ghost-streaming-card.md)
- WO 46.36 — bash_runner normal-exit path drains the pipe (no
  spurious failure, no grandchild holding the pipe open).
  [46.36](docs/workorders/46.36-bash-runner-normal-exit-drain-failure.md)
- WO 46.38 — `verify_task` `env_remove()`s the parent env before
  `cmd.env()` — the gate sees only the intended vars.
  [46.38](docs/workorders/46.38-verify-task-env-not-stripped.md)
- WO 46.39 — doc drift batch: CLI about, lib.rs path, testdoctor
  refs, test count, stale Cargo.lock, install.sh dead paths.
  [46.39](docs/workorders/46.39-doc-drift-batch-fixes.md)

### Fixed

- WO 47.20 — response cache key + async disk tier: `CacheKey` hashed
  only (model, messages, tools, response_format), so a request with
  different generation config (seed, max_tokens, extended_thinking,
  budget_tokens) replayed another request's cached response, and two
  providers serving the same model name shared entries. The
  `CachingAdapter` wrapper now captures the knobs at `set_*` call time
  and folds them plus a provider/endpoint scope (from Config) into the
  key. Separately, `cache.get()`'s sync `fs::metadata` + read +
  deserialize (up to 64 MiB) ran on the tokio worker inside async
  `stream()` (as did `put`'s `fs::write`); the disk tier now runs via
  `spawn_blocking`, memory tier stays sync.

- WO 47.29 — four adapter wire-format defects: Bedrock SigV4 now signs
  `content-length` (was omitted from the signed header set while reqwest
  sent it); both SSE parsers (Anthropic + OpenAI-compat) scan for `data: `
  line-anchored instead of as a raw substring, so an embedded `data: ` in
  a payload/non-data line is no longer misparsed as a frame; both
  `OpenAiCompatAdapter` ctors strip exactly one trailing `/v1`
  (`with_base_url_and_key` de-duped nothing, `new`'s `trim_end_matches`
  erased legitimate `/v1/v1` bases); Vertex endpoint path segments
  (project/region/model) are percent-encoded so ids containing `/`, `?`,
  `#` cannot reshape the URL.
  [47.29](docs/workorders/47.29-adapter-wire-format-fixes.md)
- WO 47.16 — jobd auth and socket hardening (the disclosed WO 46.32
  deferral): the jobs daemon's private `check_auth` did a raw
  constant-time compare on token bytes (length-leaking timing oracle)
  and bound its socket with umask-default perms (0o755 — any local
  user could connect and call Shutdown). The SHA-256-then-compare logic
  is now a shared `check_auth_ct` free fn used by both the session
  daemon and jobd, and jobd tightens its socket to 0o600 after bind
  (fail-closed), with a `jobd_socket_is_owner_only` regression test.
- WO 47.21 — `ensure_private_data_dir` no longer caches the FIRST data dir
  process-globally: the `OnceLock<()>` became a
  `OnceLock<Mutex<HashSet<PathBuf>>>`, so each distinct path is created +
  chmod 0o700 exactly once and a new `KF_CODE_DATA_DIR` (or test
  `DataDirGuard` override) actually gets its `tasks/`/`jobs/` subdirs
  instead of NotFound. A path is remembered only after successful creation,
  so a deleted tempdir is re-created on next call. The racing tests named
  in the WO migrated off process-global env mutation:
  `tools/task/persist.rs` + `tui/commands/tasks.rs` tests now use the
  thread-local `DataDirGuard`; `adapters/auth.rs` tests serialize their
  `*_API_KEY` env mutations on a module-local mutex.
  [47.21](docs/workorders/47.21-ensure-private-data-dir-oncelock.md)
- WO 46.30 — `bench run_task` no longer leaks `KF_CODE_BUDGET_CEILING`
  when it exits early: a private RAII `BudgetEnvGuard` replaces the
  success-path-only `remove_var`, so an error between env export and
  cleanup (conversation open, executor build) can no longer poison
  later tasks in the same `bench run` (the Token Budget Challenge runs
  5 tasks per invocation). Drop restores the pre-task value rather
  than blanket-removing, so a user-set global ceiling survives a bench
  run.

### Changed

- WO 46.34 — `InMemoryOffloadStore` (kf-budget-core) now evicts in true
  FIFO order. `evict_if_over_cap` claimed "remove the oldest entries" but
  took an arbitrary `HashMap` key slice, so a just-returned key could be
  evicted while old entries survived (a `get` for it then races to
  `NotFound`). The store now keeps a `VecDeque` insertion order alongside
  the map (mirroring the kf-compress-core store's WO 42.7 fix); re-put of
  a live key does not grow the order. Pinned by
  `evict_if_over_cap_is_fifo` and `duplicate_put_does_not_grow_order`.
- WO 46.37 — `web_fetch` and `web_search` now race every network await
  (DNS guards, HTTP execute, body streaming, Brave request) against the
  tool cancel token via `tokio::select!`, returning `ToolError::Cancelled`
  promptly on a cancelled turn instead of waiting out the 30s fetch
  timeout. Mirrors the WO 46.8 grep/glob cancel pattern.
- WO 46.28 — `prune_oldest_in_dir` now deletes the OLDEST `delete_count`
  sessions (the tail of the newest-first list) instead of the `delete_count`
  sessions immediately after the keep window. The prior slice
  `entries[keep..keep+delete_count]` left the absolute oldest sessions on
  disk, contradicting the documented "delete the oldest N, keep K most
  recent" contract. The "delete at most N" budget semantics (defaults
  N=5, K=10) are preserved; the workorder's proposed "delete everything
  beyond keep" was rejected as a data-loss surprise. Existing test
  `test_prune_oldest_deletes_oldest` corrected to match its own "oldest"
  wording; new regression test pins the workorder's exact scenario.
  [46.28](docs/workorders/46.28-prune-oldest-ignores-keep-semantics.md)
- WO 46.26 — `bench run-models` now exits non-zero when any model gets
  0/N tasks passed, mirroring the WO 38.10 guard on `bench run`. A
  total-failure run previously exited 0, blinding CI. Reports and the
  comparison are still written before bailing.
  [46.26](docs/workorders/46.26-handle-bench-run-models-missing-zero-guard.md)
- WO 46.21 — `web_fetch` now streams the HTTP response body via
  `response.bytes_stream()` with incremental `MAX_BODY_BYTES` (1 MiB)
  enforcement per chunk, instead of buffering the entire body with
  `response.bytes().await` and checking the cap afterward. A server
  streaming a multi-GB body within the 30s timeout no longer OOMs the
  process; the fetch aborts at the 1 MiB boundary.
  [46.21](docs/workorders/46.21-web-fetch-unbounded-body-read.md)

- WO 46.24 — 10 atomic-write sites migrated to the shared
  `tools::atomic_write::atomic_write` helper (O_EXCL + random tmp name +
  fsync + rename), closing the predictable-`.tmp` symlink-race TOCTOU.
  Sites: carryover save, config save, conversation checkpoint + replace,
  undo push + pop, session-index save, jobs store save + record_run,
  task persist. The two append-mode sites (audit log, tracing log) are
  a different fix shape (O_NOFOLLOW) and are deferred — see WO file.

### Changed

- WO 45.63 — pricing table no longer silently $0 for current Anthropic
  model families. Added rows for `claude-sonnet-5`, `claude-opus-4-8`,
  `claude-haiku-4-5`, and `claude-3-7-sonnet` (the last found by a new
  pricing↔thinking cross-DRIFT test). `claude-opus-4-8` previously
  wrongly inherited `claude-opus-4` pricing via partial prefix match;
  `claude-sonnet-5`/`claude-haiku-4-5` fell to the $0 sentinel. The
  unmapped-model warn now fires eagerly at session startup (was lazy —
  only after turn 1) and its message names the concrete consequence.
  [45.63](docs/workorders/45.63-pricing-table-stale-missing-current-models.md)
- WO 44.30 — `max_tool_result_chars` now applies to file tools.
  `truncate_tool_output` only handled `ToolOutcome::Success` and was only
  called on the non-file branch of `record_tool_result`, so
  `read_file(limit=500000)` injected megabytes into context regardless of
  the configured cap. Extended `truncate_tool_output` to handle
  `FileContent` (truncate content, force `truncated: true`) and
  `GrepMatches` (render + truncate), and wired it into the file-tool
  branch before the budget slice. [44.30](docs/workorders/44.30-file-tool-result-truncation-bypass.md)
- WO 44.45 — workflow engine error/budget semantics hardened: `eval_condition`
  is now bounded (kill_on_drop + 30s wall, 2s under test) and routed through
  the bash deny gate (no more `sleep infinity` wedge); `budget.on_exceeded`
  handler output reaches the model (returns `Ok` with `budget_exceeded` flag
  instead of bailing and dropping `WorkflowSummary`); `run_batch` joins ALL
  handles and preserves successful siblings on error (`BatchErrors` carries
  ok/err partitions) instead of early-returning and mislabeling every task.
- WO 44.29 — `/plugins` reload no longer silently drops the Node/Go/generic
  built-in verifiers. `BUILTIN_VERIFIERS` (the retain allowlist in
  `rebuild_plugin_verifiers`) was missing the 5 WO 32.20 names
  (node_test, node_lint, go_test, go_vet, generic_test), so the first live
  plugin reload permanently removed them from the correction loop for the rest
  of the session. Added the names + a guard test that fails the next time the
  list drifts from `init_default_verifiers`.
  [44.29](docs/workorders/44.29-plugin-reload-drops-node-go-verifiers.md)
- WO 44.31 — budget guard: `check_and_slice` now compares the result's
  token cost (via `count_tokens`, same estimator as `record_tool_usage`)
  against `budget.remaining()` (tokens), not `result.len()` (bytes) against
  `remaining()` (tokens). The byte-based `HeadTailSlicer` is fed byte
  budgets derived from `remaining() * 4`. Pre-fix the guard sliced English
  tool output ~4× too early (the model lost the middle of outputs far
  sooner than the configured ceiling required). [44.31](docs/workorders/44.31-budget-slice-byte-token-unit-mismatch.md)
- WO 43.20 — deps/binary-size: handlebars → in-tree stand-alone-tag-faithful
  renderer (fixes latent `{{!` comment leak in every system prompt), arboard
  slimmed, aws-sigv4 1.3.8, rustyline 16, `computer_use` feature gates
  headless_chrome (default builds lose local Chrome execution); lock graph
  572→549 packages. [43.20](docs/workorders/43.20-dep-size-audit.md)
- WO 43.22 — adapter transport: Bedrock `[DONE]` no longer launders mid-turn
  drops into success; `Retry-After` honored; wall-clock jitter; usage-less
  Done → estimated CostStats; connect_timeout 10s; vertex token cache.
  [43.22](docs/workorders/43.22-adapter-transport-robustness.md)
- WO 43.23 — subprocess lifecycle: PDEATHSIG kills children on parent
  abort/SIGKILL; background jobs cancelled on session exit; MCP reader idle
  timeout only while requests pending; kf-plugin-host tool/hook watchdogs.
  [43.23](docs/workorders/43.23-subprocess-lifecycle.md)
- WO 43.24 — test quality: named assertion-free tests now assert; 2
  can't-fail tests deleted. [43.24](docs/workorders/43.24-test-assertion-quality.md)
- WO 44.0-44.56 — WO 43 regression audit (36 WOs verified, 34 clean) +
  five-area fresh sweep → 25 planned workorders mapping the next phase.
  [44.0](docs/workorders/44.0-wo44-overview.md)

- WO 43.1: typed `AdapterError` (Unreachable/ModelNotFound/Denied/Other) for
  ollama stream errors — `KirkForgeError::from` downcasts before the
  string-probe fallback (fallback kept for unmigrated adapters). [43.1](docs/workorders/43.1-typed-adapter-errors.md)
- WO 43.0-43.17 — serialized the honest-assessment backlog into 17 verified
  workorders (6 analysis agents; ~11 stale claims corrected in-line).
- WO 43.18-43.24 — round-3 fresh segment audit: 7 workorders of NEW findings
  (concurrency/shutdown, TUI, deps/size, persistence, adapters, subprocess,
  test quality).
- WO 43.25-43.39 — round-4 full-coverage segment sweep: 15 workorders, 24 NEW
  findings (UTF-8 slice panics, prompt-compaction OOB, background-bash secret
  scrub, dead env-override layer, context-index retrieval smear, file-tool
  hook veto, unguarded subprocesses, crates bugs).
- WO 43.28 — applied `scrub_secrets_from_child_env` to background/scheduled
  (`BashJobRegistry::spawn`) and PTY (`pty::run_with_pty`) bash spawn paths;
  the foreground-only scrub leaked provider secrets to the model via
  `bash(background=true)` + `bash_status`. Added `pub(crate)` helper + pinning
  test.
- WO 43.38: spawn_blocking for glob walks, tokio::time::sleep for
  computer_use wait, glob-metacharacter redirection gate — [43.38](docs/workorders/43.38-async-blocking-glob-sleep-redirection.md)
- WO 43.18-43.24 — fresh segment audit round: abrupt-exit safety, TUI
  hardening, dep/size audit, persistence crash-robustness, adapter
  transport, subprocess lifecycle, test quality (7 analysis agents).
- docs: update stale WO statuses — 14 WOs marked Done (shipped but never updated): 14.6, 27.0-27.7, 28.6, 31.0, 32.3, 32.4, 33.0, 33.4, 33.9
- WO 43.4: property-based tests for `kf-routing` path-safety — proptest suite (traversal, absolute injection, no-panic, NFC/NFD, symlink fixtures) covering 5 branches that had zero tests — [43.4](docs/workorders/43.4-path-safety-proptest.md)
- WO 43.14: cargo-mutants nightly job for security-critical modules (path_safety, secret scrubbing, audit chain, sandbox) — informational baseline via `continue-on-error`; `scripts/run-mutants.sh` local wrapper; nightly-only per ADR-074 — [43.14](docs/workorders/43.14-mutation-testing.md)
### Performance
- WO 38.9/42.6 items 4-6: memory mtime cache, CachedIndex embeddings in query path, prompt stem stability — [38.9](docs/workorders/38.9-session-performance.md), [42.6](docs/workorders/42.6-performance-items.md)

### Fixed
- WO 46.22: MCP HTTP transport Drop arm now signals SSE reader shutdown — the `McpClient::Http(_)` Drop arm was empty, so on config-reload swap in-flight JSON-RPC calls waited the full `REQUEST_TIMEOUT` (60s) × N concurrent instead of failing immediately; now takes `shutdown_tx` and sends `()` (mirrors the Stdio arm), waking the reader which calls `fail_all_pending` — [46.22](docs/workorders/46.22-mcp-http-drop-empty-inflight-hangs.md)
- WO 43.25: char-boundary-safe truncation at 3 byte-slice panic sites (loop_.rs:72, stream.rs:111, events.rs:477) — non-ASCII Error text / file content / PTY output with a multibyte char straddling the cut point no longer panics the turn (two sites were outside catch_unwind) — [43.25](docs/workorders/43.25-utf8-byte-slice-panic-hardening.md)
- WO 43.26: guard workflow bash steps + plugin-bus verifier subprocesses — workflow `run_bash`/`run_batch` Bash arm now set `kill_on_drop` + 30s step timeout + cancel-token select; plugin-bus verifier timeout (WO 38.3 watchdog) pinned by a bus-wrapper test — [43.26](docs/workorders/43.26-unguarded-subprocess-workflow-pluginbus.md)
- WO 43.27: preserve target permissions on atomic write (Unix mode copy before rename) + fix notebook_edit undo ordering (push after write succeeds) — [43.27](docs/workorders/43.27-atomic-write-permissions-undo-order.md)
- WO 43.30: pre-tool hook veto now blocks file tools before mutation — file tools (read_file/write_file/edit_file) short-circuited to Spawn in pre_run before the hook block, so a hook `deny` fired after the write was already applied; now file tools run the same hook gate as non-file tools (path resolved first, then hook, then spawn) — [43.30](docs/workorders/43.30-file-tool-hook-veto.md)
- WO 43.31: doom-loop banner now highlights the user's actual arrow-key selection (was hardcoded to index 0 / "Break"); Enter now matches the highlighted action — [43.31](docs/workorders/43.31-doom-loop-banner-selection.md)
- WO 43.32: wire `apply_env_overrides` into production `load_config` — `KF_CODE_*` env vars were documented as layer-2 overrides but only applied under `#[cfg(test)]`; the shipped binary silently ignored them — [43.32](docs/workorders/43.32-config-env-override-layer-dead.md)
- WO 43.33: jobd `--stop` sends auth token, validates response, 30s timeout, conditional pid removal — `kf-code jobd --stop` was rejected by auth-enabled daemons, returned Ok on Error, hung on unresponsive daemons, and deleted a live daemon's pid file; now reads the token, bails on Error/Busy/timeout, and only removes the pid on confirmed stop — [43.33](docs/workorders/43.33-jobd-stop-auth-timeout.md)
- WO 43.34: `retrieve()` no longer smears unresolved import edges into all matched symbols — filter now `resolved_file == Some(sym.file)`, matching the already-fixed `to_retrieval_result`; fixes multi-MB system-prompt inflation (7MB / >1M tokens observed) — [43.34](docs/workorders/43.34-context-index-retrieve-smear.md)
- WO 43.36: kf-compress-core Lite mode now applies transforms (was a no-op identical to Off) — `Pipeline::run` gate short-circuited on `!offloads_bloat()` before the transform loop; restructured to independent gates — [43.36](docs/workorders/43.36-compress-core-lite-noop.md)
- WO 43.37: remove dead `pending_corrections` queue (write-only state; correction loop applies fixes directly) + fix MCP `stdio_send_request` `Ok(Err(_))` branch leaking pending-map entry on channel-close — [43.37](docs/workorders/43.37-verifier-corrections-queue-mcp-leak.md)
- WO 43.39: kf-bench markdown-delta uses summary rates, not recomputed union-set rates — [43.39](docs/workorders/43.39-bench-delta-rate.md)
- WO 43.16: no-throw dispatch hub — eliminated 3 remaining panic sites in dispatch-reachable code (Phase-2.5 deferred-file `expect` → guarded `Failure(Internal)`; `stratum_store` `expect` → `unwrap_or_else` + log + skip; `build_task_spawner` RwLock `unwrap` → poison pattern); added grep gate in ci-local.sh rejecting new non-test `unwrap`/`expect`/`panic!` in `dispatch.rs`; pinned catch_unwind contract with a panicking-tool test — [43.16](docs/workorders/43.16-no-throw-dispatch.md)
- WO 43.3: scrub secrets from audit log free-text fields (bash command, plugin args_summary, hook reason) — `scrub_free_text` strips `NAME=value` tokens matching credential shapes + token literals (Bearer, sk-, ghp_, AKIA, xox[bp]-) — [43.3](docs/workorders/43.3-audit-redaction.md)
- WO 42.11 / WO 42.6 item 2: wire content_hash into verifier path — verdict cache keyed by (file, content_hash); skip re-running cargo build/clippy/test for unchanged file content across correction iterations — [42.11](docs/workorders/42.11-content-hash-wiring.md)
- WO 42.5: MCP .mcp.json content-based re-approval — approvals now store a sha256 content hash; a modified `.mcp.json` under an approved path re-gates — [42.5](docs/workorders/42.5-mcp-content-approval.md)
- WO 42.7: offload store FIFO eviction + byte cap — replace random-order HashMap eviction with insertion-ordered VecDeque, add `max_bytes` cap — [42.7](docs/workorders/42.7-offload-fifo.md)
- WO 41.1: apply coder patch to parent + rename to PipelineOrchestrator — [41.1](docs/workorders/41.1-patch-application.md)
- WO 41.2: escape handoff delimiters in content to prevent fence spoofing — [41.2](docs/workorders/41.2-handoff-delimiter-spoof.md)
- WO 41.3: permission docs fix + ADR audit — accurate permission engine description, stale ADR amendments — [41.3](docs/workorders/41.3-permission-docs.md)
- WO 38.1: security chokepoints — [38.1](docs/workorders/38.1-security-chokepoints.md)
- WO 38.2: panic containment + terminal survival — [38.2](docs/workorders/38.2-panic-containment.md)
- WO 38.3: liveness — unblock TUI event loop, bound subprocesses — [38.3](docs/workorders/38.3-liveness.md)
- WO 38.4: orchestration correctness — real parallel cancel, collision-proof ids, fenced handoffs — [38.4](docs/workorders/38.4-orchestration-correctness.md)
- WO 38.5: adapter robustness — session survives turn errors, Anthropic usage capture, pricing, truncation — [38.5](docs/workorders/38.5-adapter-robustness.md)
- WO 38.6: crash-corruption recovery — heal torn logs, surface StartedEmpty, atomic config save — [38.6](docs/workorders/38.6-crash-recovery.md)
- WO 38.7: resource lifecycle — worktree drop fallback + boot sweep, MCP kill_on_drop, tmp cleanup — [38.7](docs/workorders/38.7-resource-lifecycle.md)
- WO 38.10: CLI first-run — banner to stderr, empty-model guard, `-p` flag, exit codes — [38.10](docs/workorders/38.10-cli-first-run.md)
- WO 38.11: TUI state hygiene — cache invalidation, bounded buffers, z-order fix — [38.11](docs/workorders/38.11-tui-state-hygiene.md)
- WO 38.12: test hygiene — kill wall-clock margins, gated-start cancel tests, un-ignore job-cancel — [38.12](docs/workorders/38.12-test-hygiene.md)
- WO 38.13: docs truth pass — fix overclaims, reconcile WO statuses, drift test, config example — [38.13](docs/workorders/38.13-docs-truth-pass.md)
- WO 39.1: bench suite repair — load fix, free-win verifies, export-tasks, script fix — [39.1](docs/workorders/39.1-bench-cross-tool.md)
- WO 40.2: Windows cross-compile gate in ci-local.sh — [40.2](docs/workorders/40.2-windows-cross-compile-gate.md)
- WO 40.4: sleep elimination — all gratuitous test-sync sleeps eliminated; 2 residual sleeps documented (mock behavior + unconvertible race) — [40.4](docs/workorders/40.4-sleep-elimination.md)
- WO 42.1: delete dead testdoctor test referencing deleted ci.yml — [42.1](docs/workorders/42.1-dead-testdoctor.md)
- WO 42.2: audit chain resumes on restart + `FileAuditSink::verify_chain` — [42.2](docs/workorders/42.2-audit-chain-verify.md)
- WO 43.29: guard OOB write in `minify_old_messages` on over-budget path — index-out-of-bounds panic when microcompaction collapsed the middle and the compacted form still exceeded budget — [43.29](docs/workorders/43.29-prompt-compaction-oob-panic.md)
- WO 43.35: memory store stale-lock recovery via PID-liveness check — crashed process's `.lock` file is reclaimed (dead PID removal + age fallback) instead of permanently latching the store — [43.35](docs/workorders/43.35-memory-store-stale-lock.md)


### Added
- WO 35.1: real scout→coder→reviewer pipeline — [35.1](docs/workorders/35.1-pipeline-semantics.md)
- WO 35.2: per-subagent worktree isolation + patch return — [35.2](docs/workorders/35.2-subagent-worktrees.md)
- WO 35.3: cooperative subagent cancellation — [35.3](docs/workorders/35.3-cooperative-cancellation.md)
- WO 35.4: sandbox posture indicator — [35.4](docs/workorders/35.4-sandbox-posture-indicator.md)
- WO 35.5: cross-component integration tests — [35.5](docs/workorders/35.5-cross-component-integration-tests.md)
- WO 35.6: ModelClient for kf-orchestrator — [35.6](docs/workorders/35.6-orchestrator-modelclient-wiring.md)
- WO 35.7: version-badge consistency gate — [35.7](docs/workorders/35.7-version-badge-gate.md)
- WO 36.1: rusqlite binary-size measurement — [36.1](docs/workorders/36.1-binary-size-rusqlite.md)
- WO 36.2: bash-job owner tracking + cancel-by-owner — [36.2](docs/workorders/36.2-bash-job-owner-tracking.md)
- WO 36.3: abort model streams on cancel — [36.3](docs/workorders/36.3-stream-abort.md)
- WO 36.4: live cancel token for parent session — [36.4](docs/workorders/36.4-parent-cancel-token.md)
- WO 36.5: production-wire ModelClient seam — [36.5](docs/workorders/36.5-modelclient-production-wiring.md)
- WO 36.6: EventSink → EventBus bridge — [36.6](docs/workorders/36.6-eventsink-bridge.md)
- WO 37.1: registry hardening — global task ids, bounded remove, no phantom jobs — [37.1](docs/workorders/37.1-registry-hardening.md)
- WO 37.2: the reducer — DelegationResult.packet is real — [37.2](docs/workorders/37.2-reducer.md)
- WO 38.8: wire budget guard into production — [38.8](docs/workorders/38.8-budget-guard-wiring.md)
- WO 38.9: long-session performance — kill O(N²) checkpoints, coalesce verifiers, cache token counts — [38.9](docs/workorders/38.9-session-performance.md)
- WO 39.2: Claude compat phase 1 — skill trigger derivation, commands loader, .mcp.json discovery — [39.2](docs/workorders/39.2-claude-compat-phase1.md)
- WO 39.3: Claude compat phase 2 — dynamic agents, tool-name aliases — [39.3](docs/workorders/39.3-claude-compat-phase2.md)
- WO 40.1: CI workflow architecture reset — [40.1](docs/workorders/40.1-ci-workflow-reset.md)
- WO 40.3: nextest profiles + timeout policy — [40.3](docs/workorders/40.3-nextest-profiles.md)
- WO 40.5: global state isolation — CwdGuard, env injection, data dir override — [40.5](docs/workorders/40.5-global-state-isolation.md)
- WO 42.3: secret scrubbing expanded — AWS creds, passwords, private keys, conn strings — [42.3](docs/workorders/42.3-secret-scrubbing.md)
- WO 42.4: rlimits/unshare fail-closed when `--harden` is set — [42.4](docs/workorders/42.4-rlimits-fail-closed.md)
- WO 32.15: landlock FS confinement integration test — verifies real bash job confined by landlock cannot read outside the sandbox — [32.15](docs/workorders/32.15-landlock-fs-confinement-test.md)



### Changed
- WO 40.4: test sleeps → structural sync (10 tests: 8 in commit 941377a3 + 2 in this commit) — [40.4](docs/workorders/40.4-sleep-elimination.md)

### Performance
- WO 42.12: populate `Message.token_count` at append time — estimators use cached value, eliminating redundant full-history BPE passes — [42.12](docs/workorders/42.12-token-count-cache.md)

## [0.3.10] - 2026-08-16

Release prep — version bump only. WO 33-34 series highlights:

### Fixed
- Windows stdin detach (P0 CI hang), kf-budget-core env-guard race.

### Changed
- TUI IA reset (WO 34.1-34.10): command palette, /help overlay, welcome screen, tab rewrites, action-first approval dialog, simplified status bar.
- CI architecture reset (ADR-074): split ci.yml into ci-pr/ci-merge/ci-nightly.
- Test optimization (WO 33.14/33.16): CommandRunner trait, EnvGuard, event-driven sync.
- Path-aware changed-package test selection (WO 33.6).

### Added
- GitHub Discussions enabled.
- WO 32.16-32.20: Windows stub tests, computer_use beta, security emitter, multi-language verifiers, parallel orchestration, self-update.












