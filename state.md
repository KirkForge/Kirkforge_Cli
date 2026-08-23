# kf-code Repo State

*Current state only. History lives in `git log` and WO files.*

## Shipped (closed this session)

- **WO 43.0**: Done (overview). Series 43 honest-assessment backlog
  serialization — all sub-items shipped except 43.20 (deferred) and
  43.22-43.24 (still planned, not implemented). Tracked in
  [43.0](docs/workorders/43.0-wo43-overview.md).
- **WO 43.1-43.19**: ALL Done (rounds 1-3). Typed `AdapterError` (43.1,
  ollama migrated); atomic-write EINTR/EAGAIN retry (43.2); audit-log
  redaction residual gap (43.3); path-safety proptest (43.4); panic-path
  elimination + no-op stub removal (43.5); `kf-rbac` wired into the daemon
  (43.6); placeholder ADR triage 0011/0012/0018 (43.7); silent
  error-handling triage (43.8); per-failure correction prompt guidance
  (43.9); cross-session state policy (43.10); Landlock/seccomp graduation
  decision (43.11); Windows test parity finish (43.12); 19 unimplemented
  spec task triage (43.13); machine-greppable ADR predicate blocks
  (43.15); content-hash consent binding for plugin trust (43.17);
  abrupt-exit safety — line-mode SIGINT, audit flush, grep blocking
  (43.18); TUI unicode-cursor fix + render-path test coverage (43.19).
- **WO 43.21**: Done. Persistence crash-robustness. AuditLog per-entry
  flush+fsync (survives SIGKILL/panic-abort — the audit trail is now the
  MOST durable store, not the least). FileAuditSink torn-tail tolerance
  (`impl Drop`→flush; `new` truncates unparseable final line + resumes
  chain from last intact hash, not genesis; `verify_chain` skips torn
  final line). Session log UTF-8 tolerance (`load_messages` reads via
  `read_until(b'\n')`+`from_utf8_lossy` — a mid-file invalid UTF-8 byte
  skips that line instead of failing the whole file). Atomic temp+rename
  writes for `task.rs` persist + `CachedIndex` save. `CachedIndex`
  `format_version` stamp (mismatch → Err → rebuild).
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
- **WO 43.20**: Cancelled (deferred). Dependency and binary-size audit.
  Deferred — the release profile (`opt-level = "z"` + `lto = true` +
  `codegen-units = 1`) already optimizes for binary size; a full dep audit
  remains desirable but is not blocking. Exact remaining work: enumerate
  every dep's binary-size contribution, drop/replace the heaviest that are
  not load-bearing, measure the delta. Tracked in
  [43.20](docs/workorders/43.20-dep-size-audit.md).
- **WO 43.22, 43.23, 43.24**: Still Planned (not implemented). Round-3 fresh
  segment audit findings that did not get a fix branch: adapter
  transport/streaming robustness residual (43.22 — Bedrock `[DONE]`
  injection bypasses truncation); subprocess lifecycle — parent-death
  orphans, MCP idle-kill, unguarded host-crate spawns (43.23); test
  assertion-quality triage — assert-free, tautological, stale ignores
  (43.24). Tracked in their WO files under `docs/workorders/`.
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
- WO drift test in `kf-budget-core/tests/adr_xref_drift.rs` — enforces WO file header ↔ README index agreement. Prevents future status drift. `wo_status_headers_match_readme_index` is one of its 6 checks.
- `.config/nextest.toml` profiles: `ci-fast` (30s, fail-fast), `ci-full` (60s), `nightly` (600s). CI references by name, no inline `--config`. Per-test override for `run_bash_stuck_step_times_out` (60s budget — the 30s workflow step timeout exceeds the 30s ci-fast slow-timeout).

## CI / branch state

- **CI: GREEN.** `cargo nextest run --profile ci-fast --workspace --lib --bins --locked`
  → 4644 passed, 0 failed, 16 skipped. `cargo clippy --all-targets -- -D warnings`
  clean. `cargo fmt --check` clean. `adr_xref_drift` 6/6 passed.
- **main == dev** at SHA `3093f0da`.