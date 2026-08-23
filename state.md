# kf-code Repo State

*Current state only. History lives in `git log` and WO files.*

## Shipped (closed this session)

- **WO 43.12**: Done. Windows test parity finish — added `--doc` step to
  ci-merge.yml windows job (mirror of Linux :186); audited 205 `cfg(unix)`
  occurrences / 90 `#[cfg(unix)]`-gated test fns, ungated 8 platform-agnostic
  tests (TUI path-completion, symlink_swap_denied non-symlink cases,
  drain_capped Cursor tests). 82 stay Unix-only (Unix API, bash scripts,
  setrlimit, process groups, UnixStream, module-gated, subprocess-dependent).
  No PR-tier windows job (ADR-074 tier). Pushed to wo/wo43.12 (f7a0e035).
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

## Pending / Deferred (open)

- **WO 43.1 (deferred tail)**: migrate the remaining model adapters to return
  typed `AdapterError` from their `stream()` error paths so the string-probe
  fallback in `src/main/error.rs` can be deleted. Deferred because 43.1 scoped
  to ollama only (it owns "model not found"); remaining: openai_compat,
  anthropic, anthropic_bedrock, anthropic_vertex. Exact remaining work: add
  `.map_err(super::classify_transport_error)` to each adapter's
  `send_with_retry(...).await?` call, then remove the `contains()` block in
  `error.rs:49-73` once all producers are typed. Tracked in
  [43.1](docs/workorders/43.1-typed-adapter-errors.md).
- **WO 43.2, 43.5-43.17**: honest-assessment backlog (rounds 1-2), all Planned.
  6 analysis agents verified every claim; ~11 stale claims corrected in-line.
  Start at [43.0](docs/workorders/43.0-wo43-overview.md).
- **WO 43.18-43.24**: round-3 fresh segment audit (concurrency/shutdown,
  TUI, deps/size, persistence crash-robustness, adapter transport,
  subprocess lifecycle, test quality). NEW findings — top risks: line-mode
  Ctrl-C orphans bash children; audit BufWriter lost on panic-abort; no
  PDEATHSIG (parent abort orphans all subprocesses); Bedrock `[DONE]`
  injection bypasses truncation; headless_chrome ungated (~1-2 MB).
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
- **Bench spec task triage (WO 43.13, ADR-077)** — 4 implement-backlog, 12
  deferred, 6 dropped. The 12 deferrals and their concrete blockers:
  - **1 Find Dead Code / 2 Dep Graph / 3 Call Graph / 4 Explain Module / 5
    Cross-Repo Search** (A-category): blocked on `kf-context-index`
    exposing symbol-graph queries (unreferenced-symbol, crate-level
    dep-edge, per-symbol `call_graph()`, grounded module summarisation,
    workspace/trait-impl search). Remaining: add the query APIs to
    `kf-context-index`, then author the 5 task TOMLs. Tracked in
    [ADR-077](docs/adr/077-bench-spec-task-triage.md).
  - **9 Split Giant File / 32 Large Refactor**: blocked on fixture
    authorship (2500-line and 50+ file crate fixtures). Remaining: author
    the fixture crates + task TOMLs. Tracked in ADR-077.
  - **18 Add REST Endpoint**: blocked on non-Rust task setup (harness is
    Rust-only). Remaining: decide a non-Rust fixture path or a polyglot
    task loader. Tracked in ADR-077.
  - **27 Large Repository Navigation**: blocked on a Linux-scale
    context-index fixture (~30k files) — no such fixture exists and the
    index has not been load-tested at that scale. Remaining: build the
    fixture + load-test the index. Tracked in ADR-077.
  - **33 Merge Conflict Resolution**: blocked on a realistic
    mid-merge/conflict fixture + non-deterministic verify. Remaining:
    author the fixture + a deterministic verify block. Tracked in ADR-077.
  - **35 Regression Detection**: blocked on non-deterministic scoring (PR
    regression prediction is model-judgement, not `command_exits_zero`).
    Remaining: decide a deterministic proxy or accept model-judgement
    verify. Tracked in ADR-077.
  - **Fix Integration Test** (unmapped): blocked on a live
    integration-test fixture (Ollama + `qwen2.5:0.5b`) baked into the
    task — `verify-only` skips `requires_model` tasks. Remaining: author
    the fixture + wire a `requires_model` task. Tracked in ADR-077.
  - The 4 implement-backlog items (22 Build, 23 Formatter, 24 Lint,
    Implement Missing Trait) are NOT deferrals — they are future WOs
    (43.40+) with cheap deterministic verify, no blocker. Tracked in
    ADR-077 + the TECHNICAL.md "Planned tasks" Triage column.
  - The 6 dropped items (36 Token Efficiency, 37 Dollar Cost, 38 Time,
    39 Retry Count, 40 Human Intervention, Fix Compilation Error) are
    removed from the bench backlog — replaced by Budget Challenge
    telemetry (36/37), `TaskResult.duration` (38), ADR-0033 retry
    telemetry (39), the headless harness contract (40), and existing
    slot-16 tasks (Fix Compilation Error). Reversible only by a new ADR
    superseding ADR-077. Tracked in ADR-077.

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

- **CI: GREEN.** `cargo nextest run --profile ci-fast --workspace --lib --bins --locked`
  → 4579 passed, 0 failed, 16 skipped. `cargo clippy --all-targets -- -D warnings`
  clean. `cargo fmt --check` clean. `adr_xref_drift` 5/5 passed.
- **main == dev** at SHA `7b19dca6`.