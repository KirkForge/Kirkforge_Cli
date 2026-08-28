# kf-code Repo State

*Current state only. History lives in `git log` and WO files.*

## Shipped (closed this session)

- **WO 48.10 (2026-08-28, branch `wo48.10`, not merged)**: Windows
  daemon-stub `try_list_recent`/`try_resolve_recent`/`try_resolve_id`
  (`src/daemon/client.rs` `windows_imp`) now serve the on-disk session
  index instead of `Ok(None)` — the same platform-neutral
  `session_index` code the Unix WO 47.12 disk fallback uses, so
  `--attach`, `--auto-resume`, the startup picker and `/fork` picker
  behave as the 47.12 help promises on Windows too. cli.rs unchanged
  (wording accurate post-fix). FAST GATES ONLY per owner directive:
  daemon::client tests 8/8, Windows cross clippy -D warnings green,
  workspace check green, fmt green. detect_changes: 0 indexed symbols
  affected (cfg(windows) bodies are outside the Linux symbol graph).
  [48.10](docs/workorders/48.10-windows-daemon-stub-no-fallback.md)

- **WO 48.2 (2026-08-28)**: pre-tool hooks no longer run twice per
  file-tool call. The Phase-3 recorder (`record_tool_result`) re-ran
  `pre-tool-{name}` AFTER the write/edit hit disk (doubled side-effects +
  a divergent verdict could deny recording a completed write — the WO
  43.30 contract violation surviving in the second-run window). The
  single evaluation now runs in the Phase-1 pre-gate
  (`pre_run.rs`) with the resolved path substituted into the hook args —
  identical JSON to what the deleted Phase-3 run passed, so hooks see the
  same view, now enforced pre-mutation. Regression test
  `pre_tool_hook_runs_exactly_once_per_file_tool_call` (counting hook
  asserts 1 invocation; 2 pre-fix). ADR-020 Phase-1 wording + TECHNICAL.md
  hooks bullet updated to pin the single-gate contract. FAST GATES ONLY
  per owner directive (check --workspace --all-targets, clippy -p kf-code,
  session::hooks + dispatch tests, fmt, adr_xref_drift).
  [48.2](docs/workorders/48.2-pre-tool-hook-double-run.md)
- **WO 48.3 (2026-08-28, branch `wo/wo48.3`, not merged)**: fixed the WO
  47.12 regression where the Sessions tab permanently showed "No recent
  sessions" in the default daemon-less config. The three tab-switch sites
  in `src/tui/keys/mod.rs` primed Jobs cold-start but never tripped
  `sessions_dirty`; now one shared `prime_overlay_cold_data` helper (used
  by `open_overlay`, palette Enter, and the F-key arm) trips both flags,
  and the draw loop's existing `refresh_sessions` handler populates the
  picker via the on-disk session-index fallback. Startup picker path was
  already correct. FAST GATES ONLY per owner directive (check/clippy/
  targeted tests/fmt green; full suite + Windows cross gate owed before
  any merge — see CI/branch state).
  [48.3](docs/workorders/48.3-sessions-tab-empty-without-daemon.md)
- **WO 48.4 (2026-08-28)**: daemon `ThreadsChanged` pushes no longer open
  the full-screen "Resume a recent session" modal at every TUI startup.
  Split the dual-purpose `session_picker` field into data-only
  `SessionState::recent_sessions` (written by background
  `refresh_sessions`; read by Sessions tab + welcome screen) vs the
  modal `session_picker` (set ONLY by explicit `/resume` with no args).
  Also fixed the picker dropping itself on advertised k/j/↑/↓ nav keys
  (`handle_session_picker_keys` now restores the picker while still
  open). Regression tests for both. Fast gates green (workspace check,
  clippy -p kf-code, fmt, targeted tests); full suites deferred to CI.
  [48.4](docs/workorders/48.4-session-picker-modal-pops-at-startup.md)
- **Known-flake stabilization (2026-08-28)**: the four wall-clock flake
  families that caused red/noise across the WO 47 campaign got realistic
  budgets — assertions unchanged, nothing weakened. (1)
  `attached_cancel_token_kills_inflight_bash_promptly`: in-test readiness
  deadline 10s→30s, bash-kill bound 10s→25s (still below the child's 30s
  sleep, so a no-kill regression still fails it), ci-fast override 60s.
  (2) `same_ms_double_spawn_gets_distinct_{temp_dirs,worktrees}`:
  readiness deadlines 10s→60s — the WO 47.21 residual (these tests do
  NOT route through `ensure_private_data_dir`; nextest process isolation
  + ~50 parallel test processes starved the old 10s poll), ci-fast
  override 90s. (3)
  `edit_file_{fuzzy_preserves_unmatched_lines,roundtrip_reverses_replacement}`:
  ci-fast override 60s (proptest ×32, fresh tokio runtime per case,
  25-35s solo — no in-test window exists to raise). All in
  `.config/nextest.toml` beside the `run_bash_stuck_step_times_out`
  precedent. Gates: each family targeted 2× green + combined ci-fast
  filter green, fmt clean, `cargo check -p kf-code --lib` clean.
  FAST GATES ONLY per owner directive — full suites not run this session.
- **WO 47 series COMPLETE** (37 workorders: 36 Done + 47.8 owner-skipped,
  see Pending). Final wave (47.6, 47.7, 47.9-47.14) merged at `c3a1eaf2`;
  earlier 47.x waves shipped through the campaign. One-liners in
  CHANGELOG; full detail in `docs/workorders/47.*`. Highlights —
  47.1 table-driven verifier registration; 47.2 generic env-override
  loader; 47.3 kf-rbac JWT/JWKS half deleted; 47.4 workspace 13→10
  crates; 47.5 bench/testdoctor feature-gated out of the default binary;
  47.6 compression dedup (structural fold deferred); 47.7 MCP transport
  trait (−310 lines); 47.9 corpus archived (450 files →
  docs/archive/workorders/); 47.10 `emit!` ceremony macro (−100 lines);
  47.11 computer_use FROZEN not deleted; 47.12 daemon opt-in;
  47.13 TUI command gates; 47.14 plugin verifiers bus-only; audit wave
  47.15-47.37 (security/robustness fixes, all Done).
- **Everything earlier is closed**: WO 46.1-46.39, 45.x, 44.x, 43.x and
  prior series complete (verified by the WO 44 regression audit + WO 47.9
  archival pass). History: `git log` + `docs/archive/workorders/`.

## Pending / Deferred (open)

- **WO 47.8 (owner-skipped)**: wire or delete the 9 fuzz targets — the
  only WO in the 47 series not Done (Status: Planned by owner choice).
  [47.8](docs/workorders/47.8-wire-or-delete-fuzz-targets.md)
- **WO 47.6 (deferred tail — structural 6→2)**: fold
  `maybe_microcompact` + `compact_to_budget` behind one `MiddleStrategy`
  enum (CollapseToSummary | StubPerSlot), wire `summarize_conversation`
  as its LLM arm at the `/compact` call site, collapse the 3-rung
  request-build fallback ladder into escalation params. Deferred because
  the executor call site (`loop_.rs`) + TUI `/compact` were outside the
  WO's file scope and the rungs are only outer-contract-pinned. Shipped
  half was the zero-drift dedup (shared `TOOL_RESULT_STUB` +
  `stub_tool_result()`, `anchor_len()`, estimator delegates deleted).
  [47.6](docs/workorders/47.6-compression-layers-6-to-2.md)
- **WO 47.12 (deferred tail)**: compile-time excision of `src/daemon/**`
  + kf-rbac from default builds via a default-off cargo feature (the
  runtime opt-in shipped instead: `KF_CODE_DAEMON_AUTOSTART`, on-disk
  session-index fallback). Remaining: dedicated WO with consumer cfg
  sites (6+ files) + CI feature matrix.
  [47.12](docs/workorders/47.12-daemon-opt-in.md)
- **WO 47.13 (gates-not-cuts disclosure)**: TUI commands were GATED
  behind `[display] extra_commands`, not deleted — shipped-line count
  unchanged; flipping the gates to deletions belongs to a future WO.
  Doom banner deliberately ungated (safety UI, WO 43.31); line-mode
  `/carryover show|clear` ungated (parity follow-up if wanted).
  [47.13](docs/workorders/47.13-tui-command-diet.md)
- **WO 47.14 (1-of-N consumers)**: plugin verifiers are bus-only
  (PluginVerifierAdapter + dual registration deleted); the remaining
  `Verifier`→`BusVerifier` migration (steps 1-5: handler, correction
  loop, 14 built-in verifiers + test suites, ~1.5-2K lines) is undone.
  `BusVerifier` is the designated surviving trait.
  [47.14](docs/workorders/47.14-unify-verifier-traits.md)
- **WO 47.4 cosmetic tail**: src/ imports SDK types via the
  `kf_plugin_host` root re-exports; an optional future sweep could import
  from `kf_plugin_host::sdk` explicitly for provenance. Zero behavior
  difference. [47.4](docs/workorders/47.4-fold-routing-memory-crates.md)
- **WO 46.24 (deferred tail)**: the two append-mode sites
  (`src/shared/audit.rs:143` `AuditLog::new`,
  `src/main/cli_dispatch.rs:73` `init_tracing`) open without `O_NOFOLLOW`
  — a pre-created symlink makes appends follow it. Remaining: add
  `O_NOFOLLOW` on Unix + decide the Windows path.
  [46.24](docs/workorders/46.24-predictable-tmp-filenames-toctou.md)
- **WO 44.44 item 4**: `run_bash_stuck_step_times_out` is
  `#[cfg(unix)]` — on Windows the test future deadlocks past its own
  inner timeout (msys sh + kill_on_drop orphan grandchildren; fix = Job
  Objects). Un-gate the test when 44.44 lands.
- **WO 43.26 DEFERRED**: `dispatch.rs:185` holds `Mutex<VerifierBus>`
  while calling sync `verify()` (up to 5s on a tokio worker). Fix
  requires a contract change (async `BusVerifier` or `spawn_blocking`
  per verifier inside `VerifierBus::run`); AGENTS.md §7 forbids the
  trait unification in one pass. No separate WO yet.
- **kf-lsp PDEATHSIG gap**: `crates/kf-lsp/src/lib.rs:1059` has its own
  `setup_process_group` duplicate without the PDEATHSIG call. Remaining:
  one prctl line or dedupe onto the session helper.
- **WO 43.1 (deferred tail)**: migrate openai_compat / anthropic /
  anthropic_bedrock / anthropic_vertex to typed `AdapterError` so the
  string-probe fallback in `src/main/error.rs` can be deleted.
  [43.1](docs/archive/workorders/43.1-typed-adapter-errors.md)
- **WO 43.20 (deferred tail)**: http 0.2/http-body 0.4 dedup NOT
  achievable — aws `sign-http` needs http 0.2 and the newer crate set
  needs rustc ≥1.91 (toolchain 1.88). Revisit at toolchain ≥1.94.
  Wayland clipboard path unverified (manual: Wayland session → TUI →
  select → Ctrl+Shift+C → `wl-paste`).
- **WO 43.22 (deferred tail)**: `estimated: bool` on
  TurnEvent::CostStats for the usage-less fallback; unit tests for the
  fallback + vertex token cache (executor harness / Authenticator
  injection needed).
- **WO 43.24 (deferred tail)**: kf-testdoctor assert-free-body heuristic
  — needs a source-scan pass in suggest.rs (~150+ lines).
- **WO 39.4** Claude-compat phase 3 (hook stdin-JSON contract);
  **WO 39.1** phase 3-4 (external `claude -p`/`codex exec`/`opencode run`
  runner + LiteLLM gateway); **WO 38.10** P2 CLI polish;
  **WO 19.11** plugin production hardening; **WO 21.0.14** deferred
  tracker (ledger, open by design); **WO 29.1** verify-tools fold
  (phase 1 shipped, verify tools deferred to 29.7). Trackers live in
  `docs/archive/workorders/`.

## Known flakes

**Stabilized 2026-08-28.** The four documented families
(`attached_cancel_token_kills_inflight_bash_promptly`,
`same_ms_double_spawn_gets_distinct_{temp_dirs,worktrees}`,
`edit_file_{fuzzy_preserves_unmatched_lines,roundtrip_reverses_replacement}`)
now carry realistic wall-clock budgets: raised in-test deadlines where a
deadline existed + per-test ci-fast overrides in `.config/nextest.toml`.
No assertion weakened — the bash-kill bound still sits below the 30s
sleep a no-kill regression would run. Watch item: the edit_file pair is
inherent CPU cost (proptest ×32, fresh runtime per case); if it flakes
again under sustained >2× load, the next step is sharing one tokio
runtime across proptest cases (WO 33.11 follow-up), not lowering
coverage.

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
- `.config/nextest.toml` profiles: `ci-fast` (30s, fail-fast), `ci-full` (60s), `nightly` (600s). CI references by name, no inline `--config`. Per-test overrides: `run_bash_stuck_step_times_out` (60s — 30s workflow-step timeout, WO 43.26) + the four known-flake families stabilized 2026-08-28 (cancel 60s, same_ms pair 90s, edit_file pair 60s — see "Known flakes").

## CI / branch state

- **main == dev == origin/main == origin/dev at `c3a1eaf2`** (2026-08-28).
- **CI GREEN**: GitHub run `33141778511` (CI (merge), branch main, head
  `c3a1eaf2`, 2026-08-28T04:25:39Z) — the WO 47 final-wave merge commit.
- Branch flow (NON-NEGOTIABLE, AGENTS.md task 7): push to dev → watch CI
  green (`gh run watch --exit-status`) → only then main. Never both at
  once; never main first.
- Worktrees: 3 (main checkout, dev-integration, user's external); remote
  branches just `dev` + `main`.
- This session (flake stabilization + docs truth pass) ran FAST GATES
  ONLY per owner directive: `cargo fmt --check`, `cargo check -p kf-code
  --lib`, each flake family 2× targeted, combined ci-fast filter run. A
  full gate run is owed before the next push.
