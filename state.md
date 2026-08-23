# kf-code Repo State

*Current state only. History lives in `git log` and WO files.*

## Shipped (closed this session)

- **WO 43.26**: Done — workflow bash steps (`run_bash` + `run_batch` Bash arm)
  now spawn with `kill_on_drop` + 30s step timeout + cancel-token select;
  plugin-bus verifier timeout (WO 38.3 watchdog) pinned by a bus-wrapper
  test. Premise that bus.rs had "NO timeout" was stale — the 5s killpg
  watchdog lives in `kf-plugin-host/verifier.rs` (WO 38.3, in-branch);
  the real gap was the workflow.rs spawns + the lack of a pinning test.
- **WO 43.28**: Done. `scrub_secrets_from_child_env` (bash_runner/mod.rs)
  was foreground-only; background/scheduled (`BashJobRegistry::spawn`,
  covers jobs/runner.rs) and PTY (`pty::run_with_pty`) inherited the full
  parent env and could leak `*_API_KEY`/`*_TOKEN` to the model via
  `bash(background=true)` + `bash_status`. Helper made `pub(crate)`, applied
  on all three spawn paths; pinning test added. Helper + `is_secret_env_name`
  now `pub(crate)` so PTY (separate `portable_pty::CommandBuilder` type)
  reuses the same name-match logic.
- **WO 43.1 (partial)**: typed `AdapterError` (Unreachable/ModelNotFound/
  Denied/Other) added in `src/adapters/error.rs`; ollama's `stream()` wraps its
  `send_with_retry` error via `classify_transport_error`; `KirkForgeError::from`
  downcasts `AdapterError` before the string-probe fallback. String-probe
  fallback KEPT for unmigrated adapters (ponytail: comment). 4 new downcast
  tests green; all `hint_*`/`downcast_*` tests stay green.
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
- **WO 43.0-43.17**: honest-assessment backlog serialized as the Series 43
  workorders (all Planned, no code yet). 6 analysis agents verified every
  claim; ~11 backlog claims were stale and corrected in-line (notably:
  scout.rs:138 `unimplemented!()` is `#[cfg(test)]`-only, ADR 0011/0012
  already Rejected, landlock already default-on for bash, Windows rename
  retry already shipped). Start at [43.0](docs/workorders/43.0-wo43-overview.md).
- **WO 43.18-43.24**: round-3 fresh segment audit (concurrency/shutdown,
  TUI, deps/size, persistence crash-robustness, adapter transport,
  subprocess lifecycle, test quality). NEW findings — top risks: line-mode
  Ctrl-C orphans bash children; audit BufWriter lost on panic-abort; no
  PDEATHSIG (parent abort orphans all subprocesses); Bedrock `[DONE]`
  injection bypasses truncation; headless_chrome ungated (~1-2 MB).
- **WO 43.25-43.39**: round-4 full-coverage segment sweep (executor/turn,
  tools, session/mcp/prompt/verifier, TUI widgets/commands,
  sandbox/security, daemon/jobs/cli, `crates/*`). 7 fresh read-only agents,
  24 NEW verified findings (no re-scoping of existing 43.x items). Top
  risks: 3 raw UTF-8 byte-slice panics incl. two outside the tool
  catch_unwind (43.25); prompt-compaction OOB panic on over-budget path
  (43.29); background/PTY bash skip the secret env scrub (43.28 — DONE);
  `KF_CODE_*` env overrides dead in production (43.32); context-index
  `retrieve()` smears unresolved edges → multi-MB prompts (43.34); pre-tool
  hook deny ineffective for file tools (43.30); workflow-bash/plugin-bus
  subprocesses unguarded (43.26 — **43.26 Done this session**). See
  [43.0](docs/workorders/43.0-wo43-overview.md).
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
- `.config/nextest.toml` profiles: `ci-fast` (30s, fail-fast), `ci-full` (60s), `nightly` (600s). CI references by name, no inline `--config`.

## CI / branch state

- **CI: GREEN.** `cargo nextest run --profile ci-fast --workspace --lib --bins --locked`
  → 4518 passed, 0 failed, 16 skipped. `cargo clippy --all-targets -- -D warnings`
  clean. `cargo fmt --check` clean. `adr_xref_drift` 5/5 passed.
- **main == dev** at SHA `ff7d8132`.

