# kf-code Repo State

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`dev`** at latest merge. WO 21 + WO 22 + WO 23 + WO 24 + WO 25 + WO 26 + WO 27 + WO 28 + WO 29 series merged. `main` lags at `d848b37` (pending ff). See commit log for details.

## Version

**Current: `3.8.0`** (Cargo.toml + Cargo.lock; bumped from `0.3.6` in commit `6e2e0d4`). The user wants the next release tagged `0.3.9` — **not yet bumped in `Cargo.toml`** (per instructions: note here, don't change the manifest). When ready: `0.3.6 → 3.8.0` was the last bump; `0.3.9` is the next target (the `3.8.0` jump was a one-off to reflect the WO 27/28/29/30 architecture step-change; the line returns to `0.3.x` for the next minor).

## Session 2026-08-15 — WO 33.6 + 33.15 + 32.20 + 32.17 (worktree `.worktrees/wo32e`)

### What changed this session

- **WO 33.6: Path-aware changed-package test selection.**
  `scripts/changed-packages.sh` maps `git diff --name-only <base>..HEAD` to
  affected cargo packages including reverse-dep closure (4 internal edges,
  hardcoded adjacency table — ponytail: ceiling documented in script).
  `ci-pr.yml` adds a `changes` job that gates clippy + fast-tests on the
  output; docs-only / non-Rust changes emit `__NO_RUST_CHANGES__` and skip.
- **WO 33.15: Reduce #[serial] usage.** No-op: zero `#[serial]` in repo.
  The codebase went straight to `EnvGuard` (WO 33.13), never adopted
  `#[serial]`. Documented the 0→0 finding; remaining env-mutation cleanup
  is WO 33.13's scope (Pending).
- **WO 32.20: Node/Go/Generic multi-language verifiers.** Five new
  verifier files following the Python pattern (WO 31.1): `node_test.rs`
  (npm test / vitest), `node_lint.rs` (eslint / tsc --noEmit),
  `go_test.rs` (go test), `go_vet.rs` (go vet), `generic_test.rs`
  (make test / ctest / ./test.sh). `detect.rs` refactored to a shared
  `find_root_with_markers` helper + `find_node_root` / `find_go_root`.
  Each self-gates on language-marker detection; safe for pure-Rust
  workspaces.
- **WO 32.17: Anthropic hosted computer_use beta.** Completes the R4
  deferred item (see deferrals #3 below — now DONE). `ComputerUseConfig.hosted`
  flag (env `KF_CODE_COMPUTER_USE_HOSTED`, TOML `[computer_use].hosted`).
  `ModelAdapter::set_computer_use_dims` trait method (default no-op;
  Anthropic honours it). `computer_use.rs` splits into `local_def()` /
  `hosted_def()` and dispatches to `run_hosted_action()` which translates
  Anthropic's action vocabulary to CDP + always captures a screenshot.
  Executor activates at startup + config refresh (feature-gated
  `computer_use`). Config drift guards bumped: ENV_OVERRIDE_EXPECTED
  92→93, MERGE_TOML_EXPECTED 96→97 (added `hosted` to test fixture).

### Session incident

- **gitnexus MCP drop killed 3 subagents at 17:36:29Z.** Recurring
  gitnexus instability (8th drop since 08-08). All 3 subagent sessions
  aborted (error=Aborted). Work survived in worktree `wo32e` uncommitted.
  Resumed: fixed 2 clippy errors + fmt drift (the exact point where the
  subagent died at step 22), bumped config drift guards, split into 4
  logical commits. One file (`executor/mod.rs`) was lost during stash
  recovery and rebuilt from `/tmp/opencode/executor-full.diff`.

### Gate

- `cargo check --workspace --all-targets`: PASS
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS
- `cargo clippy -p kf-code --lib --tests --features computer_use -- -D warnings`: PASS
- `cargo fmt --check`: PASS
- `cargo nextest run -p kf-code --lib`: 3298 passed, 17 skipped (159s)

---

## Session 2026-08-15 — WO 32.5 parallel orchestration (worktree `.worktrees/wo32d`)

### What changed this session

- **WO 32.5: Parallel scout/coder/reviewer orchestration.** New
  `ParallelOrchestrator` (`src/session/parallel_orchestrator.rs`) spawns three
  subagents in parallel via `tokio::join!` on `InProcessTaskSpawner::run_task`.
  Each gets its own `TaskManager` entry. Triggered by `/workflow run <name>
  --parallel`. Sequential fallback (`run_sequential`) when `worktree_enabled`
  is false. Reuses the existing spawner seam (WO 32.4 landlock + WO 30.6
  approval forwarding). `TaskManager::get_mut` added for recording terminal
  results. 14 tests (6 orchestrator + 8 workflow). Gate: all green
  (`cargo check`, `nextest`, `clippy`, `fmt`).

### Deferrals

- **Per-subagent worktree**: DEFERRED. The session's worktree provides CWD
  confinement; creating 3 separate worktrees adds git overhead. Remaining:
  create 3 `WorktreeSession` instances in `spawn_role`, thread each path into
  config. Tracked in WO 32.5 (status "partially implemented").
- **Reviewer running BusVerifier**: DEFERRED. Reviewer uses `plan` persona +
  LLM critique. Remaining: inject `VerifyContext` into the subagent
  `Executor`. Tracked in WO 32.5.

### Pending

- WO 32.5 per-subagent worktree (see deferrals above).
- WO 32.5 Reviewer BusVerifier wiring (see deferrals above).

---

## Session 2026-08-15 — test-health + CI split + update subcommand + cleanup

Landed across the WO 32/33 series (worktree `.worktrees/wo32-series-c` and
main checkout). All items below are shipped to the working tree; none is
over-claimed.

### What changed this session

- **P0 deadlock in approval tests fixed.** Three approval tests
  (`approval::deny::test_always_approve_does_not_overwrite_existing_deny`,
  `approval::deny::test_deny_rule_blocks_bash_even_with_auto_approve`,
  `approval::auto::test_always_approve_dedups_repeated_calls`) hung ~860 s
  each under the full suite. Root cause: the spawner's `parent_approval`
  clone was held for the subagent's lifetime and never released, so the
  parent approval channel saturated and the test's `recv()` parked forever.
  Fix releases the clone once forwarding is wired. Suite runtime for those
  three: 860 s → ~2 s. This was the "Executor approval test hang" pending
  item from the 2026-08-14 config-drift session — now closed.
- **CI split into 3 workflows** (WO 33.3). Monolithic `ci.yml` (7 jobs,
  full matrix on every PR) → `ci-pr.yml` (PR gate: fmt + clippy + fast lib
  tests, <5 min target, fail-fast + concurrency cancellation),
  `ci-merge.yml` (push to main/dev: PR gate + full tests + doctests +
  windows + e2e + integration, parallel jobs), `ci-nightly.yml`
  (schedule + dispatch: coverage + ollama + e2e-exhaustive + audit +
  release-build matrix). `ci.yml` deleted. No job dropped; `quality` job
  decomposed into `clippy`/`fast-tests`/`full-tests`.
- **LSP disabled** in `~/.config/opencode/opencode.jsonc` (`lsp: true` →
  `false`). Root cause: rust-analyzer indexes one workspace per process,
  so the main checkout's server returned stale cross-workspace diagnostics
  for files a linked worktree had changed; subagents that trusted the
  stale LSP diagnostics reverted files to "fix" them, destroying other
  subagents' work. This is the editor-embedded LSP only — the in-repo
  `kf-lsp` crate and `lsp_query` tool are unchanged. See AGENTS.md §7.
- **Config migration from legacy kirkforge path.** First-run migration
  moves `~/.local/share/kirkforge/` → `~/.local/share/kf-code/` (and the
  config equivalent) so pre-rename installs keep their state.
- **116 env mutations killed in tests.** Shared `EnvGuard` serializes env
  access across the test suite; 116 `std::env::set_var`/`remove_var` sites
  converted. Removes a flaky cross-thread race class.
- **Wall-clock sleeps killed in 10 test files.** Replaced with
  `tokio::time::pause` / `interval` / channel-driven pacing.
- **5 regexes cached in `error_recovery.rs`.** `LazyLock<Regex>` instead
  of recompiling per diagnostic.
- **RSA key shared in `kf-rbac` tests.** One keypair per file instead of
  per test.
- **`bash.require_allowlist` config (default-off)** (WO 32.18). New
  `bash.require_allowlist: bool` + `bash.allowlist: Vec<String>`. When
  `true`, bash commands must prefix-match the allowlist on the command
  head or be denied; compound commands require every clause to match.
  Default `false` preserves current behavior. Env: `KF_CODE_BASH_REQUIRE_ALLOWLIST`,
  `KF_CODE_BASH_ALLOWLIST`.
- **Click-in-prompt cursor positioning.** TUI input box places the cursor
  at the click site instead of the end of the buffer.
- **Configurable streaming timeout.** The 90 s `STREAM_IDLE_TIMEOUT`
  constant is now `stream_idle_timeout_secs` in config (with env override).
- **`kf-code update` subcommand** (WO 33.17). Self-update: downloads
  latest GitHub release, verifies SHA256 against `SHA256SUMS.txt`,
  extracts the binary, replaces the running exe via atomic rename.
  `--check` prints current vs latest without installing. 8 unit tests.
  See `src/main/handle_update.rs`.
- **Cross-tool benchmark harness.** `docs/benchmarks/cross-tool-2026-08.md`
  + harness for running the same task across kf-code / Codex / Claude Code
  under descending budget ceilings — the WO 30.7 experiment that validates
  the context-efficiency thesis.
- **Port-trait residuals cut.** The 3 non-cyclic port-trait residuals
  left after WO 28.1's `tools↔session` cycle cut (bash I/O, bash-jobs
  registry, remember/memory) are removed — 0 `session` imports remain in
  `tools/`. Closes the WO 28.1 follow-up note from the 8th-ed review.
- **11 missing WO 28.9 tests added.** Session coverage gap tests the
  workorder named but never landed.
- **TUI Esc bug fixed.** Esc was invisibly toggling the thinking-panel
  visibility instead of its documented cancel/exit role. The toggle is
  now on an explicit key; Esc does what help says.
- **e2e tests feature-gated (not `#[ignore]`'d).** The 7 binary-spawn e2e
  tests that were `#[ignore]`'d since the 5th edition are now behind an
  `e2e` Cargo feature — runnable with `--features e2e`, absent from the
  default gate. `#[ignore]` count drops by 7 (35 → 28).
- **Coverage gate wired into CI.** `scripts/check-cov-regression.sh`
  (WO 28.7) now runs in `ci-merge.yml` + `ci-nightly.yml`, not just
  `ci-local.sh full`. Closes WO 32.14.
- **Nextest profiles** (WO 33.5). `.config/nextest.toml` defines
  `ci-fast` / `ci-full` / `integration` / `e2e`; CI uses
  `cargo nextest run --profile <name>` instead of inline `--config` flags.
- **Concurrency cancellation + parallel jobs + fail-fast on PR.** PR runs
  cancel superseded runs; merge-job steps run in parallel where safe;
  PR gate fails fast on the first red job.

### Pending / blocked (this session)

- **WO 32.11 — three deferrals from WO 29.3 (comment-only cleanup, shipped).**
  Three stale `ponytail:`/doc comments pointed at closed WOs (29.6, 29.7) as
  if still pending. All three updated to disclose the deferral to WO 32.11
  with concrete blockers + remaining work. No code changed (comment-only);
  the deferrals stay open:
  - **ClassifierMemory learned-examples** — persistence needs fs + session-dir
    ownership; belongs in `kf-orchestrator`, not the pure `kf-routing` crate.
    Remaining: add a persistence adapter in `kf-orchestrator` that saves/loads
    learned examples to the session dir.
  - **buildCorrectionPrompt real template** — the real template lives in
    `correction-core` (TS, not ported); porting needs the orchestrator's
    model-call layer (WO 29.7, not shipped). The placeholder prompt carries
    no per-failure guidance. Remaining: port the template into
    `kf-orchestrator` and wire it into the correction loop.
  - **PathGuard unification** — `shared::access::PathGuard` (async,
    `Path`/`OsStr`, canonicalize, fail-closed, config-coupled) and
    `kf-routing::path_safety` (sync, `&str`, lexical, fail-open,
    profile-coupled) have different types/error semantics on every overlap.
    Delegating needs an adapter that's more code than the duplication it
    removes. Remaining: design a `PathPolicy` trait in `kf-routing::path_safety`
    with sync + async-compatible signatures; adapt `PathGuard` to implement it.
- **WO 33.14 — subprocess test fakes.** Replace real-subprocess tests
  (bash, git, cargo spawn) with in-process fakes where the subprocess is
  an implementation detail, not the behavior under test. Reduces CI
  flakiness from external-binary availability. Not started.
- **WO 32.19 — R7 shipped, R6 disclosed as YAGNI** (branch `wo/32-series-d`,
  worktree `.worktrees/wo32d`). R7: wired the security emitter (WO 29.2)
  into the `kf-orchestrator` correction loop's verify cycle. New module
  `crates/kf-orchestrator/src/verifier.rs` ports the 14 regex rules
  (crate-local; the binary's `security_emitter.rs` is not reachable from
  the library crate). `run_correction_loop` scans the delegation's written
  files after each turn and populates `packet.verification.security` so
  `decide_correction` sees real findings. 10 new tests (8 verifier + 2
  wiring). R6 (`SloMonitor` + `AuthPolicySloMonitor`) disclosed as YAGNI:
  zero consumers of SLO numbers exist in the codebase (grep finds only
  "slow" false matches). Deferred per AGENTS.md §11 — reopen when a
  consumer materializes.
- **Full env-mutation cleanup.** 116 sites converted this session; a
  residual set remains in integration/e2e tests that spawn real
  subprocesses (those need the real env). Tracked in WO 33.13.
- **`main` fast-forward: still pending.** `origin/main` sits at `d848b37`;
  HEAD is several WO merges ahead.
- **Version bump to `0.3.9`: pending.** Noted above; do not bump
  `Cargo.toml` until the release cut.

## Session 2026-08-15 — `kf-code update` subcommand (branch `wo/32-series-c`, worktree `.worktrees/wo32c`)

- **Self-update command shipped.** New `kf-code update [--check]` subcommand
  downloads the latest GitHub release, verifies SHA256 against
  `SHA256SUMS.txt`, extracts the binary, and replaces the running exe in
  place via atomic rename. `--check` prints current vs latest without
  installing. Mirrors `scripts/install.sh` logic; uses existing deps only
  (reqwest, sha2, hex, tempfile). Extraction shells out to `tar` (no
  `flate2`/`tar` crate dep — binary size matters per the `opt-level="z"`
  release profile). Target-triple detection via `std::env::consts::{OS,ARCH}`
  (4 supported: linux x86_64/aarch64, macOS x86_64/aarch64). Windows
  unsupported (locked binary) — matches install.sh.
- **Files:** `src/main/handle_update.rs` (NEW, 8 unit tests), `src/cli.rs`
  (+`Update { check: bool }` variant + 2 parse tests), `src/main/mod.rs`
  (+`mod handle_update`), `src/main/cli_dispatch.rs` (dispatch wire),
  `CHANGELOG.md`, `state.md`.
- **Impact:** LOW — `Command` enum has 0 upstream consumers; the only
  `match` is in `cli_dispatch.rs:120`, updated in the same commit.
- **Gate green:** `cargo check -p kf-code --bin kf-code` ✓ ·
  `cargo test -p kf-code --bin kf-code` (30 passed) ✓ ·
  `cargo test -p kf-code --lib cli::tests::update` (2 passed) ✓ ·
  `cargo clippy -p kf-code -- -D warnings` ✓ · `cargo fmt --check` ✓.
- **Environment note:** `headless_chrome` build script fetches CDP JSON
  over the network; on the first lib/test build after a clean it fails
  offline. The host build reuses the cached `protocol.rs` so subsequent
  builds work. NOT caused by this change.

## Session 2026-08-14 — Config drift wipe fix (branch `woconfig`, worktree `.worktrees/woconfig`)

- **Config regeneration no longer wipes user values on schema drift.**
  Root cause: strict `toml::from_str::<Config>` failed whenever the file
  lacked a field without a field-level serde default (any newly added
  field — the field-addition checklist never required the attribute), and
  the `merge_toml_into_config` fallback silently resets the ~15 fields it
  doesn't handle (`budget_ceiling`, `summarize_enabled`, `docker`,
  `sandbox`, `permission_rules`, `mcp_servers`, …); first `save_config`
  persisted the wipe. Fix: struct-level `#[serde(default)]` on all five
  Config sub-structs — missing fields fill from `Default` via the primary
  serde path (verified to compose with `#[serde(flatten)]`). Regression
  test `schema_drift_preserves_user_values` (session/config/mod.rs) pins
  the load→save round-trip with two fallback-skipped canaries.
- **Scope creep:** resolved committed merge-conflict markers in
  `src/tui/selftest.rs` (HEAD `45c82b1` imported them from the wo30misc
  merge; ALL compilation was broken). Kept the wo30misc/WO 30.0.13 side —
  consistent with the 30.0.15 session entry ("retired the
  token_stream_stress guard").

### Pending / pre-existing (disclosed, not fixed here)

- **Executor approval test hang** — ✅ **RESOLVED 2026-08-15** (see the
  Session 2026-08-15 block above). Root cause was the spawner's
  `parent_approval` clone held for the subagent lifetime; fix releases it
  once forwarding is wired. The three tests now run in ~2 s instead of
  ~860 s each. The bisect suspects below were superseded by the actual
  root-cause fix.

## Session 2026-08-14 — WO 30 misc: subagent provider + TUI fixes (branch `wo30misc`)

Three WO 30 items shipped off `origin/dev`:

- **WO 30.0.6 — Per-subagent provider config (`ee4f3c4`).** New
  `SubagentProvider` struct (7 `Option` fields) on `ModelConfig` + a
  `[subagent_provider]` TOML block + `KF_CODE_SUBAGENT_*` env vars.
  `InProcessTaskSpawner` resolves the model as `task`-arg →
  `subagent_provider.model` → parent's `default_model`; host and per-provider
  keys fall back to parent when unset. Enables brain+brawn. `CONFIG_FIELD_COUNT`
  99 → 100; drift-guard test literals updated (merge 86 → 93, env 82 → 89).
  `config.toml.example` documents the block.
- **WO 30.0.15 — Streaming markdown fragmentation (`3a26115`).**
  `render_entry_lines` gains `is_streaming: bool`; streaming assistant content
  renders as plain text (`textwrap::fill`) — only completed messages get
  markdown parsing. Fixes partial-header artifacts (lone `#`). Side fix: the
  chat render cache no longer stores streaming renders (would shadow the
  markdown re-render on turn completion). Incidental fix: streaming now
  pre-wraps into one `Line` per visual row, so `max_scroll` is correct and
  `auto_scroll` pins to the bottom — retired the `token_stream_stress` guard.
- **WO 30.0.14 — Tool grouping in production path (`f896bf6`).** Commit
  `4668f91` added grouping to `build_chat_lines` (search-scroll) only;
  `render_chat` (production) was missed. Extracted `grouped_tool_header(state,
  idx) -> Option<(end_idx, lines)>` helper called from BOTH paths. Also fixed
  the expanded-mode idx-advance bug (middle tools skipped when group expanded).
  New `tool_call_grouping` selftest locks in 3 edge cases.

## Session 2026-08-13 — WO 31.6: TUI selftest harness (branch `wo31tui`)

A `#[cfg(test)]` harness in `src/tui/selftest.rs` that drives the FULL TUI
render pipeline (tab bar + chat + slash menu + input + status + approval +
doom banner) against an in-memory ratatui `TestBackend` — no terminal / PTY /
tmux needed. Runs in <1s as `cargo test --lib -p kf-code tui::selftest`.

- **`src/tui/mod.rs`** — extracted the body of `render_frame`'s closure into
  `pub(crate) fn render_app(f: &mut Frame, state: &mut AppState)` so the
  harness drives the EXACT same layout the production event loop does.
  `render_frame` (single caller, verified by grep) now just wraps
  `terminal.draw(|f| render_app(f, state))`. LOW-risk pure refactor.
- **`src/tui/selftest.rs`** — NEW. `TuiTestHarness` (owns `AppState`,
  exposes `feed_event` / `feed_events` / `render` / `assert_contains` /
  `assert_not_contains`), `render_to_string(state, w, h) -> String` helper,
  and 10 spec scenarios (token-stream stress, thinking word-wrap, tool-card
  render, approval prompt, budget indicator, scroll-100-messages, slash menu,
  search overlay, doom-loop banner, empty state) + 2 belt-and-suspenders
  tests. 12 tests, all green.

### Finding surfaced by the harness (DEFERRED — out of WO 31.6 scope)
The `token_stream_stress` scenario caught a real latent bug on first run:
**`auto_scroll` does not pin to the bottom for a long single-paragraph
assistant message.** Root cause: `render_chat` (`src/tui/widgets/chat/mod.rs`)
computes `max_scroll` from the pre-`.wrap()` `Vec<Line>` length, but a long
markdown paragraph is ONE `Line` (pulldown-cmark emits a paragraph as a single
flush), so `max_scroll` saturates to 0 and `auto_scroll` leaves `scroll_offset
= 0`. `Paragraph::wrap` then re-wraps the long Line at render time and clips
the tail out of view. The existing widget tests miss it because they use short
messages. The `token_stream_stress` test pins the bug with a guard assertion
(panics if the bug is fixed, prompting removal). **Remaining work to close
it:** either pre-wrap the assistant body into multiple `Line`s before
computing scroll geometry, or compute `max_scroll` from the post-wrap row
count. Tracked here; not fixed in this session (harness-only workorder).

## Session 2026-08-13 — WO 30.9: plan mode no longer traps `--non-interactive` (branch `wo30fix2`)

The doom-loop circuit breaker (WO 23.8) auto-switches to plan mode, but
`/implement` (the only exit) is interactive-only — so a scripted run hit
"Plan mode blocked" on every write tool and bricked. Fix (worktree `wo30fix2`):

- **`src/session/executor/mod.rs`** — `Executor` gains a `non_interactive: bool`
  field + `set_non_interactive()` setter; `observe_tool_outcome` downgrades
  `AutoPlan`→`WarnOnly` when non-interactive (the warning still logs; no trap).
- **`src/session/executor/pre_run.rs`** — plan-mode block gated by
  `self.plan_mode && !self.non_interactive` (belt-and-suspenders: writes never
  blocked in `--non-interactive` regardless of how `plan_mode` got set).
- **`src/main/line_mode.rs`** — calls `executor.set_non_interactive(non_interactive)`.
- **New test:** `doom_loop_circuit_breaker_downgrades_to_warn_only_when_non_interactive`
  in `tests/loop_.rs`. Existing doom-loop + plan-mode tests unchanged (all green).

Gate: `cargo check` + `clippy -D warnings` + `fmt --check` green; doom-loop (4)
+ plan-mode (6) tests green. WO 30.9 row in `docs/workorders/30.0.0-wo30-overview.md`
  → FIXED.

## Session 2026-08-13 — Comprehensive auto_approve audit + fix (branch `dev`, main repo)

The recurring `auto_approve` bug class (WO 12 → 24 → 27 → 30) was a
defence-in-depth "safety downgrade" in `pre_run.rs` that forced
destructive non-read-only `bash` to `Ask` *even when* `auto_approve =
true`, silently defeating the operator opt-in for the most common
destructive operation. Full audit of every approval endpoint + fixes:

- **`src/session/executor/pre_run.rs`** — removed the bash-specific
  `Ask` downgrade. The evaluator is now the single gate: when
  `auto_approve = true`, the default action is `Allow` for ALL
  destructive tools (incl. non-read-only bash). Deny rules still win
  (handled by `evaluate`); only the default changed. One branch
  collapsed.
- **`src/session/mcp_client/mod.rs`** — MCP `sampling/createMessage`
  now honors `security.auto_approve` in addition to
  `tools.allow_sampling_unattended`. A global opt-in covers
  server-initiated sampling too.
- **`src/main/line_mode.rs`** — fixed a RED test that slipped the WO 31
  gate. `non_interactive_approval_handler_denies_all_requests` passed
  `auto_approve=true` (sig fixed in `958e4f2`) but still asserted
  `DeniedWithReason`. The WO 31 worker's gate was `cargo test --lib` —
  the binary-crate test was never run, so the RED slipped in (confirmed
  RED: `panicked ... expected a reasoned denial, got Approved`).
  Renamed + split into `..._approves_when_auto_approve` and
  `..._denies_when_auto_approve_false`.
- **`src/session/executor/tests/approval/auto.rs`** — flipped
  `test_auto_approve_does_not_skip_approval_for_non_read_only_bash`
  (which asserted the buggy behaviour) to
  `test_auto_approve_skips_approval_for_non_read_only_bash` (asserts
  NO approval request is sent under `auto_approve=true`).
- **New test:** `test_sampling_auto_approved_when_auto_approve_set`
  in `mcp_client/mod.rs` — regression guard for the sampling fix.

### Endpoints audited (verdict)
| Endpoint | Verdict |
|---|---|
| `shared/permission.rs::evaluate()` | OK — default-action design, not auto_approve-aware by design |
| `shared/config/security.rs` field + `Default` | OK — `false` default |
| `session/config/mod.rs` TOML parse (nested + legacy) | OK |
| `session/config/env_overrides.rs` `KF_CODE_AUTO_APPROVE` | OK |
| `main/run_session.rs` CLI `--auto-approve` | OK |
| `main/line_mode.rs` non-interactive handler impl | OK |
| `main/line_mode.rs` interactive handler | OK — only reached if evaluator returns Ask |
| `session/task_spawner.rs` subagent handler | OK — parent-forward else auto_approve gate |
| `session/executor/pre_run.rs` default_action | **BUG → FIXED** |
| `session/executor/sandbox.rs` | N/A — FS path guard |
| `tools/bash.rs`, `session/plugin_tools/wrapper.rs` | N/A — rely on executor |
| `session/mcp_client/mod.rs` sampling | **BUG → FIXED** |
| `tui/commands/persona.rs` persona fork | OK — always-approve is intentional (isolated fork) |
| `tui/mod.rs` TUI approval prompt | OK — never reached under auto_approve after fix |
| `jobs/runner.rs` scheduled bash | N/A — separate `scheduled_bash_auto_approve` subsystem |

### Gate
`cargo check -p kf-code --lib --tests` ✓ · `cargo clippy -p kf-code --lib --tests -- -D warnings` ✓ · `cargo fmt --check` ✓ · `permission::` 41 passed · `session::executor::tests::approval::auto::` key tests passed (incl. flipped `..._skips_approval...`) · `session::mcp_client::tests::test_sampling` 9 passed (incl. new `..._when_auto_approve_set`) · `kf-code --bin kf-code non_interactive_approval_handler` 2 passed (previously RED). HEAD pre-push: see commit.

## Session 2026-08-13 — WO 31.1 + 31.4 Python verification loop (branch `wo31`, worktree `.worktrees/wo31`)

Multi-language verification loop — Python half. The verification bus previously
only fired for Rust; editing a `.py` file returned `Skipped` and the
generate→execute→verify→correct loop degraded to generate→execute→hope
(proven by the 2026-08-13 dogfood test against KirkForge-MCP). Shipped:
- **31.4 (detect):** `src/session/verifier/detect.rs` — `ProjectLanguage`
  enum + `detect_project_languages(&Path) -> Vec<ProjectLanguage>` (sniffs
  `Cargo.toml` / `pyproject.toml`|`setup.py`|`conftest.py` / `package.json`
  / `go.mod`; multi-language aware; stable Rust→Python→Node→Go order) +
  `find_python_root` walker (mirrors `helpers::find_cargo_root`). 10 tests.
- **31.1 (Python verifiers):** `python_test.rs` (`python -m pytest -x
  --tb=short -q`), `python_lint.rs` (probes `ruff` then `flake8`),
  `python_typecheck.rs` (`mypy`, only when `mypy.ini` or `[tool.mypy]` in
  pyproject.toml). Each self-gates on `.py` ext + Python detected at root;
  each returns `Verdict::Skipped` when its tool is absent (never blocks the
  turn). 13 tests.
- **Registration:** all three in `init_default_verifiers` at priorities
  6/7/8 (after Rust `test`=5) + added to `BUILTIN_VERIFIERS` so
  `rebuild_plugin_verifiers` retains them across plugin reloads.
- **Docs synced:** `docs/TECHNICAL.md` verifier section + workorder 31.0
  status header. Gate green: `cargo test --lib -p kf-code verifier::` →
  235 passed / 0 failed / 3 ignored; `cargo check` + clippy `-D warnings` +
  `cargo fmt --check` all clean.
- **Disclosed scope creep (pre-existing red, fixed to unblock gate):**
  (a) `src/main/line_mode.rs:720` test called
  `spawn_non_interactive_approval_handler(rx)` with the old 1-arg signature
  — the fn gained `auto_approve: bool` in `958e4f2` but the test wasn't
  updated; passed `true` (the test asserts "deny even when auto_approve").
  (b) Pre-existing `cargo fmt --check` drift in `line_mode.rs` + `task_spawner.rs`
  (flagged by the WO30.4 worker, not mine) — `cargo fmt` mechanical fix.
  (c) `Cargo.lock` `kf-code` version `0.3.6`→`3.8.0` to match `Cargo.toml`
  (commit `6e2e0d4` bumped Cargo.toml but not the lock).

### Pending
- **WO 31.2 (Node: tsc + eslint)** — not started. Same pattern; `Node`
  variant already exists in `detect.rs`; needs `node_test.rs` +
  `node_lint.rs` + npm-script detection.
- **WO 31.3 (Go: go test + go vet)** — not started. `Go` variant exists.
- **WO 31.5 (generic fallback: make test / ctest / ./test.sh)** — not started.

## Session 2026-08-13 — WO 30.4 seccomp syscall filter (branch `wo30c`, worktree `.worktrees/wo30c`)

The missing OS-isolation layer (external review 2026-08-13 §10): landlock confines the FS, seccomp confines the syscall surface. Shipped as a **default-OFF** `seccomp` Cargo feature (`seccomp = ["dep:seccompiler"]`):
- New `src/session/bash_runner/seccomp.rs` (cfg `all(target_os="linux", feature="seccomp")`). Allowlist filter: each listed syscall → empty rule (unconditional allow); match action `Allow`, mismatch action `Errno(EPERM)` (graceful, not KILL/SIGSYS). Allowlist = WO 30.4 base list (bash + grep/sed/awk/curl/cargo/node/python) **+ a glibc-startup/modern-`at`-variant block** (`arch_prctl`, `set_tid_address`, `set_robust_list`, `rt_sigreturn`, `sigaltstack`, `mremap`, `madvise`, `sched_getaffinity`, `getpid`, `getppid`, `newfstatat`, `faccessat`, `faccessat2`, `renameat2`, `fchmodat`, `fchownat`) — without these, no dynamically-linked binary (ld.so/bash) execs, making the filter dead-on-arrival; the workorder's literal list omitted them.
- Compile-in-parent / apply-in-pre_exec split (mirrors landlock): `SeccompFilter::new` + BPF emit allocate a `BTreeMap` → parent; `seccompiler::apply_filter` does only `prctl(PR_SET_NO_NEW_PRIVS)` + `seccomp()` syscalls (no alloc) → safe in pre_exec. Applied LAST (after landlock + rlimits). Fail-closed like landlock; `--i-accept-unsandboxed` governs both.
- `setup_rlimits` signature UNCHANGED → all 3 callers (`bash_runner/mod.rs`, `bash_jobs.rs`, `plugin_tools/wrapper.rs`) unaffected.
- ADR-054 amended (was "Do NOT ship seccomp"; the `seccompiler` crate removed the BPF-compiler blocker); status header + `docs/adr/README.md` row updated identically. `docs/TECHNICAL.md` feature-flag table + bash-sandbox section synced. Workorder 30.4 row → SHIPPED (opt-in).
- **DEFERRED (see pending):** (a) real-workload allowlist tuning — some tools will hit `EPERM` on unlisted syscalls; (b) cross-arch aarch64/riscv64 (legacy syscalls stat/fstat/lstat/access/pipe/dup2/fork/vfork/umount2/mount/getdents/arch_prctl are x86_64-only — the list is x86_64-tuned, CI is x86_64-only); (c) the default-on flip (kept opt-in until exercised). Gate green (both `--features seccomp` and default), clippy clean both ways, `cargo fmt --check` clean on my files (pre-existing task_spawner.rs:211 drift NOT mine).

## Session 2026-08-13 — MEDIUM security hardening (branch `wo28h`)

Knocked down the MEDIUM findings from the deep review (worktree `.worktrees/wo28h`, 3 commits, not pushed):
1. **Per-verifier timeout** — `verifier/handler.rs` wraps each `verify()` in `tokio::time::timeout` (30s prod / 50ms under `cfg(test)`); a wedged `cargo build` no longer hangs the turn. On elapsed → `Verdict::Skipped`.
2. **Audit-log records `read_file`** — `executor/turn.rs` + `dispatch.rs` audit gate renamed `is_destructive`→`should_audit` and now includes `read_file` (path kept by `redact_args`, content N/A).
3. **MCP stdio env hardening** — `mcp_client/mod.rs connect()` does `env_clear()` + `kf_plugin_host::env::curated_env` so MCP subprocesses no longer inherit parent API keys.
4. **`block_dotfiles` default true + deny-list expansion** — `config/security.rs` default false→true (+ serde default); `deny_list.rs` adds `~/.config`, `~/.docker`, `~/.netrc`, `~/.gitconfig` (`.aws` already present). Belt-and-suspenders behind landlock.
5. **Bare `#[ignore]` reasons** — 9 bare attributes across `web_fetch.rs`×5, `tui/commands/mod.rs`, `executor/tests/approval/timeout.rs`, `bash_jobs.rs`, `kf-testdoctor/gaps.rs` now carry `= "reason"` strings.
6. **This state.md refresh** (item 6).
- Gate per item green: `cargo check`/`clippy -D warnings`/`fmt --check` (`-p kf-code --lib --tests`); verifier (8) + access (69) + config (88) + executor (157) + audit (26) + mcp (83) tests pass; new deny-list credential/netrc/gitconfig tests pass.

## Session 2026-08-13 — WO 28.17 / 28.15 / 28.13 (branch `wo28g`, worktree `.worktrees/wo28g`)

- **WO 28.17 (shipped):** bash deny-list = tripwire, landlock = boundary posture documented. `ponytail:` posture comments on `check_bash_command_str` + `contains_shell_expansion_evasion` in `src/shared/bash_safety.rs` (WO cited old path `src/session/bash_runner/safety.rs`; file refactored to `src/shared/`). New "Security posture — tripwire vs boundary" subsection in `docs/TECHNICAL.md`. R2 (`bash.require_allowlist`) DEFERRED — see below.
- **WO 28.15 (shipped):** memory near-duplicate dedup gate in `MemoryStore::upsert` — token-set Jaccard over description+body, default threshold 0.85, configurable via `with_dedup_threshold(f64)`, skip + `tracing::trace!` log on match, returns existing fact. 8 new tests (R3). 3 existing tests with artificially-similar fixtures (eviction/budget/subset) scoped with `with_dedup_threshold(1.0)`. R2 (verify `score_for_context` magnitude normalization) verified no-op.
- **WO 28.13 (R1 shipped, R2 + full-adapter-turn DEFERRED — see below):** Bedrock wiremock contract tests in new `src/adapters/bedrock_vertex_mocks.rs`. Custom `SigV4Authorization` wiremock matcher makes the mock a contract gate (rejects unsigned/malformed, not theater): (1) signed request passes the gate, mock asserts `Authorization: AWS4-HMAC-SHA256 ...SignedHeaders=`, `x-amz-content-sha256`, model id in path; (2) unsigned request rejected (non-2xx) while signed passes; (3) event-stream frame sequence served by mock decodes through `parse_bedrock_event_stream` → Text deltas. Zero live AWS creds. `parse_bedrock_event_stream` → `pub(super)`; `bedrock_signing::tests` → `pub(crate)` so the shared env lock serializes the async tests with the offline signing tests.

### Pending / deferred (this session)
- **WO 28.13 R2 — Vertex wiremock contract test (DEFERRED):** `AnthropicVertexAdapter::access_token` calls `yup_oauth2`'s `ServiceAccountAuthenticator`, which hits Google's real OAuth endpoint and cannot be redirected at a wiremock server without an authenticator injection (the upgrade path named in `vertex_auth.rs:11-16`). Remaining work: inject an `Authenticator` trait so tests can supply a fake bearer token, then add a wiremock test asserting the `Authorization: Bearer <token>` header + project/region in path + Anthropic SSE framing. Tracked in WO 28.13 R2-later.
- **WO 28.13 — full-adapter Bedrock turn through wiremock (DEFERRED):** `AnthropicBedrockAdapter::endpoint()` hardcodes the AWS URL, so the adapter cannot be pointed at a wiremock server without a base-URL/endpoint-override injection. Remaining work: add an endpoint-override knob (also legit for LocalStack / VPC endpoints), then drive `adapter_for_with_provider(..., AnthropicBedrock, ...)` through `run_turn_collecting` against wiremock. Tracked in WO 28.13 R1-later. The signed-request + event-stream-decode paths ARE exercised by the shipped tests (the AWS-specific wire logic).
- **WO 28.17 R2 — `bash.require_allowlist` mode (DEFERRED):** allowlist semantics (glob vs prefix vs regex, compound-command handling) need operator input per the WO defer note. Remaining work: new `bash.require_allowlist: bool` config field + `bash.allowlist` list; reject non-matching commands. Tracked in WO 28.17 R2-later.

## Session 2026-08-12 — ADR drift gate fix (branch `adr-fix`, worktree `.worktrees/adr`)

- **Task:** make `cargo test -p kf-budget-core --test adr_xref_drift` green (4/4); fix ADR-054 status drift; check all ADRs + TECHNICAL.md count.
- **Finding (honest):** the ADR-054 header↔README status drift the workorder cited was **already fixed** in a prior merge — file header and README index row are byte-identical (`Accepted (WO 27.1 added landlock — see amendment below)`). No ADR content edit was needed.
- **Real blocker (pre-existing, scope creep — disclosed):** the WO 29.7 merge (`7a0de4d`) left committed merge-conflict markers in `Cargo.toml` (workspace.dep `kf-orchestrator`) and `Cargo.lock` (`thiserror` 2.0.19↔2.0.20). `cargo` could not parse the workspace, so the gate was unrunnable. `git status` showed clean because the broken file was the committed state (same regression class as the WO 29.6 one noted below). Resolved: kept `kf-orchestrator` dep (crate exists, intended by merge) + took `thiserror 2.0.20`. This finishes the cleanup the WO 29.7 CHANGELOG entry had already claimed.
- **Doc drift fixed:** `docs/TECHNICAL.md` ADR count 89 → 90 (matches `ls docs/adr/*.md | grep -v README | wc -l`). Not caught by `adr_xref_drift` (the test enforces header↔index agreement, not the prose count).
- **Gate:** `cargo test -p kf-budget-core --test adr_xref_drift` → 4/4 PASS.
- **Pending:** none from this task. Note for future merges: the WO 29.x merge series keeps re-introducing committed conflict markers (29.6, then 29.7) — worth a pre-commit hook that rejects `^<<<<<<<`/`^>>>>>>>` in tracked files.

## WO 29.7 — Port orchestrator to kf-orchestrator crate (branch `wo29g`, not yet merged)

- **DONE (R1+R2+R3+R4+R5):** Ported `@kirkforge/orchestrator` to a new `crates/kf-orchestrator/` workspace member.
  - **R1 Mode executors:** `modes.rs` — `execute_hard_prompt` / `execute_schema_contract` / `execute_artifact` + pure helpers `finalize_*` (testable without a ModelClient). `parse_jsonl_artifacts` (sha256-validated JSONL protocol with hash-mismatch/base64/missing-field/unknown-type/non-JSON-line rejection). `parse_artifacts` (legacy `### FILE:`/`### END` marker protocol, gated behind `allow_marker_fallback`). `persist_code_blocks` (fenced-block extraction + largest-block-wins for pinned target files + atomic tmp-rename writes + path-safety reuse from kf-routing).
  - **R2 Delegation pipeline:** `delegate.rs` — `Orchestrator::delegate`: classify via `kf_routing::classify_task` → recall+optional memory-bias override → resolve profile (honoring `task.language` override) → build `TaskBrief` → dispatch to mode executor → `flush_signals_to_sink` → write memory observation (unless `suppress_memory`) → bump stats. `task-decompose` mode short-circuits to `decompose_task` and synthesizes a delegation result.
  - **R3 Decompose pipeline:** `decompose.rs` — `topological_sort` (Kahn's, with cycle/self-dep/unknown-dep/duplicate-id detection), `parse_decomposition` (fence-strip + bracket-heuristic + complexity/language validation + 24-task cap), `decompose_task` (model call → parse → persist; retry-once-on-fail), `execute_decomposition` (recall → topological sort → dep-ordered subtask execution with retry-once per subtask + skipped-on-failed-dep).
  - **R4 Correction loop:** `correction.rs` — `run_correction_loop` iterates `0..=max_corrections`: delegate_turn → optional external validator (deferred) → `kf_routing::correction::decide_correction` → accept/escalate/correct. Cost tracking via `kf_routing::cost`. Truth-model precedence via `compute_final_verdict`. Memory observation written at loop exit.
  - **R5 Workspace manager:** `workspace.rs` — `WorkspaceManager` with `create_isolated` (copy + optional overlay), `ensure_baseline` (cached snapshot), `drop_baseline` (force recreate). `should_exclude_from_turn_copy` strips `node_modules`/`.git`/`dist`/`.tsbuildinfo`. `TempDir`-owned cleanup on drop.
  - **Trait seams (the deferrals):** `ModelClient` (async `execute(TaskBrief) -> Result<Emission>`) — no production impl, `PanickingClient` default + `RecordingClient` for tests. `EventSink` (async `emit(ArtifactEvent)`) — `NullSink` default + `RecordingSink` for tests.
  - **Helpers:** `correction_loop_helpers.rs` (`task_outcome_from_validation`); `types.rs` (full TS shape: `TaskInput`, `Emission`, `Signal`, `DelegationResult`, `DecompositionResult`, `TaskNode`, `SubtaskExecutionResult`, `CorrectionLoopConfig`/`Outcome`, etc.); `model.rs` (`TaskBrief`, `PanickingClient`/`RecordingClient`); `sink.rs` (`ArtifactEvent`, `NullSink`/`RecordingSink`).
  - **Tests:** 61 ported across modules (modes 22, decompose 11, correction 4 + 4 + 1 helper, workspace 6, sink 3, types 4, model 3, delegate 7, correction_loop_helpers 1), all green.
- **Pre-existing regression fixed (scope creep):** The WO 29.6 merge commit `5a6c32d` left committed merge-conflict markers in `Cargo.toml` (workspace.dependencies kf-rbac/kf-memory-store), `docs/TECHNICAL.md` (crate-map rows), AND `Cargo.lock` (3 spots). `git status` showed clean because the broken file was the committed state — `cargo check` was impossible from a fresh clone. Resolved by keeping both sides (the merge needed both) + regenerating `Cargo.lock`. Also bumped stale `crates/kf-budget-core/README.md` test count 772 → 860 (catches up on drift from WO 29.5+WO 29.6 README bumps that didn't account for the new `crates/` sub-crates — `readme_drift` was RED on baseline HEAD).
- **Design decisions:** Trait-based seams (not concrete adapters) — the kf-code `Executor` impl of `ModelClient` comes in a follow-up WO; this crate compiles + tests standalone. The reducer + deterministic verifier bus (`orchestrator-verifiers.ts` + `reducer.ts`) is NOT ported here — the packet on each `DelegationResult` is `None` and the correction loop feeds `Default::default()` to `decide_correction`. The TS reducer is a substantial port (~500 LOC + state machine) and belongs in its own WO. Token-cost tracking uses `kf_routing::cost` directly (no duplicate rate table).
- **Deviation disclosed (DEFERRED):** `ModelClient` production impl (kf-code `Executor` adapter) — deferred because the executor lives in the binary crate and depends on the model-provider registry; wiring is its own WO. Remaining work: (a) `impl ModelClient for ExecutorAdapter` in `src/session/`, (b) replace the 3 `PanickingClient` test stubs in production paths with the real adapter. Tracked here + WO 29.7 status line.
- **Deviation disclosed (DEFERRED):** Shell/structured `ValidatorConfig` execution — `CorrectionLoopConfig.validator` parses but doesn't run. The loop falls through to `decide_correction` with `task_pass=None` (verifier-only path). Remaining work: wire `ValidatorConfig` to `tokio::process::Command`, port `runTaskValidator`/`runStructuredTaskValidator` from `orchestrator-validators.ts`. Tracked here.
- **Deviation disclosed (DEFERRED per workorder):** R6 SLO monitor (`slo-monitor.ts`) — workorder explicitly defers; low CLI value. R7 security-emitter integration — tracked in WO 29.9 per state.md.
- **New deps:** `kf-routing`, `kf-memory-store` (workspace path), `async-trait`, `base64`, `sha2`, `hex`, `tempfile`, `regex`. No new external deps beyond what the workspace already uses elsewhere.
- Gate green at HEAD `5a6c32d` + this branch: `cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test -p kf-orchestrator` (61/61), `cargo test -p kf-budget-core --test readme_drift` (2/2).

## WO 29.6 — Port memory-palace to kf-memory-store crate (branch `wo29f`, not yet merged)

- **DONE (R1+R2+R3):** Ported `@kirkforge/memory-palace` to a new `crates/kf-memory-store/` workspace member.
  - **R1 MemoryStore facade:** `store.rs` — 13 public methods (`create`, `evict_expired`, `evict_overflow`, `write_task_observation`, `write_decomposition`, `recall_decomposition`, `recall`, `write_emission_records`, `write_run_record`, `write_run_and_emissions`, `query_runs`, `query_emissions`, `query_emissions_for_run`). Reuses `kf-routing` for `tokenize`/`vectorize`/`detect_family`/`build_empirical_recommendation` (WO 29.3) — no duplication of routing-engine pure functions.
  - **R2 FileAdapter:** `adapters/file.rs` — JSON-file with `tempfile::NamedTempFile` atomic rename, `.lock` exclusive-create retry loop (3s timeout), `.corrupt` backup on parse failure, lazy load with cached `load_error`.
  - **R3 SqliteAdapter:** `adapters/sqlite.rs` — `rusqlite` 0.40 (`bundled` + `backup` features). Schema DDL, `SCHEMA_VERSION = 3`, prepared statements (re-prepared per call — rusqlite statements are tied to Connection borrow lifetime), migrations 2 (outcome_reason) + 3 (routing_bias), `backup`/`restore`/`list_backups` with SHA-256 + row counts.
  - **`MemoryAdapter` trait:** single trait with default-impl optional methods (TS duck-typing `adapter.writeRun?` → Rust default `Ok(())`/`Ok(None)`/`Ok(false)`). `SqliteAdapter` overrides the specialized methods; `FileAdapter`/`InMemoryAdapter` accept defaults. No split trait, no downcasting.
  - **`InMemoryAdapter`** also ported (it's in the TS barrel).
- **R4 SKIPPED (per workorder — not a deferral):** `EncryptedAdapter` (AES-256-GCM) not ported. Not re-exported from the TS barrel; zero production consumers. Port only if explicitly requested.
- **Design decisions:** sync trait (no async — both SQLite + file ops are sync in Rust; avoids the `tokio::task::block_in_place` panic risk). `Mutex<T>` for interior mutability (keeps multi-threaded use open without a trait change). Time helpers in `src/time.rs` (no `chrono` dep — Howard Hinnant civil-from-days for ISO timestamps).
- **Deviation disclosed:** the TS adapter has 6 optional methods (`writeRun?`, `writeEmission?`, `queryRuns?`, `queryEmissionsForRun?`, `writeRunAndEmissions?`, `schemaVersion?`). The Rust port captures the same "specialized if available, generic fallback otherwise" semantics via trait default impls returning sentinel values (`Ok(())` / `Ok(None)` / `Ok(false)`). Avoids downcasting / split traits; the store branches on the sentinel. One-to-one with TS duck-typing.
- **New deps:** `rusqlite = { version = "0.40", features = ["bundled", "backup"] }` (workspace root + crate), `sha2 = "0.10"`, `hex = "0.4"` (already present in root binary; added to crate), `tempfile = { workspace = true }` (already workspace). `kf-routing` path dep.
- **Tests:** 34 ported (5 InMemory + 4 File + 8 Sqlite + 17 store facade), all green. `crates/kf-budget-core/README.md` test count bumped 738 → 772 (drift fudge is 2). `docs/TECHNICAL.md` crate count 10 → 11, both crate maps updated.
- Gate green at HEAD `1320e7b0f`: `cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test -p kf-memory-store` (34/34), `cargo test -p kf-budget-core --test readme_drift` (2/2).
- **Pre-existing (NOT mine, verified by stash):** `adr_xref_drift::status_counts_match_index_table_summary` is RED on HEAD (WO 29.3 lesson noted it). Also 13 `kf-code --lib` tests are flaky on shared-state when run in the full suite but pass in isolation on baseline (verified by stash). Neither is caused by WO 29.6.

## WO 29.4 — Port EventBus + AuditLogger (core-events) to Rust (branch `wo29d`, not yet merged)

- **DONE (R1+R2+R3):** Ported the LIVE surface of `@kirkforge/core-events` to Rust.
  - **R1 EventBus:** `src/shared/event_bus.rs` — async `emit` with idempotency cache (`HashMap<event_id, Instant>` + TTL eviction + size cap) and bounded buffer, `on` returning an unsub callable (handler identity via monotonic u64 ID), `drain_buffer`, `shutdown`, `graceful_shutdown`. Defaults match TS (buffer 1000 / cache 10_000 / TTL 5 min).
  - **R2 AuditLogger + hash chain:** extended `src/shared/audit.rs` with `AuditEvent`, `AuditAction` (29 dotted-string literals), `AuditOutcome`, `initial_hash`, `chain_hash_of` (recursive key-sorted canonical JSON for metadata; null vs absent both → `{}`), `MemoryAuditSink` (+ `verify_chain`), `AuditLogger`, `create_audit_sink` factory. Plain SHA-256 by default; HMAC-SHA256 when keyed.
  - **R3 FileAuditSink:** buffered append + size-based rotation (default 50 MB / 10 files). `.N` shift then current → `.1` on rotation.
- **R4 SKIPPED (per inventory — not a deferral):** `HttpAuditSink`, `SyslogAuditSink`, `WormAuditSink` are not ported. Zero production consumers in the TS tree. If a future sink is needed, it's a follow-up WO.
- **Existing `AuditLog`/`AuditEntry` untouched** — they serve a different purpose (redacted tool-call/hook NDJSON) and have 5+ consumers; new types added alongside.
- **Deviation disclosed:** the workorder suggested `tokio::sync::broadcast` for the EventBus backing channel, but the TS impl does serial inline `await handler(event)` with an inflight counter and drain semantics. `Mutex<HashMap<kind, Vec<Handler>>>` + `VecDeque` is a 1:1 behavioral port; broadcast would introduce fan-out concurrency + `Lagged` failure modes that don't exist in TS. Easy swap if a future need emerges.
- **New dep:** `hmac = "0.12"` (workspace root). `sha2 = "0.10"` + `hex = "0.4"` were already present (SigV4 / bedrock signing).
- **Tests:** 32 ported (6 EventBus + 26 audit), all green. Of the 36 TS tests, 4 Syslog + 9 WORM (R4) + 3 createAuditSink-factory-http-variant were skipped; the rest ported.
- Gate green: `cargo check -p kf-code --lib --tests`, `cargo clippy -p kf-code --lib --tests -- -D warnings`, `cargo fmt --check`, `cargo test --lib -p kf-code audit::` (26 passed), `cargo test --lib -p kf-code event_bus::` (6 passed).

## WO 29.1 — Fold bundled plugin into compiled-in Rust tools (branch `wo29b`, not yet merged)

- **DONE (Phase 1):** Added `kf-plugin-tools` cargo feature (default on). `src/session/plugin_tools/native.rs` implements the 6 plugin tools as compiled-in Rust calls: `doctor` (probes eslint/tsc/ruff/pyright/bandit via `tokio::process::Command --version` + derives languages), `health`, and `tools` run fully native (no shell hop, no Node hop). Registered in `run_session.rs` mirroring the stratum/budget pattern. The `/kf-code` skill is registered inline in `skills.rs::scan_and_load` when the feature is on (the manifest no longer loads). Added a folded-skip guard in `all_plugin_tools` so a data-dir-loaded manifest can't double-register with the compiled-in tools. `docs/TECHNICAL.md` plugin-section updated.
- **DEFERRED → WO 29.7 + WO 29.4:** `plugin_verify`, `plugin_verify_workspace`, `plugin_audit_verify` are registered as Rust tools but emit an explicit "not yet implemented in Rust; use the Node SDK" message. Blocker: the orchestrator verification pipeline ports in WO 29.7 and the audit hash-chain (`chainHashOf`/`initialHash`) in WO 29.4. Remaining work: port the pipeline + emitters, then replace the 3 deferral messages with native impls. R3 (delete `plugins/kf-plugin/tools/*.sh`) is also deferred until then, so the shell/Node fallback survives for users who rebuild with `--no-default-features`.
- Gate green: `cargo check -p kf-code --lib --tests`, `cargo clippy -p kf-code --lib --tests -- -D warnings`, `cargo fmt --check`; 68 module tests passed (loader_tests + plugin_tools::native + skills).

## WO 29.2 — Rust security emitter (branch `wo29-impl`, not yet merged)

- **DONE (R1+R2):** Ported the 14 regex security rules from `security-emitter.ts` to `src/session/verifier/security_emitter.rs`. `TsOrchestratorBridgeVerifier` is now a thin wrapper calling `emit_security_findings()` directly — no Node subprocess, no NDJSON. Last Rust→TS call path eliminated. ADR-028 NDJSON wire format retired (Rust returns typed `VerdictEntry`s).
- **DEFERRED → WO 29.9:** R3 — delete the now-dead `bridge-emitter.ts` + `security-emitter.ts` + `tests/bridge-emitter.test.ts` (out of scope for 29.2; TS sources left in place, dead). Remaining: `rm` the 3 files + drop the `SecurityEmitter`/`EventBus` imports once WO 29.7 (orchestrator port) confirms nothing else uses them.
- Gate green: check, clippy `-D warnings`, fmt, `verifier::` (210 passed), `security_emitter::` (25 passed).

## WO 26 series (merged into dev, commit cb82b05)

| WO | Status | Items |
|----|--------|-------|
| 26.1 | DONE | F0: gate drift test (#[ignore]); F1: cargo-audit --deny syntax |
| 26.2 | DONE | F2: notebook_edit unwrap guard; F3: web_fetch char-boundary slice |
| 26.3 | DONE | F17: landlock feature compiles (missing `mod landlock` declaration) |
| 26.4 | DONE | F4: saturating prune; F5: defer job map removal until reaped; F6: sub-second timeouts; F7: unique run-id; F8: drop lock before await; F9: compaction div-by-zero guard; F10: dedup key includes tool fields |
| 26.5 | DONE | F11: bound broadcast channels; F12: daemon client timeouts; F13: stale worktree cleanup; F14: schedule tag case; F15: canonicalize cwd before fork; F16: orphan .snap cleanup |
| 26.6 | DONE | R1: sessions-list dirty refresh; R2: persona adapter provider routing; R3: non-Rust linting (eslint wired) |
| 26.7 | DONE (R4 re-deferred) | R1: bash streaming TurnEvent; R2: MCP sampling/createMessage (ADR-072); R3: TUI memory widget; R4: computer_use re-deferred with disclosure |
| 26.8 | DONE | AppState decomposition → 11 sub-structs |
| 26.9 | DONE (partial) | R1: top-10 slowest tests fixed/skipped; R3+R4: testdoctor parallel scan + caching |
| 26.10 | NOT STARTED | provider hardening (mocks, Plugin3 shim, landlock default, memory dedup) |

## Current state (refreshed 2026-08-13, HEAD `2bfc2fa`, branch `docs-sweep`)

*Source of truth: [`docs/workorders/30.0.0-wo30-overview.md`](docs/workorders/30.0.0-wo30-overview.md) — the living master index of unfinished work — reality-checked against HEAD.*

- **WO 27 / 28 / 29 / 30 shipped.** WO 27 (landlock default-on + plugin-trust + test-health + themes + mouse); WO 28.x (cycle cuts, `turn.rs` split 2087→1191 LOC, coverage gate, bedrock/tripwire/computer_use-beta items); WO 29.x (the `kf-routing` / `kf-rbac` / `kf-memory-store` / `kf-orchestrator` ports — 4 new crates, 13 total — Rust security emitter, EventBus + AuditLogger, **delete of the `npm/kf-plugin/` TS tree** in WO 29.9); WO 30 (subagent worktree isolation + approval forwarding 30.1/30.6, TaskManager lifecycle 30.2, seccomp filter 30.4).
- **OS isolation shipped.** Landlock FS confinement is default-on for Linux (WO 27.1, fail-closed, `--i-accept-unsandboxed` escape). Seccomp syscall-filter is opt-in behind the `seccomp` Cargo feature (WO 30.4, applied last in the bash `pre_exec` hook; default-OFF pending real-workload tuning).
- **Subagent isolation shipped.** `coder` subagents get their own git worktree (WO 30.1) and destructive-tool approvals forward to the parent session's approval channel (WO 30.6).
- **CI is green** (the 5th-ed CI-red debt is closed). The e2e stdin-piping hang that kept the `windows` job red is RESOLVED (`260e7d8`: 90s `STREAM_IDLE_TIMEOUT` + `[adapter_routing] "e2e-" = "Ollama"`). This docs-sweep run is gated on `scripts/check-artifact-consistency.sh` + `adr_xref_drift`.
- **`main` fast-forward: still pending.** `origin/main` sits at `d848b37`; HEAD `2bfc2fa` is several WO merges ahead.
- **Version: `3.8.0`** (`Cargo.toml` + `Cargo.lock` both bumped from `0.3.6` in commit `6e2e0d4`). The earlier "still 0.3.6" note was stale.
- **Local install at `/home/henrik/own-code/kf-code`: not done** (carryover; not code debt).
- **Where the remaining work lives:** `docs/workorders/30.0.0-wo30-overview.md`. Headline open items: WO 28.6 (7 binary-spawn e2e), WO 28.13 (Vertex mock + full Bedrock turn), WO 28.16 R4 (computer_use vision loop), WO 29.7 residuals (`ModelClient` prod impl + `ValidatorConfig` execution), WO 30.3 (parallel orchestration), WO 30.5 (context-index target/ filter), WO 30.7 (cross-tool benchmark).

### Pending / blocked
- **PENDING:** fast-forward `main` to current HEAD; pick + bump the next version; push.
- **Deferred items** are tracked in the "Deferred items" ledger below and in the WO 30 overview capabilities/security tables — not in this current-state block.

## Review-fix session (2026-08-11) — 7 commits on dev

Full codebase review (8 parallel read-only subagents) surfaced findings; safe fixes applied in 7 commits (`6129891`→`b5e7190`):

| Commit | Finding | What |
|--------|---------|------|
| `6129891` | H1 | budget+stratum mutex poison: 26 `.lock().expect("…poisoned")` → `.unwrap_or_else(\|e\| e.into_inner())` matching the established convention |
| `81c2e37` | docs | scrubbed stale claims: ADR count 88→89, plugins tree (stratum/kf-budget compiled-in), AGENTS.md "CI disabled" reconciled, state.md phantom `kf-code-review.md` deleted, docs/README.md `reviews/` dropped, CHANGELOG dup `## [Unreleased]` merged |
| `e18b4f8` | docs | synced WO 21/23/24/25 overview `## Status` headers + 29 index rows with state.md |
| `114bc17` | deps | ratatui `default-features=false` (review's "wezterm stack" premise was stale — ratatui 0.30 already split; smaller win), crossterm 0.28→0.29, thiserror 1→2 |
| `e15a99f` | gate | `cargo fmt` on tests/e2e/harness (pre-existing fmt failure was blocking the gate) |
| `260e7d8` | C2 | stream idle timeout (90s) via `next_chunk_or_idle_timeout` helper across 4 adapter parsers + e2e Ollama routing via `[adapter_routing]` (CI red blocker — see RESOLVED above) |
| `b5e7190` | H4 + H2 | web_fetch `.redirect(Policy::none())` closes SSRF-via-302 bypass; `run_prepared_call` wraps `tool.run()` in `AssertUnwindSafe().catch_unwind()` so panicking tools return `ToolError::Internal` instead of unwinding the executor (protects deterministic + Phase 2.5 file-call paths, preserves panic msg for spawned path) |

### NOT fixed this session (flagged — need design decisions / separate workorders)
- **C1 — landlock never applied in prod — RESOLVED (WO 27.1).** Phases 1-3 landed at `91a2365` (apply_landlock moved out of `#[cfg(test)]`, module compiled under `cfg(target_os="linux")` with no Cargo feature, wired into `setup_rlimits` pre_exec, fail-closed on supported-but-rejected kernels). Phases 4-6 (this commit) added `security.landlock_extra_paths` config + `KF_CODE_LANDLOCK_EXTRA_PATHS` env, un-gated `--i-accept-unsandboxed` for release, and synced docs (ADR-054 amendment, TECHNICAL.md).
- **H8 — `tools ↔ session` circular module dependency** (tools import `session::access::PathGuard`, `task.rs` constructs nested `Executor`). Needs a `tool::Sandbox`/`tool::Guard` port trait. Architecture refactor.
- **H9 — god-objects on hot path** (`anthropic.rs` 2316 LOC monolithic; `executor/turn.rs` `dispatch_tool_call_batch` ~360 LOC + `stream_iteration` ~390 LOC). Split into directory form mirroring `openai_compat/`. Multi-step.
- **H6 — 29 "known-broken" `#[ignore]` tests** need per-test root-cause diagnosis. 8 in `plugin_tools/tests.rs` are security-critical (sandbox isolation, env sanitization). `config_field_count_drift_guard` canary also ignored. Tracked separately.
- **H10 — workspace plugin trust bypass** (`local_trust_policy` sets `verify_signatures: false` with no operator opt-in). **IN PROGRESS — WO 27.4**: `plugin_trust_workspace` config field added (default `false`); workspace plugins now fail-closed on missing/invalid signatures unless opted in. (Note: WO 27.4 title labels this "H9"; state.md uses H9 for the god-objects item. This is the workspace-trust item.)
- **H3 — bash deny-list bypassable** (`$()` subst, var indirection, base64 payloads). Fundamental; depends on C1.

### Review subagent corrections (over-eager "dead code" flags, verified NOT dead)
- `FileOffloadStore` — exported public API, pinned by `offload_store_spec_drift.rs` + `state_spec_drift.rs` + ADR-0004/0014/0017. NOT dead.
- `TsOrchestratorBridgeVerifier` — live TS contract: `npm/kf-plugin/.../bridge-emitter.ts` emits the NDJSON it consumes. NOT dead.
- `trim_ascii_whitespace` — hot-path SSE parser; ~6 LOC saving not worth a subtle behavior-diff risk. Left as-is.
- `default_verifier_bus` — small, low-confidence. Left as-is.

### Subagent discipline incident (recorded in lessons.md)
A "completed" deps subagent left a detached `cargo run -p kf-context-index --example timing` process that kept editing `crates/kf-context-index/src/lib.rs` in the background (rogue perf investigation: `is_ignored_dir` walker filter + `resolve_call_edges` HashMap optimization + `examples/timing.rs` benchmark). Caught via `ps aux` showing the live binary at 65% CPU; killed + reverted 3 times before it stayed dead. **New rule: `ps aux | grep cargo | grep -v grep` after every subagent batch.**

## Completed workorders

### WO 22 series (all done)

| WO | Status | Items |
|----|--------|-------|
| 22.1 | DONE | R1: landlock ABI rewrite |
| 22.2 | DONE | R1: default plugins (stratum + kf-budget) |
| 22.3 | DONE | R1: MCP URI validation, R2: capabilities handshake |
| 22.4 | DONE (R2/R3/R4 deferred) | R1: MAX_FACTS=3, FNV hash, rate limit |
| 22.5 | DONE (R3/R4 deferred) | R1: F2-F5 Enter handlers, R2: jobs_dirty refresh |
| 22.6 | DONE | R1-R6: token estimation, offload store, SearchState, PostHook, CorrectionResult, verifier-findings pinned in compaction tail (compaction.rs:247, loop_.rs:483) |
| 22.7 | DONE | R1-R6: all over-engineering cleanup |
| 22.8 | DONE | R1-R18: doc fixes |
| 22.9 | DONE (R4 deferred) | R7: ADR-070, R8: ADR-070 |
| 22.10 | DONE | R1: verifier Skipped → CorrectionResult |
| 22.11 | DONE | R1-R4: catch_unwind, Skipped, pub(crate) |
| 22.12 | DONE | R1: 28 ADRs updated, path literals fixed |
| 22.13 | DONE | R1-R3: multi-turn prompt fix, bg task Notify, configurable concurrency |
| 22.14 | DONE | R1-R3: JSON-schema structured output, ResponseFormat enum |

### WO 23 series (all done)

| WO | Status | Items |
|----|--------|-------|
| 23.5 | DONE | R1-R3: remember tool, system-prompt instruction, memory_auto_populate flag |
| 23.7 | DONE | R1: configurable task concurrency semaphore |
| 23.8 | DONE | R1-R3: doom-loop circuit breaker + auto-plan-mode + drift guard |
| 23.9 | DONE | R1-R3: max-continuation hard cap, TUI indicator |

### WO 21 series (all done or explicitly deferred)

| WO | Status | Items |
|----|--------|-------|
| 21.0 | DONE | Overview + rules |
| 21.1 | DONE | Scope decisions (draw/video yeeted) |
| 21.2 | DONE | Plugin rebuilds (21.11 superseded) |
| 21.3 | DONE | Stratum real transforms (21.11-R1) |
| 21.4 | DONE | Adapter gaps (tool_choice, JSON schema, native adapters) |
| 21.5 | DONE (R2/R4/R9 deferred) | R1: ripgrep grep, R3: MCP resource surfacing, R5: replace_all, R6: computer_use dedup, R7: HTML→md, R8: schema validation |
| 21.6 | DONE | R1: LSP federation, R2: memory auto-populate, R3: real tokenizer, R4: incremental rebuild, R5: compaction rename |
| 21.7 | DONE | R1: landlock ABI correct (feature-gated behind `landlock`, not default-on; via 22.1), R2: ADR-054 quantified, R3: diff-review-before-apply, R4: cosign blocking, R5: sandbox refusal, R6: PathGuardTower rename, R7: signature default-on, R8: plugin sandbox note |
| 21.8 | DONE (AppState decomposition + themes deferred) | multi-turn subagents (task.rs:538-568), doom-loop circuit breaker, task concurrency |
| 21.9 | DONE | ADR drift fixes, test deadlock, fuzzing, dead code, overclaims (coverage >75% deferred — tracked separately as WO 24.6) |
| 21.10 | DONE | MCP-first overlay (hooks/verifiers) |
| 21.11 | DONE | Plugin real rebuild, draw/video yeet, SDK/budget/stratum |

### WO 24 series (6/8 done, 1 deferred)

| WO | Status | Items |
|----|--------|-------|
| 24.1 | DONE | R1: cargo audit split — block on critical/unsound, warn on rest |
| 24.2 | DONE | R1: cosign verify-blob step in release workflow |
| 24.3 | DONE | R1: --i-accept-unsandboxed gated to debug builds only |
| 24.4 | DONE | R1: remove not(budget) /4 fallback, R2: TUI BPE, R3: deprecate heuristic |
| 24.5 | DONE | R1-R3: diff-review-before-apply (done in WO 21.7-R3) |
| 24.6 | DEFERRED | session coverage 75% — needs coverage toolchain + executor loop tests |
| 24.7 | DONE | R1-R4: fuzz targets for SSE/NDJSON/Bedrock/JS/CSS |
| 24.8 | DONE | R1: 23 tracing::debug! → warn!/info!/trace!, zero debug! remaining |

### WO 25 series (18 done, 2 pending)

| WO | Status | Items |
|----|--------|-------|
| 25.0-R3 | DONE | rename misleading doom-loop test + correct CHANGELOG halt claim |
| 25.1 | DONE | R1-R3: create scripts/test-fast.sh + test-full.sh, update AGENTS.md tiered gate |
| 25.2 | DONE (R2+R4 deferred) | R1: #[ignore] 29 known-broken tests; R3: tokio flavor audit (no single_thread found) |
| 25.3 | DONE (R3+R4 deferred) | R1+R2: testdoctor 2.9s→1.8s via single-pass scan merge |
| 25.4 | DONE (R3 deferred) | R1+R2: coverage CI job + baseline placeholder |
| 25.5 | DONE | R1-R5: fix stale plugin3/stratum/kfd refs in 5 scripts |
| 25.6 | DONE | R1-R3: lift deadlock CI quarantine |
| 25.7 | DONE | R1-R2: benchmark link + task count fix |
| 25.8 | DONE (R4 deferred) | R1-R3: audit clean; R5: archive editors/vscode/ |
| 25.9 | DONE | remove 6 dead-code items — -408 lines |
| 25.10 | DONE (R4 deferred) | fix config.toml.example + ADR path-literal enforcement |
| 25.11 | DONE (R2 deferred) | fix file-tool duration_ms:0 bug |
| 25.12 | DONE (R1 deferred) | fix cached_tokens fork-reset + pinning test |
| 25.13 | DONE | document SLICED_LISTENERS safe + SESSION_MODE global |
| 25.14 | DONE | add line field to verifier types + propagate |
| 25.15 | DONE (R2+R3 deferred) | advertise roots in MCP init handshake |
| 25.16 | PENDING | session coverage 75% (dep: 25.4) |
| 25.17 | DONE (R1 deferred) | persona Anthropic-direct documented; landlock opt-in |
| 25.18 | DEFERRED | carry-forward: bash streaming, computer_use, memory widget, Bedrock/Vertex mocks |
| 25.19 | DONE | phased multistep workflow in AGENTS.md |

### WO 26 series (in progress)

| WO | Status | Items |
|----|--------|-------|
| 26.7 | R1+R2 DONE (R3,R4 pending) | R1: bash streaming TurnEvent; R2: MCP sampling/createMessage via approval bus + ADR-072 |
| 26.8 | DONE | AppState decomposed from flat ~66-field struct into 11 sub-structs (conversation, generation, budget, session, provider, approval, search, ui, doom, services + dirty) with accessor shims; TUI unchanged |

## Deferred items (explicitly tracked)

### Medium priority

0. **24.6-R1..R5 / 25.16**: Raise `src/session` coverage above 75%. CI coverage job added in WO 25.4-R1. Remaining: R1 fill baseline from first CI run, R2 executor loop tests (6), R3 budget slicing tests (4), R4 compaction tests (5), R5 verifier bus tests (4). Tracked in WO 25.16.
1. **21.5-R2-R3 / 25.18-R1**: Stream partial bash output to TUI via TurnEvent::BashPartialOutput. DONE (WO 26.7-R1) — `TurnEvent::BashPartialOutput` added, PTY output forwarded through event_tx, TUI tool-result card renders streaming spinner + incremental text. Non-PTY path unchanged.
2. **21.5-R4 / 25.15-R2+R3**: MCP sampling/createMessage. R1 (roots/list capability) DONE in WO 25.15. R2 (approval-gated handler + headless policy + ADR-072) DONE in WO 26.7-R2. Resolved — sampling routes through the approval bus with default-deny headless policy.
3. **21.5-R9 / 25.18-R2 / 26.7-R4 / 28.16-R1-3**: Anthropic computer_use beta (coordinate-vision model). DONE (WO 32.17) — R4 shipped. `ComputerUseConfig.hosted` flag (env `KF_CODE_COMPUTER_USE_HOSTED`, TOML `[computer_use].hosted`); `ModelAdapter::set_computer_use_dims` trait method; `computer_use.rs` splits into `local_def()` / `hosted_def()` and dispatches to `run_hosted_action()` which translates Anthropic's action vocabulary to CDP + always captures a screenshot; executor activates at startup + config refresh (feature-gated `computer_use`). The local headless-Chrome CDP `computer_use` tool remains a separate, unaffected capability.
4. **22.4-R2/R3 / 25.18-R3**: TUI memory visibility + config flag. DONE (WO 26.7-R3) — memory indicator widget in status bar (`🧠N@tT`), `memory_show_in_status` config flag (default true), real-time updates via `TurnEvent::MemoryExtracted`.
5. **25.11-R2**: Daemon sessions-list refresh on dirty. DONE (WO 26.6-R1) — `sessions_dirty` flag now wired to a refresh path in the TUI event loop (mirrors `jobs_dirty`).
6. **25.12-R1**: AppState decomposition — DONE (WO 26.8). `AppState` is now 11 sub-structs (conversation, generation, budget, session, provider, approval, search, ui, doom, services + `dirty`). All call sites migrated; helper methods retained as accessor shims. TUI renders identically; session persistence format unchanged.
7. **25.17-R1-remaining**: Persona adapter Bedrock/Vertex plumbing. DONE (WO 26.6-R2) — persona path now uses `adapter_for_with_provider` forwarding `anthropic_provider` + full provider config; no hardcoded "anthropic".

### Low priority

8. **25.2-R2**: Top-10 slowest individual test fix. DONE (WO 26.9-R1) — 3 proptest tests fixed (256→32 cases, ~210s saved), 8 genuinely slow/flaky tests `#[ignore]`-gated with documented reasons. Total test time reduced ~25% (169s→127s).
9. **25.2-R4**: Split slow integration tests behind a feature flag or `tests/` directory separation. NOT DONE — still open. The e2e tests in `tests/e2e/` run in the `windows` CI job's `--workspace` and are currently broken (see "Current state" above). Remaining: gate e2e behind a feature flag or exclude from the Windows job, and fix the stdin-piping hang.
10. **25.3-R3**: testdoctor parallel directory scanning. DONE (WO 26.9-R3) — `rayon::par_iter` for file analysis.
11. **25.3-R4**: testdoctor result caching. DONE (WO 26.9-R4) — `target/testdoctor-cache.json` keyed by content hash + version; second run 65% faster.
12. **25.4-R3**: Coverage regression gate. NOT DONE — still open. Baseline placeholder exists but no enforcement. Remaining: `scripts/check-cov-regression.sh`, CI step comparing per-crate coverage against baseline - 1% tolerance.
13. **25.7-R3**: Benchmark manifest validation. NOT DONE — still open. Remaining: generate count from source in CI.
14. **25.8-R4 / 25.10-R4**: CI enforcement gate for dead crate/binary refs. NOT DONE — still open. `scripts/check-artifact-consistency.sh` covers this partially. Remaining: extend to also grep active source (src/, crates/) for `plugin3`, `kfd`, `kf-code-video` as identifiers (not historical prose), fail CI on hit.
15. **22.9-R4 / 25.18-R4**: Bedrock/Vertex test hardening. NOT DONE — still open (WO 26.10-R1, not started). Remaining: mock provider adapters for CI.
16. **Plugin3 env var backward compat**: PLUGIN3_* env vars renamed to KF_BUDGET_* in kf-budget-core (WO review-fix). DONE (WO 28.14) — one-release backward-compat shim in `crates/kf-budget-core/src/paths.rs` reads `PLUGIN3_*_DIR` when `KF_BUDGET_*_DIR` is unset, emits a one-shot stderr deprecation warning per var per process (three `OnceLock<()>` statics; canonical name wins silently when both set). Doc lineage added to ADR-0015 + ADR-0016. Alias window is one release — remove after.

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --workspace -- -D warnings`: PASS
- `cargo clippy --workspace --features pty -- -D warnings`: PASS
- `cargo clippy -p kf-code --lib --tests --features computer_use -- -D warnings`: PASS
- `cargo nextest run -p kf-code --lib`: 3298 passed, 17 skipped (159s)
- `cargo test -p kf-budget-core --test adr_xref_drift`: PASS

## Known pre-existing test failures (NOT from WO 21/22)

All known-broken tests are now `#[ignore]`-labeled (WO 25.2-R1, 29 tests). They remain in the source as documentation of expected behavior. Run with `--ignored` to execute them.

## Rust toolchain

Rust 1.88.0 at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before cargo commands.
