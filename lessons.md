# Lessons — WO 43 session

## WO 46.28 (prune_oldest_in_dir wrong slice)

- External-model bug reports can misdiagnose root cause AND propose the
  wrong fix. WO 46.28 framed this as "keep semantics leak" and proposed
  `entries[..keep]` (delete everything beyond keep). That would have
  been a data-loss regression: `/sessions prune` defaults N=5, K=10, so
  on 100 sessions it would erase 85. The documented contract (4 doc
  sites) is "delete the OLDEST N, keep K most recent" — delete-at-most-N
  is a budget, not a vacuum. Reading the caller (sessions.rs arg parser
  + help text) was what revealed the real intent. Lesson: when a bug
  report proposes a fix, still verify the fix against every caller and
  every doc site before applying it.
- The actual bug was the *direction* of the slice in a newest-first
  list: `entries[keep..keep+delete_count]` deletes the N just-beyond-keep
  (newest of the surplus), leaving the absolute oldest alive. Correct:
  `entries[len - delete_count..]` (the tail = oldest). Same guard, same
  budget semantics, one slice index changed.
- A test whose NAME/COMMENT says the right thing ("deletes oldest") but
  whose ASSERTION encodes the bug (deletes the middle session) is a
  tell: the author intuited the intent correctly, then copy-pasted the
  actual output. Trust the name + the doc contract over the assertion
  when they diverge — and fix the assertion to match.
- GitNexus `detect_changes` reports "no changes" from a worktree: the
  index is on the main checkout, so worktree diffs are invisible to it.
  Expected per the worktree/LSP caveat in AGENTS.md. Rely on `git diff`
  for worktree-scoped change review.
- Build contention under 8+ parallel worktree compiles makes even
  `cargo check` time out at 15-20 min; a single `cargo test -p kf-code`
  cold build took 18m45s. Poll load and wait for siblings to drain
  before launching gate runs. The one flake I hit
  (`attached_cancel_token_kills_inflight_bash_promptly`, a 10s-window
  subprocess-death assertion) is a load-induced timeout, not a logic
  failure — it passes in 5.52s in isolation. Same class as AGENTS.md's
  "Known flakes". When a gate test fails under heavy load, re-run it
  ALONE before treating it as real.

## WO 46.25 (ci-local.sh set -e vs run_step)

- `set -euo pipefail` + a helper that `return 1`s on a recorded failure =
  dead `failures[]` machinery. The non-zero return triggers `set -e` and
  kills the script before the summary prints. Fix: the helper records
  the failure and returns 0 (or just falls through); the final summary
  exits non-zero based on `failures[]` content. General rule: under
  `set -e`, any helper that wants to "record and continue" must NOT
  return non-zero — `set -e` makes the return an immediate exit.
- Host OOM under parallel worktree compiles is fierce: 6 sibling
  worktrees each running full-workspace `cargo nextest`/`clippy`
  simultaneously OOM-killed my `test-fast.sh` runs (exit 137, mid-link of
  `kf-code`) until the siblings finished. `CARGO_BUILD_JOBS=2` +
  waiting for the siblings to free memory was the only thing that
  worked — `nextest`'s own thread pool doesn't cap build-jobs. When
  blocked, a single small-crate `cargo test -p kf-budget-core --test
  adr_xref_drift` (the WO's namesake drift gate) compiled and passed in
  ~4 min and confirmed the WO/README status two-source-of-truth while
  the full gate was impossible.
- `scripts/test-fast.sh` does NOT invoke `scripts/ci-local.sh` — they're
  independent. A change to ci-local.sh cannot affect test-fast.sh's Rust
  results, so a green test-fast.sh is a "no Rust regression" signal, not
  a "ci-local.sh logic is correct" signal. The ci-local.sh logic itself
  was verified by `bash -n` + reading the flow.

## WO 43.23 (subprocess lifecycle)

- libtest `--exact` does NOT match names built from `module_path!()` (it
  includes the crate prefix; harness names don't). Use a unique substring
  filter + `--ignored` for re-exec helper tests.
- tokio `Child::id()` returns Option<u32>; std `Child::id()` returns u32.
- PDEATHSIG fires on death of the *forking thread*. Tokio worker threads
  live for the runtime, so async-context spawns are safe; beware ever
  moving a setup_process_group spawn into `spawn_blocking` (thread exits
  after the task -> premature child kill).
- `same_ms_double_spawn_gets_distinct_worktrees` flakes under full-suite
  parallel load (documented state.md:98) — passes 3/3 isolated; its git
  spawns use plain std Command, unaffected by process_group.rs changes.
- Teardown sweep must persist exit summaries (WO 43.10) BEFORE cancelling,
  else run_session's later persist finds no Running jobs and --resume
  loses the died-with-session report.
- cfg(test) const override (READER_IDLE_TIMEOUT 10s -> 300ms) is the sane
  way to timing-test reader policy under the 30s ci-fast per-test cap.
- kf-lsp has a DUPLICATE setup_process_group (lib.rs:1059) without
  PDEATHSIG — flagged in WO 43.23 Done for a future WO.
- Cold `cargo check --workspace --all-targets` in a fresh worktree: 10-20+
  min; budget gate time accordingly.

- Backlog dumps from earlier assessments drift FAST in this repo. ~11 of the
  user's ~25 claims were stale (test code mistaken for prod, shipped features
  re-listed as gaps, moved files). Always re-verify file:line before writing
  a WO. The parallel-agent + verify-then-write pattern worked well.
- scout.rs:138 unimplemented!() is #[cfg(test)]-only — the "live panic path"
  claim was wrong. Verified by direct read, not just agent report.
- WO README index + `## Status` header keyword agreement is enforced by
  `cargo test -p kf-budget-core --test adr_xref_drift` — new WO series MUST
  add README rows in the same commit. "Planned" is a recognized keyword.
- proptest is already a root dev-dependency; kf-routing just needs the one-liner.
- Several "missing" features already shipped under other WOs: MCP content-hash
  consent (42.5), Windows rename retry, landlock default-on (ADR-054 amendment),
  seccomp opt-in (30.4), WO 7.6 Done. Check state.md Shipped list first.
- Commit landed on `main` (this checkout's working branch; main==dev per
  state.md). Not pushed — user did not request push.

## Round 2 backlog triage (post-WO 43)

- Entire backlog dump predates WO 42 series. 11/12 claims stale. The git log
  is the cheapest verifier: `git log --oneline -1 -- <file>` immediately
  showed `af411364 fix(wo42.1): delete dead testdoctor test` — matched the
  user's claim exactly, already fixed.
- ADR-004 amendment shipped as WO 41.3 (2026-08-22, same date as this
  backlog's apparent snapshot — the dump and the fix crossed in the mail).
- kf-testdoctor's `#[ignore]` mentions in apply.rs are its own DOCTOR FEATURE
  (it adds ignore attributes), not broken tests — don't confuse them.
- Threshold enforcement (68.5/76.0/75.0 python heredoc) lives ONLY in
  ci-local.sh; ci-nightly uploads a report without enforcing. If someone
  asks "why does CI pass with lower coverage locally-enforced" — that's the
  ADR-074 design, local gate + nightly report.

## Round 3 fresh audit (WO 43.18-43.24)

- Fresh-exploration rounds beat backlog verification: 7 agents, all NEW
  findings, near-zero drift because nothing was pre-claimed.
- Best hunting ground: cross-surface seams. Per-surface work (TUI, daemon,
  bash runner) was solid; bugs lived at exits that bypass all surfaces
  (line-mode default SIGINT, panic-abort skipping Drop) and in files the
  grep-based WO 38.3 sweep structurally missed (tools/grep.rs itself).
- panic="abort" + BufWriter is a real pattern-combo bug class here: any
  Drop-based flush is dead code in release. audit.rs had it; check other
  Drop-flushed writers before assuming durability.
- The classic ratatui emoji-slice panic was ALREADY fixed (WO 38.2/38.11) —
  but its regression tests are ASCII-only, so a real multibyte cursor bug
  (app.rs:872 byte+char mixup) ships green. "Fixed" without a unicode
  regression test isn't fixed.
- cargo tree --duplicates + reading feature lists found 1-2MB of wins in
  30 min (ungated headless_chrome, arboard image-data). Empty cargo
  features that gate nothing are free money.

## Round 4 — WO 43.26 (workflow bash + plugin-bus verifier)

- Premise drift caught before code: the task claimed PluginBusVerifier had

- The `audit BufWriter` + `panic = "abort"` combo from WO 43.18 round-3
  notes was the real deal: audit was the LEAST durable store (buffered
  until Drop, which panic-abort skips). Per-entry flush+sync_data fixes
  it; the BufWriter is now a 1-line always-flushed buffer (harmless,
  keeping the type avoids touching the struct).
- `resume_chain` doesn't need the `hmac_key`: the `chain_hash` is stored
  IN the parsed event (computed at write time with the key), so reading
  it back gives the correct resume point without recomputing. Only
  `initial_hash`/`chain_hash_of` need the key.
- nextest filter syntax gotcha bit again (round-4 lessons): `-E
  'test(/regex/)'` with `|` alternation INSIDE the regex works; the
  task's quoted `-- "a\|b"` syntax matched 0. Always write a script file
  for complex nextest filters — inline bash quoting is fragile.
- `CachedIndex::load` returning `Err` on format_version mismatch is the
  ponytail path: the caller (run_session.rs:507-508) already treats
  `Err` as "corrupt, rebuilding". Zero caller changes — the new field
  just makes "old format" look like "corrupt" to existing code.
- Build throughput is STILL the bottleneck (parallel worktrees). A
  single `cargo check --lib` took 15min, `cargo nextest run --lib` took
  40min (test profile compiles more). The `nohup` + `kill -0` polling
  pattern from round-4 lessons is essential — the bash 120s/300s/540s
  timeouts kill the wrapper, not the build.
- `std::mem::forget(log)` is the clean way to simulate SIGKILL in a
  test: skips Drop without aborting the process, so assertions can run
  after. The test verifies the per-entry flush landed on disk without
  relying on the Drop-based flush.

- Premise drift caught before code: the task claimed PluginBusVerifier had
  "NO timeout, NO kill_on_drop" but WO 38.3 (already in-branch) added a 5s
  killpg watchdog inside `kf-plugin-host/verifier.rs::PluginVerifier::run`
  itself. Verified via `git merge-base --is-ancestor 0ad1929e HEAD` AND by
  reading the WO 43.23 file (which the 43.26 WO itself references) — 43.23
  line 41-42 explicitly acknowledges "PluginVerifier has a 5s killpg
  watchdog". The honest fix for bus.rs was a pinning test locking the
  watchdog behavior at the wrapper level, NOT re-adding a timeout that
  already exists. Re-implementing it would have either (a) wrapped a
  bounded call in a second bound (harmless but redundant) or (b) required
  changing the sync `BusVerifier` trait to async — explicitly forbidden
  by AGENTS.md §7 ("Don't try to unify them in one pass").
- The genuinely-unguarded path was workflow.rs `run_bash` + `run_batch`
  Bash arm: `Command::output().await` with no kill_on_drop, no timeout,
  no cancel. That's where the real fix landed.
- Build throughput is the bottleneck on this machine: `cargo check --lib`
  took 13min, `cargo nextest run --lib` took 6min, `cargo clippy
  --all-targets` took 10min — because parallel worktrees (wo43.30 etc.)
  were compiling concurrently (load avg 26, 12 rustc processes). Used
  `nohup ... > /tmp/...log &` + `kill -0 <pid>` polling to avoid the
  120s/300s/540s bash timeouts. The "no output" symptom on the timed-out
  commands was misleading — the process was alive, just slow; the
  shell_metadata timeout killed the wrapper, not the build.
- nextest filter gotcha: `-- "a\|b\|c"` (shell-escaped pipe regex) matched
  0 tests. nextest wants `-E 'test(/regex/)'` filter expressions, or
  multiple positional regexes. Switched to `-E 'test(/workflow/)'` and
  `-E 'test(/bus/)'` separately.
- `adr_xref_drift` only enforces WO file header ↔ README index agreement
  for WOs ALREADY IN the README index. Round-4 WOs (43.25-43.39) had files
  but no README rows, so the drift test silently skipped them. Adding a
  README row is what makes the "Done" status enforceable. The task's
  explicit instruction to add the README row was load-bearing for the
  drift guard, not cosmetic.

## Round 5 — WO 43.21 (persistence crash-robustness)

- The `audit BufWriter` + `panic = "abort"` combo from WO 43.18 round-3
  notes was the real deal: audit was the LEAST durable store (buffered
  until Drop, which panic-abort skips). Per-entry flush+sync_data fixes
  it; the BufWriter is now a 1-line always-flushed buffer (harmless,
  keeping the type avoids touching the struct).
- `resume_chain` doesn't need the `hmac_key`: the `chain_hash` is stored
  IN the parsed event (computed at write time with the key), so reading
  it back gives the correct resume point without recomputing. Only
  `initial_hash`/`chain_hash_of` need the key.
- nextest filter syntax gotcha bit again (round-4 lessons): `-E
  'test(/regex/)'` with `|` alternation INSIDE the regex works; the
  task's quoted `-- "a\|b"` syntax matched 0. Always write a script file
  for complex nextest filters — inline bash quoting is fragile.
- `CachedIndex::load` returning `Err` on format_version mismatch is the
  ponytail path: the caller (run_session.rs:507-508) already treats
  `Err` as "corrupt, rebuilding". Zero caller changes — the new field
  just makes "old format" look like "corrupt" to existing code.
- Build throughput is STILL the bottleneck (parallel worktrees). A
  single `cargo check --lib` took 15min, `cargo nextest run --lib` took
  40min (test profile compiles more). The `nohup` + `kill -0` polling
  pattern from round-4 lessons is essential — the bash 120s/300s/540s
  timeouts kill the wrapper, not the build.
- `std::mem::forget(log)` is the clean way to simulate SIGKILL in a
  test: skips Drop without aborting the process, so assertions can run
  after. The test verifies the per-entry flush landed on disk without
  relying on the Drop-based flush.

## WO 43.22 (adapter transport robustness)

- Two `build_reqwest_client` fns exist: `src/adapters/mod.rs:67` (model
  adapters — the one WO 43.22 scopes) and `src/shared/mod.rs:14` (MCP
  etc., takes Option<Duration>). Don't conflate them.
- `retry_backoff` jitter is now wall-clock seeded — any test comparing
  two samples for equality will flake. Compare bounds (shipped that bug
  in b7ca2da2, fixed in caed29b5).
- yup-oauth2 12.x `AccessToken` exposes `is_expired()` with a built-in
  1-minute margin — enough for a token cache without parsing expiry.
- ci-fast (30s terminate-after=1) + box load ~21/8 cores = timing
  flakes in `tools::edit_file` roundtrips (27-31s each in isolation)
  and `attached_cancel_token_kills_inflight_bash_promptly` (12s iso).
  Both pass in isolation; wait for load < ~13 before judging a red run.
- detect_changes works from the MCP server's main checkout with the
  `worktree` param + `scope: compare, base_ref: <merge-base>`.
- worktree prompt overrides repo AGENTS cadence where they conflict
  (CHANGELOG/state.md/README forbidden; coordinator owns them).

# lessons.md — WO 43.24 session (appended)

- Cargo config dispatches `cargo test` to cargo-nextest (`-E` filters seen in pgrep).
- libtest accepts multiple positional filters after `--` (all my filters matched in one run).
- `MakeWriter` closure HRTB inference fails on toolchain 1.88: impl `MakeWriter`
  on the buffer type instead of passing a closure to `with_writer`.
- A tool-timeout kill of a command that launched a background build can kill the
  build AND corrupt a build-script OUT_DIR artifact (headless_chrome protocol.rs
  "unclosed delimiter"). Fix: rm -rf that build dir; launch long builds with
  `setsid nohup ... < /dev/null &` in a fast-exiting command.
- `$?` after `cmd | tail` reports tail's exit — use ${PIPESTATUS[0]} in gates.
- Load-avg 11 (3 concurrent worktree gates) flaked
  attached_cancel_token_kills_inflight_bash_promptly; passes on a quiet machine.
- `run_with_context` fires `post_hooks` (not `in_process_hooks` — those fire in
  `run_decision_inner`); the test-name wording is misleading.
- scope creep: none. access/mod.rs :879 test strengthened alongside :889/:899
  (same region, validates the capture harness — disclosed in WO Done).
## Round 5 — WO 43.20 finish (salvage session)

- Salvaged uncommitted work can be 90% right with a subtle 10%: the mini
  renderer compiled and passed its own tests but lacked Handlebars
  stand-alone-tag stripping. Ground-truth capture (scratch cargo project
  in /tmp with the real handlebars 6) settled "render identically" in
  minutes and doubles as the golden-test source. Always capture ground
  truth EMPIRICALLY, don't trust spec memory.
- The old system.hbs had a latent bug: {{! ... {{#if x}} ... }} comments
  leak text (handlebars closes {{! at the first }}). It shipped junk into
  every system prompt until the {{!-- --}} rewrite.
- WO 43.20 item 1's premise was upstream-wrong: aws-sigv4 sign-http NEEDS
  http 0.2 (canonical-request internals); the smithy-http default feature
  set keeps http-body 0.4. Feature-structure reading > version-number
  reading. Also: aws crates bump rust-version aggressively — the highest
  MSRV-≤1.88 set had to be pinned crate-by-crate via cargo update
  --precise (sigv4 1.3.8 / smithy-http 0.63.3 / runtime-api 1.11.3 /
  types 1.4.3 / async 1.2.11 / credential-types 1.2.11).
- base64 0.22 persists via hyper-util + jsonwebtoken (transitive) —
  deduping direct deps is still right but the lock keeps both copies.
- attached_cancel_token_kills_inflight_bash_promptly flakes under
  concurrent-worktree load (10s bound, took 13.5s with a parallel nextest
  running); passes in isolation at 7.25s. Not a WO-43.20 regression.
- A `cargo test --release` flyby on a debug-tested repo rebuilds the world
  (15 min wasted, timed out). Re-run flakes with `cargo nextest run -E`
  to reuse artifacts.
- Task explicitly forbade editing README/CHANGELOG/state.md/WO-README —
  so docs/workorders/README.md row 43.20 still says "Planned" while the
  WO header says "Done". adr_xref_drift isn't in this task's gate list;
  flag for the merger.

# Coordinator session — WO 43 closeout + WO 44 generation

- Interrupted sessions leave three-part residue: uncommitted worktree diffs,
  WO files flipped Done without README rows (drift test red), and state.md
  "Pending" sections contradicting their own commit messages. Check all
  three before trusting any of them.
- Pre-assigning WO number ranges to parallel auditors (44.1-19, 44.20-27,
  ...) gave zero file collisions and zero coordination traffic; the gaps
  are cosmetic. README rows are safe to add centrally afterwards because
  the drift test skips unindexed files.
- Worktree prompts must forbid README/state.md/CHANGELOG edits AND tell the
  agent its WO file is the ONE doc it owns — merges then stay code-only and
  docs consolidate in one commit.
- lessons.md IS tracked on dev (AGENTS.md says gitignored — stale); four
  branches appending to it conflicted on every merge. Concatenate-and-keep
  resolves cleanly.
- Detect_changes/detect-changes tooling doesn't see worktree branches from
  the main-checkout MCP server; agents ran their own verification. Trust
  but spot-check: their per-file diff stats matched the merge diffs.
- Same load-flake hit 3 of 4 worktrees (attached_cancel_token... at
  load>10). With ≥3 concurrent full-suite gates on 8 cores, budget for one
  re-run or serialize the final gate runs.

# Round 2 — dev-first flow enforcement (user directive)

- THE FLOW IS: push to dev → CI green → fast-forward main. Never land on
  main first. The 41-commit pile sat on local main with zero CI signal
  because two sessions skipped this.
- test-fast.sh (lib/bins) is NOT a merge gate — integration tests
  (readme_drift, context_economics, kf-memory-store, kf-routing) live
  outside it. Run scripts/test-full.sh before any dev push. Two of five
  CI failures tonight were invisible to test-fast.
- "Windows test parity" claims from 43.12/43.35 were never verified:
  pid_is_alive was hardcoded true on Windows, deny_paths compared
  '/'-only, ':'-splitting broke drive letters twice (lookup AND splice
  — extract the shared helper the first time). The windows CI job died
  on runner infra twice, which masked all of it.
- GitHub windows runners flake: install-action "bash startup failure"
  (partner-runner-images#169) — rerun the failed job, don't debug ghosts.
  But rerun ONCE; a second identical failure is real (run_bash_stuck
  deadlocked twice → real).
- git merge-base --is-ancestor misses rebase-merged branches; `git cherry
  <upstream> <branch>` (patch-id) found 11 "unmerged" worktrees that were
  content-identical. Use both before pruning.
- nextest per-test budgets + libtest "running for over 60 seconds" lines
  distinguish a hung future from a slow one — grep the full log for
  TERMINATING, not just FAIL.

## WO 44.38 (PTY streaming event ordering)

- `ToolStart` was emitted at record time (inside `record_tool_result`,
  after the tool body ran), so PTY chunks flowing during the body had no
  streaming card. Moving it to `spawn_batch` at dispatch time fixed the
  root cause. The two record-time emissions were redundant.
- The TUI `ToolStart` placeholder was NOT marked `streaming = true` —
  it defaulted to `false`. The old `BashPartialOutput` arm only checked
  `last.role == "tool"` (not `last.streaming`), so it appended anyway.
  When I hardened the arm to check `last.streaming`, the placeholder
  failed the check and a duplicate card was pushed. Fix: mark the
  `ToolStart` placeholder `streaming = true` (semantically correct —
  the tool IS in-flight).
- `cargo check --lib` on this cold worktree took 5+ minutes; `cargo
  check --workspace --all-targets` took 7+ minutes. The `cargo test`
  build for the test target was even slower. Budget 15+ minutes for a
  full gate cycle on a cold worktree. Running a single test by name
  still requires compiling the test harness (~3 min after the lib is
  built). The bash tool's 120s/300s timeouts were too short for cold
  builds — use 900s.
- `nohup cargo test > log 2>&1 &` loses the test result output (the
  process finishes but the log only captures the Compiling line —
  output buffering issue). Run `cargo test` directly in the foreground
  with a long timeout instead.
- Pre-existing flakes in `tui::commands::{jobs,tasks}` tests (date-based
  job files, filesystem-dependent task dirs) — unrelated to my changes.
  `same_ms_double_spawn_gets_distinct_temp_dirs` is a known concurrency
  flake that passes in isolation.

## WO 44.53 (nightly ollama CI scope)

- `cargo nextest` `-p <pkg>` is **package** selection, not test-target
  selection. For a bin-only crate (no `[lib]`), `-p <pkg>` selects every
  test target (bin unit tests + every `tests/*.rs`). To scope to one
  integration target, use `-p <pkg> --test <target>`. The nightly profile
  has no `default-filter` so `all()` applies — `--run-ignored all` then
  sweeps in every `#[ignore]`d unit test in `src/**`, including ones that
  panic headless (`src/tui/clipboard.rs:48`). The fix pattern mirrors
  `scripts/run-integration-tests.sh:30` (`cargo test --test
  integration_test -- --include-ignored`).
- The `nightly` nextest profile (`.config/nextest.toml:55-60`) is the only
  profile without a `default-filter`. All others (`ci-fast`, `ci-full`,
  `integration`, `e2e`) set `default-filter`. That's why `--run-ignored
  all` only goes wide on the nightly job — the other profiles would skip
  `#[ignore]` even if the workflow passed the flag.
- Timing-sensitive real-subprocess tests (`edit_file` proptests,
  `attached_cancel_token_kills_inflight_bash_promptly`) flake when the
  machine is at 2-3x CPU oversubscription from parallel sibling-worktree
  builds. Both pass in isolation / under `--no-fail-fast`. This is the
  documented Known flakes pattern, not a regression — a YAML-only change
  cannot affect Rust test logic.
- When the machine is saturated (load >2x cores), `cargo check
  --workspace --all-targets` (12-13 min) + `--no-fail-fast` full suite is
  a more honest gate than a single fail-fast run that aborts on the first
  CPU-starved timing test.

## WO 46.20 (never-ending job blocks scheduler shutdown)

- The fix went in `src/jobs/daemon.rs`, NOT `src/jobs/runner.rs`, even
  though runner.rs is where `job.timeout` is consumed. runner.rs is
  shared by the TUI "Run now" path (`src/tui/commands/jobs.rs:539`),
  which the user may want to run without a default cap. Applying the
  default in the daemon loop (before `tokio::spawn`) scopes the
  coercion to the unattended path only. The "do NOT edit shared files"
  rule plus "smallest diff" both point at the daemon, not the runner.
- `cargo check --workspace --all-targets` on a cold worktree: 14m32s.
  `cargo clippy --all-targets` after: 8m40s (warm). Budget 25+ min for
  the check+clippy pair on a cold worktree; run them in background with
  nohup and poll, don't block the foreground bash timeout.
- The 3 `tools::edit_file` proptest timeouts in test-fast were the
  documented CPU-oversubscription flake (3 sibling worktrees compiling
  kf_code simultaneously held the package-cache lock and saturated the
  CPU). Re-run in isolation: 3/3 PASS in 22.3s. My change is in
  `src/jobs/daemon.rs` and has zero relationship to `tools::edit_file`
  proptests — the flake is a load artifact, not a regression. AGENTS.md
  §6 forbids rewriting tests to make red go green; the right move is to
  re-run in isolation and disclose the flake.
- The `ponytail:` ceiling comment on `DEFAULT_JOB_TIMEOUT` names the
  upgrade path (per-kind defaults in `ScheduleSpec`). Long-running
  scheduled workflows that legitimately exceed 300s will hit the cap
  until they set an explicit `timeout` — that's the intended trade
  (free the daemon) and the comment makes it grep-able.
- scope creep: none. Single file (`src/jobs/daemon.rs`), 3 logical
  lines (import + const + coercion), all within the WO's named scope
  (`daemon.rs:146-197`).
## WO 46.24 — predictable .tmp TOCTOU (session 2026-08-26)

- The codebase already had the correct atomic-write pattern
  (`tools/atomic_write.rs`: O_EXCL + random tmp name + fsync + rename +
  permission preservation). 10 sites reimplemented it inline with
  predictable `.tmp` names. The lazy fix was reuse, not a new helper —
  the workorder's "create a `secure_atomic_write` helper" suggestion
  would have duplicated what already exists. Ponytail ladder rung 2
  ("already in this codebase? reuse it") applied.
- The workorder listed 9 line numbers but 2 of them (`audit.rs:143`,
  `cli_dispatch.rs:73`) were append-mode writes, not tmp+rename. They
  are a DIFFERENT attack shape (symlink on the target, not the temp)
  and need `O_NOFOLLOW`, not the tmp+rename migration. Disclosed as
  deferred in the WO + state.md pending — not silently dropped.
- Tests that asserted on a fixed `.tmp` path (`carryover.tmp`,
  `task-atom.json.tmp`, `config.toml.tmp`) became stale once the helper
  switched to random tmp names. The assertions were rewritten to check
  the directory contents / target file directly, not a fixed temp name.
  The stale predictable `.toml.tmp` is now harmless orphan litter — the
  save no longer opens it, so the "stale tmp is cleaned up" guarantee
  was rewritten as "stale tmp is never touched".
- `cargo clippy --all-targets` under heavy sibling-worktree contention
  took ~17 min; `cargo check --workspace --all-targets` ~8 min;
  `test-fast.sh` ~6 min. Budget 30+ min for the full gate when other
  worktrees are active. Run one gate at a time — parallel cargo
  invocations on the same target dir contend on the file lock.

## WO 46.8 — grep/glob cancellable (session 2026-08-26)

- `tokio::select! { biased; _ = ctx.token.cancelled() => ..., out = child.wait_with_output() => ... }`
  is the repo's established cancel pattern. When the cancel branch wins,
  the unfinished `wait_with_output` future is dropped, which drops the
  `Child`, which fires `kill_on_drop` — exactly the cancellation
  semantics we want. No explicit `child.kill().await` needed. Pattern
  references: `plugin_tools/wrapper.rs:324`, `session/bench.rs:61`,
  `session/verifier/security.rs:236`, `tools/workflow.rs:313`.
- `spawn_blocking` tasks CANNOT be killed from outside — dropping the
  JoinHandle detaches the task; the blocking-pool thread runs to
  completion. For `glob`/grep-fallback this is acceptable (the leaked
  thread is bounded by the walk/read finishing on its own; no
  subprocess). For `grep`'s `rg` it was NOT acceptable — the subprocess
  could hang indefinitely. The fix was to move `rg` to
  `tokio::process` (cancellable via `kill_on_drop`) rather than trying
  to kill the `spawn_blocking` thread. Key distinction: a blocking
  thread doing CPU/IO work is bounded; a blocking thread waiting on a
  hung subprocess is not.
- `tokio::process::Command::kill_on_drop` requires the `process` feature
  on tokio — this repo has `features = ["full"]` so it's available. No
  new dep needed.
- When a sync helper becomes test-only (production path moved to async),
  gate it with `#[cfg(test)]` AND gate its imports — otherwise
  `dead_code` warnings fire in non-test builds. `std::process::Command`
  was only used by the now-test-only `rg_available`/`run_rg_blocking`,
  so the import got `#[cfg(test)]` too.
- `tokio::process::Child::wait_with_output` consumes `self` (takes
  ownership, not `&mut self`). `let mut child = ...` then
  `child.wait_with_output()` triggers `unused_mut` — declare the child
  as `let child = ...` (no `mut`) when only `wait_with_output` is used.
- Full gate timing under sibling-worktree contention: clippy ~13 min,
  check --workspace --all-targets ~11 min, test-fast ~7 min (after warm
  cache). The first `cargo check --lib` after edits took ~8 min because
  the shared target dir (wo46.5) was cold for this worktree's fingerprint.
  Budget 30+ min; run gates sequentially (file-lock contention).

## WO 46.11 (ci-merge bench TOML [verify].type validation)

- The ci-merge.yml static job had the bench-TOML required-key check
  but was missing the [verify].type validation that ci-pr.yml:44-75
  has. Copy-verbatim from ci-pr.yml was the correct minimal fix
  (single YAML file, +9 lines). No new logic, no new deps.
- `scripts/test-fast.sh` under 7 concurrent sibling worktrees (load
  17-23, mem down to 1.6GB avail) repeatedly flaked the single
  real-subprocess timing test
  `attached_cancel_token_kills_inflight_bash_promptly` (fail-fast
  killed the run at 1284/4766). Re-running with `--no-fail-fast`
  completed all 4765 tests green (the flake test passed once the
  scheduler gave it a full core). This is the documented Known flakes
  pattern — a YAML-only change cannot affect Rust test logic. When
  load >2x cores, judge test-fast.sh red on the flake tests only after
  an isolation re-run; do NOT treat the load-induced flake as a
  regression.
- kf-code `--test` rustc compile in a cold worktree under mem
  pressure: 25+ min for that one crate (RSS 2.3GB). Polling with
  `setsid`-detached background + 90-115s sleep checks is the only way
  to outlast the 2-min bash-tool cap. Don't trust a frozen "Compiling
  kf-code" log line as stuck — check `ps STAT` (Sl = sleeping on
  I/O, R = running) and RSS growth to confirm progress.

# Lessons — WO 46.30 session (bench env-var leak)

## What I learned

- `shared::test_util::EnvGuard` and the kf-bench test-file `EnvGuard`
  are both `#[cfg(test)]`-only. Production RAII env guards must be
  local to the module that needs them (~25 lines); un-gating test_util
  for prod would be the bigger, wrong-direction diff.
- ENV_LOCK statics are module-local (config/mod.rs, adapters/anthropic).
  A new test mutating a var another module's tests mutate
  (KF_CODE_BUDGET_CEILING) must use a throwaway key — there is no
  cross-module env serialization.
- Full-suite gates are unattainable while sibling worktrees compile:
  3 worktrees × ~82 rustc threads each (load 17-27) starves even
  trivial tests past the 30s ci-fast slow-timeout — a pure string test
  (`edit_file_replacement_equals_replacen`) TIMED OUT at 30s. Evidence
  pattern that held: --no-fail-fast full run + immediate unstarved
  re-run of every anomaly + document. Same signature as WO 46.28's
  accepted flake note.
- `test_always_approve_rule_round_trips_to_next_turn` takes ~26s solo —
  only 4s under the ci-fast 30s budget. Expect it to flake under any
  real load.
- `cargo test | tail` hides all progress until completion — under load,
  use nohup + log file + poll, or you burn 30-min timeouts blind.
- lessons.md IS tracked here despite the stale .gitignore entry —
  APPEND, never Write the whole file (this session clobbered 517 lines
  and had to restore from HEAD).

## What didn't work / would do differently

- Two foreground `cargo test | tail` attempts (7 + 30 min) burned ~40
  min before noticing the box was saturated by sibling agents. Check
  `ps aux | grep rustc` + uptime BEFORE any compile in a worktree.
## WO 46.34 session (in-memory offload store FIFO eviction)

- The `readme_drift.rs` integration test (not just AGENTS.md prose)
  enforces the kf-budget-core README `| Tests | N passing |` row and it
  counts ONLY `#[test]` immediately followed by a `fn` line — my naive
  `grep -rc '#[test]'` overcounted by 18 (comment mentions etc.). Run
  `cargo test -p kf-budget-core --test readme_drift` to get the real
  number; the README row was 2 stale at HEAD (933 actual vs 931 claimed)
  — the wave-4 merge added tests without bumping.
- Detect_changes (gitnexus) does NOT see `.worktrees/woXX` changes — the
  index follows the main checkout. In a worktree, verify scope with
  `git diff --stat` + pre-edit impact() instead.
- Machine-load flakes dominate gate runs when parallel worktree agents
  run cargo concurrently (load 25-32 sustained). The ci-fast 30s
  slow-timeout is the edge: `test_always_approve_rule_round_trips_to_next_turn`
  takes 35.7s SOLO — it is structurally on the timeout edge and will
  flake whenever load pushes it past 30s. Candidate for a nextest
  per-test slow-timeout override (like `run_bash_stuck_step_times_out`)
  in a future WO.
- Plain `cargo test` (libtest) and nextest builds are separate artifact
  graphs: after touching kf-budget-core, the kf-code libtest binary
  relinks (~20+ min under load) even though nextest artifacts are warm.
  Budget accordingly; don't mistake it for a hang.

## WO 47.15 session (secret-env scrub, 3+1 spawn sites)
- Scope creep: `src/session/bench.rs` (verify_task_bounded) — sibling
  `sh -c` site of the exact WO class, same crate, one-line fix; fixed here
  rather than left leaking. Cross-crate siblings (kf-bench, kf-workflow)
  deferred — helper is pub(crate), promotion is an API change (follow-up WO).
- `scrub_secrets_from_child_env` order matters: scrub the inherited env
  BEFORE explicit `.env(k, v)` sets, so deliberate context vars win even if
  a name collides with a secret-shaped parent var.
- `attached_cancel_token_kills_inflight_bash_promptly` is load-flaky: fails
  at ~18s under full-suite parallel churn, passes solo in 8s. If it fires
  during test-fast.sh, rerun solo before diagnosing.
- Fresh worktree = ~20 min per cold cargo pass (check, clippy, test each).
  Budget an hour+ for gates; don't panic at silent long builds.
## WO 47.16 session (jobd auth timing oracle + socket perms)
- `check_auth_ct` extraction: keep `DaemonState::check_auth`'s signature
  (`Result<(), Response>`) and map the free fn's `Result<(), String>`
  via `.map_err(Response::error)` — zero churn at the 9 server.rs call
  sites + 5 test sites.
- jobs/daemon.rs already unix-gated via jobs/mod.rs, so PermissionsExt
  needed no cfg wrapper.
- The WO 46.28 flake + the edit_file 30s-edge pair flake as a SET under
  load: each test-fast run fails a different subset (cancel-test twice,
  then cancel GREEN and both edit_file tests timing out in the
  no-fail-fast run). Machine load 17-24 on 8 cores. All pass in
  isolation. Don't chase whichever one fired — check load first
  (`uptime`).
- cargo nextest vs libtest artifact graphs again: after `--lib` nextest
  runs, `cargo test --lib` relinks (10+ min under load). Budget gates
  accordingly.
## WO 47.21 session (ensure_private_data_dir OnceLock)

- The WO's claim that the OnceLock explains the `same_ms_double_spawn_*`
  flakes is WRONG for those two tests specifically: they create temp dirs
  via `std::env::temp_dir()` + `WorktreeSession::create`, never through
  `data_dir()`/`tasks_dir()`/`jobs_dir()`, and nextest gives each test its
  own process (fresh OnceLock). Their real flake: 10s readiness deadline
  starved when ~50 nextest processes oversubscribe 8 cores (load 43 with
  sibling worktree agents). They pass solo. Fix for them is a deadline
  bump / nextest slow-timeout override — NOT the OnceLock change. Filed in
  state.md Pending.
- `DataDirGuard` is safe across `tokio::spawn` in `#[tokio::test]` DEFAULT
  flavor (current_thread): spawned tasks run on the calling thread, so the
  thread-local override is visible to the worker. Would NOT hold for
  `multi_thread` flavor tests or `spawn_blocking`.
- `cargo check/clippy --all-targets` took ~15-16 min each at load 40+ with
  6 sibling cargo processes; budget an hour for the full gate matrix in
  that regime. test-fast: ~260-400s.
- `tasks_dir_respects_env_override` in tui/commands/tasks.rs silently
  drifted from its name (already used DataDirGuard-style paths? no — it
  used EnvGuard). When migrating a test to a different override mechanism,
  rename it in the same commit or the next reader trusts a lie.

## WO 47.20 session (2026-08-27)

- `maybe_wrap_cached` wraps BEFORE the executor pushes `set_*` config
  (executor/mod.rs:244-266) — a caching/config wrapper must capture
  request-shaping knobs in its own set_* overrides; the inner adapter
  keeping them is not enough for the wrapper to key on them.
- Existing skip-assertions in caching.rs checked `cache.get(model_name)`;
  after re-keying to a fingerprint they'd pass vacuously (different key).
  When the key derivation changes, re-key every test that peeks at the
  cache directly — `request_fingerprint()` is visible to the in-file
  test module even though private.
- An edit tool oldString I typed from memory dropped an `.await` from an
  untouched line (`rx.recv()`); rustc caught it, and `git show HEAD:<file>`
  vs `git diff` distinguished my damage from pre-existing residue. Always
  diff-hunk-review after edits near copied patterns; don't trust recall.
- Load flakes on this box remain exactly the documented set: WO 46.28
  `attached_cancel_token_kills_inflight_bash_promptly` + the two edit_file
  30s-edge timeouts. Isolation-green proof + no-fail-fast full run is the
  accepted evidence pattern (matches WO 47.16/46.30/46.34 sessions).
- Cold clippy --all-targets took 19m at load 39; test-fast full
  no-fail-fast pass 473s. Budget gate time accordingly.
## WO 47.26 session (verifier parallel execution)

- rustc Send-inference landmine: an async block OR a closure inside a
  stream combinator's `.map`, living in an async fn's future, made 5
  DOWNSTREAM spawn sites fail with "implementation of `Send` is not
  general enough" (tui/mod.rs, executor_adapter.rs, task_spawner.rs,
  persona.rs) — errors point at callers, not at the offending closure.
  Fix: build the futures in a plain loop into a Vec, then
  `stream::iter(vec).buffer_unordered(n)`. Named-helper async fn alone
  was NOT enough; removing the closure from the future's type was.
- The WO problem statement said verifiers are sync (`BusVerifier`) —
  actually `verify_event` drives the async `Verifier` trait
  (async_trait). Always re-check which of the two coexisting traits a
  site uses before picking a concurrency shape (AGENTS.md §7).
- Machine at load 30-40 from sibling worktree agents: cold `cargo check
  --lib` in a fresh worktree took ~18 min wall; budget gate time
  accordingly and run targeted test filters between items.
## WO 47.29 session (adapter wire fixes)

- `gitnexus detect_changes` MCP runs `git diff` in the MAIN checkout by
  default — for worktree work pass the `worktree` param explicitly (it
  worked: 21 symbols, correct scope). Without it you get a false "No
  changes detected".
- `percent_encoding` has no `percent_encode_str` — the str API is
  `utf8_percent_encode(&str, &AsciiSet)` (`percent_encode_str` is a
  phantom I invented; the crate has `percent_decode_str` for decode only).
- Under sibling-worktree load (12-26), a cold `cargo test -p kf-code --lib`
  takes 15-25 min; a warm incremental one 8-10 min. Piped commands die with
  the tool timeout and lose everything — run detached via
  `setsid bash -c '... > log 2>&1; echo EXIT=$? >> log' &` and poll. Give
  EACH detached run its own log file: two runs sharing one log interleave
  (sparse overwrites) and produce fake-looking errors (an E0425 from run A
  appeared after run B had already fixed the source).
- The anthropic SSE stream tests all live in `anthropic/mod.rs`, not
  `sse.rs` — but `sse.rs` accepts its own `#[cfg(test)] mod tests` fine,
  which is the scope-clean place for them when `mod.rs` is out of scope.

## Session WO 47.27 (memory slug secrets + dupes + LIKE escape)

- **Parallel worktrees starve builds**: sibling WO agents (wo47.28/30/32)
  pushed load to ~18 and a nohup'd rustc died silently (log ended at
  "Compiling", no error — likely OOM). On this box, budget 10-30 min for
  the first `cargo test --lib` in a fresh worktree and retry once the
  load average drops; don't conclude the build is broken while siblings
  are compiling.
- **sqlite LIKE over JSON columns needs two escape layers**: the tags
  column stores the JSON array, so a tag containing `\` lives in the
  column as `\\`. Escaping only the LIKE metachars (\\, %, _) makes the
  query-side `\` literal but still mismatches the stored JSON form —
  build the match pattern from `serde_json::to_string(tag)` first, then
  LIKE-escape. Pre-existing: backslash tags never matched at all.
- **extract.rs byte-slicing at 120/80 is a pre-existing multibyte panic
  risk** (both before and after this WO — not touched, out of scope).
  If a panic report ever mentions extract.rs + char boundary, that's it.
- **The mm-H21 "same fact 14×" is the nested-tail shape**: with pattern
  lists where every match yields `msg[idx..]`, all matches overlap and
  the later ones are suffixes of the earliest. `filter_map(|p|
  lower.find(p)).min()` — keep the earliest match — is the whole fix;
  no normalize/suffix machinery needed.
- **Commit-per-defect with entangled edits**: stage an honest intermediate
  state (defect-1-only), verify, commit, then layer defect 2. Cheaper
  than git add -p surgery through a CLI.

## WO 47.3 session (delete dead JWT half of kf-rbac)

- gitnexus does NOT index crates/kf-rbac at all (cypher: zero File nodes)
  — impact() is a no-op for this crate; the cross-layer grep IS the
  impact analysis for kf-rbac changes.
- scope creep (necessary, disclosed): crates/kf-rbac/Cargo.toml +
  Cargo.lock + crates/kf-budget-core/README.md were not in the WO file
  list — the deps (jsonwebtoken/reqwest/rsa/p256/rand/base64/tokio-dev)
  existed solely for the deleted file, and the budget-core Tests row
  counts #[test] under all of crates/ (941 → 925, confirmed by
  readme_drift). `thiserror` was ALREADY unused in kf-rbac pre-WO —
  removed while editing the same file.
- README Tests-row arithmetic: only `#[test]` lines count — `#[tokio::test]`
  (all 11 async ones in tests/jwt.rs) are invisible to the drift test.
- WO gate "git grep kf_rbac::jwt → zero hits" is self-referential: the WO
  file itself quotes the string, so the honest reading is "zero CODE hits"
  (verified: hits are only docs/workorders/47.3 itself).
- kf-code --lib daemon filter run took ~20 min under sibling-worktree
  contention (wo47.1/wo47.2 compiling concurrently, load 23); the log
  looking frozen at "Compiling similar" was normal — confirm via
  `pgrep -a rustc` out-dir before assuming a hang.
## WO 47.4 session (crate folds)

- Cargo REJECTS `pkg = { package = "other", path = ... }` alongside a direct
  dep on `other` in the same [dependencies]: "depends on crate X multiple
  times with different names". The dependency-rename trick for keeping an
  old crate name alive only works when the target is NOT also a direct dep.
  When it is, the honest mechanical rename of refs is the only path.
- `clippy::module_inception` (-D warnings) fires on a fold that puts
  `routing/routing.rs` inside `pub mod routing` — rename the inner file
  (engine.rs) rather than #[allow]; same cost, no lint debt.
- The `adr_xref_drift` test walks ADR PROSE for `crates/X` literals AND
  `affects-crates:` predicate lists — any crate removal must sweep
  docs/adr/*.md in the same commit, not just TECHNICAL.md.
- Chained sed hazard confirmed the hard way: running
  `s/kf_routing::/crate::routing::/g` then `s/\bcrate::/crate::memory::/g`
  on the SAME file double-transforms the first sed's output into
  `crate::memory::routing`. Verify chained seds with a negative grep
  (`memory::routing|routing::routing`) immediately after.
- kf-budget-core tests are the cheap always-runnable gate (~30s warm) while
  workspace checks take minutes — run the drift test FIRST after any
  doc/status/crate-structure change to catch reference rot early.
## WO 47.2 session (generic env/config override loader)

- The "serde flatten+default broken (#2230)" claim in config/mod.rs was
  STALE: with container-level `#[serde(default)]` on every sub-struct,
  `toml::from_str::<Config>`/`Value::try_into::<Config>` handles partial
  flat files, unknown keys (dropped), the compaction alias, and partial
  sub-tables. Always re-verify "known broken" claims empirically —
  /tmp/opencode scratch crates settle them in minutes.
- serde flatten gotcha 1: inserting an ALIAS key while the serialized
  base holds the primary is a duplicate-field decode error (silently
  skipped by per-key isolation). Swap alias→primary at insert time.
- serde flatten gotcha 2: a field name declared in TWO flattened structs
  (memory_auto_populate in ToolConfig + DisplayConfig) is hidden from
  the struct AFTER a custom-Deserialize sibling (SessionConfig sits
  between them), AND serialization writes the LAST struct's copy —
  round-trips smear the value. Only stable state is both equal → mirror
  the incoming value onto both fields outside the overlay.
- Per-key overlay (serialize→insert one key→decode→assign, skip on
  error) reproduces the old if-let "bad value ignored, siblings keep"
  semantics exactly, and deep-inserting sub-tables preserves
  earlier-layer siblings. Cost is irrelevant (once per startup).
- When a custom-syntax env var must REJECT values (task_concurrency_mode
  queue|reject), returning None from the custom parser is NOT enough —
  the generic type-guided coercion happily applies any non-empty string
  to a String field. Vars with validate/replace semantics must be
  excluded from the generic loop entirely and applied post-overlay.
- Empty-string env semantics differ per field class and must be encoded
  deliberately: plain String fields skip empty; Option paths clear to
  None ("empty disables"); sandbox_dir/cache_dir keep Some("") (escape
  hatch). KF_CODE_SANDBOX_DIR="" was silently re-sandboxed to cwd before
  (contradicting the WO 28.1 documented opt-out) — now pinned as Some("").
- Literal-count tripwires see `"KF_CODE_` in strip_prefix calls and
  KEY_MAP comments — count what's actually in the file with grep before
  pinning the expected number; KEY_MAP has 19 KF entries (computer_use
  has 8 keys incl. chrome_path, not 7).
- scope creep: src/session/config/mod.rs (drift-guard test rewrite —
  literal counting meaningless vs generic loader; stale serde comment;
  +6 pinning tests) and src/shared/config/mod.rs (comment-only —
  CONFIG_FIELD_COUNT doc still instructed per-field loader edits).
- Gate timing under sibling load: cold lib check 12m, workspace
  all-targets check 7m, clippy 3m, config test filter 3-14m per pass.
## WO 47.6 session (compression layers 6→2, smallest safe consolidation)

- The WO's "~2.5K lines, 6→2 pipelines" ask is a strategy-enum rewrite
  across prompt/ + executor loop_.rs + TUI compact.rs — the latter two
  outside the WO's own file list. Per the worker-prompt CAUTION the
  safe scope was deduplicating the shared machinery (stub marker,
  stub shape, anchor split, estimator delegates) — byte-identical
  outputs, zero test churn beyond deleting one vacuous duplicate.
- The "kept in sync" comment on compaction::TOOL_RESULT_STUB was the
  tell for the duplicate: whenever a comment documents a manual sync
  obligation between two constants, that's the consolidation target.
- microcompaction's `use_llm=true` path calls
  `deterministic_compaction_summary` — NOT an LLM; the real LLM arm is
  summarizer.rs, reachable only from the executor /compact handler.
  Any future strategy-enum consolidation must respect that
  request-build time is sync (no adapter) while /compact time is async.
- sed over a test file renamed a fn by pattern and corrupted a
  `fn test_estimate_token_count()` header into
  `fn test_crate::session::prompt::estimate_tokens()` — path-qualified
  call replacement must not touch declaration lines; grep the result
  before compiling.
- Sibling-agent load (16 rustc procs, load 20): lib check 10m21s,
  workspace check 7m17s, clippy 10m12s, prompt tests 146/146 in 34s
  once warm. Detached setsid + poll remains the only sane pattern.
- scope creep: none. 4 files, all in the WO's named scope; state.md/
  CHANGELOG left to the coordinator per parallel-wave convention.
## WO 47.7 (MCP transport trait)

- The WO's "~180 lines" estimate was directionally right: net −310
  (927 del / 617 ins across mod.rs + http.rs), because the refactor also
  collapsed the duplicated initialize handshake and let ~130 lines of
  http-only tests for the simpler content-block helper go (unified on
  the richer stdio helper).
- dyn-compatible async trait without async-trait: one `type TransportFut<'a,
  T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>` alias + explicit `'a`
  lifetimes on the two async trait methods; impls are `Box::pin(async move
  { ...body... })`. Two pages of boxing boilerplate total — cheaper than a
  new dependency in a size-optimized binary.
- Gotcha: an inherent method called as `http::connect(...)` must stay an
  inherent method — moving connect's return type did not move its
  namespace. `http::McpHttpTransport::connect` is the correct path.
- A trait method used only by one test (`pending()`) trips dead_code in
  non-test builds — `#[cfg(test)]` on the trait method + both impls, same
  pattern as the test-only `disconnect`.
- First `cargo nextest --lib` run after the edit timed out at 15 min —
  cold test-binary build under load-14 sibling-worktree contention
  (known from WO 46.28 lessons). The retry with the warm fingerprint
  finished the whole gate in minutes. Budget the first build.
## WO 47.9 session (archive workorder corpus)

- The WO drift test needed NO code change: it scans docs/workorders/
  non-recursively against that dir's README index, so moving pre-46 files
  out AND trimming the index to 46/47 keeps it green — layout and scan
  path stay in agreement by construction. Index rows whose files are gone
  are silently skipped, so trim-then-move (or move-without-trim) would
  have left a lying index even while green; do both in one commit.
- README index had merge-cruft duplicate rows (47.18/19 x4, 47.24/25/26
  x3, 47.27/28 x3) — an awk first-occurrence dedupe on the row key
  ($2 splitting on '|') collapsed them in place.
- Scope creep (disclosed): src/shared/sandbox_policy.rs,
  crates/kf-plugin-sdk/src/lib.rs, tests/e2e/harness/mod.rs doc-comments,
  docs/TECHNICAL.md, and state.md live links repointed at
  docs/archive/workorders/ — grep proved these were the only non-history
  references to moved files (CHANGELOG's 120 historical links left
  as-is, disclosed as deferred).
- git mv of 450 files in one xargs-less `git mv $(ls ...)` worked fine;
  `git status` shows them as renames (R), preserving history.
## WO 47.10 session (send_or_warn! ceremony → emit! macro)

- The WO's "47 sites" counted only the ASYNC emission sites
  (`event_tx.send(TurnEvent::…).await`); actual 46 (turn 24, outcome 8,
  dispatch 5, loop_ 3, model 6). The 29 sync `send_or_warn!` sites
  (unbounded/oneshot, no `.await`) are already 1-3-liners — no ceremony
  to collapse, correctly out of scope. One macro cannot span both:
  `.await` on a `Result` won't compile, macro_rules can't branch on
  awaitability. Don't try a trait-based unification — not worth it for
  3 saved lines.
- For a mechanical 46-site rewrite, a 2-regex Python script (single-line
  vs multi-line `.send(` arg, anchored on `)\n .await,\n "msg"`) beat
  hand-editing: zero misses, zero typos, whitespace normalized by
  `cargo fmt` afterwards. The anchor `\)\n\s*\.await,` is safe because
  no event payload contains `.await,`.
- regex-rewritten sites come out with ugly spacing (`"…" .into()`);
  `cargo fmt` fixes all of it — don't hand-fix before fmt.
- Pre-fmt diff (−299 lines) overstates the win: fmt re-wraps long macro
  args, honest net was −100 lines for 46 sites (~2.2/site — the WO's
  ~175-line estimate assumed more single-line sites than exist).
- Setsid-detached gate runs MUST redirect the outer process too
  (`setsid nohup bash -c '… > log 2>&1' > /dev/null 2>&1 < /dev/null &`)
  — an unredirected setsid holds the bash tool's stdout pipe and burns
  the full 120s timeout on a 5-second launcher.
- Scope creep: none. Files touched exactly as WO names + the two status
  docs the worker prompt requires.
## WO 47.12 session (daemon opt-in)

- The WO as written (default-off cargo feature) was under-scoped by its own
  file list: gating src/daemon/** drags in lib.rs, cli.rs, run_session.rs,
  tui/daemon_events.rs, tui/commands/fork.rs, and jobs/* (jobd reuses the
  daemon Request/Response/daemonize/check_auth_ct helpers). The
  worker-prompt caution pre-authorized the runtime fallback; the lazy shape
  was one env gate (`KF_CODE_DAEMON_AUTOSTART`) + disk fallback inside the
  two auto-starting try_* helpers — zero consumer-file churn.
- Auto-start lived ONLY in `try_list_recent` + `try_resolve_id`
  (`ensure_daemon_running`); `try_touch`, `try_notify_jobs_changed`, and the
  TUI instance channel already degrade gracefully with no daemon. Knowing
  exactly which 2 of 6 helpers spawn is what made the 2-file diff possible.
- `session_index::summarize_file` derives `started_at` from file MTIME for
  empty logs — tests needing deterministic newest-first ordering can use
  `set_modified` with 2s steps (same trick as daemon/mod.rs
  `recent_list_is_capped`).
- Env races in tests: a new env knob's parse test and its behavior tests
  must serialize behind `test_data_dir_lock()` (shared with other
  data-dir-sensitive daemon tests), else the parse test flipping the var
  can push a fallback test down the autostart path (2s spawn wait of the
  TEST binary). EnvGuard::remove exists for the unset case.
- scope creep (disclosed): src/cli.rs `--attach`/`--auto-resume` help text
  (2 lines) — behavior this WO changed; honest-docs. TECHNICAL.md via the
  doc-sync rule. Cargo.toml + cli_dispatch.rs were WO-named but needed no
  change (no deps; dispatch untouched).
- Fresh-worktree gate costs at load ~4: cargo check -p kf-code 5m15s,
  nextest lib test-build 13m, clippy 6m23s. Detached setsid+log+poll
  pattern per prior lessons.
## WO 47.13 session (TUI command diet, gating)

- Coordinator directive "cut or gate, pick the reversible one" flipped
  the WO from line-trimming to config gating. Config flag (runtime)
  beats default-off cargo feature here: feature-gated code is never
  compiled by default gates/CI -> guaranteed rot; config-gated code
  stays compiled + tested, reversible per user, live via /reload.
- The WO's "doom banner" trim item was mislabeled — it is the
  doom-loop runaway WARNING interrupt (safety UI, WO 43.31
  regression-pinned), not a cosmetic banner. Left ungated + disclosed.
  Re-verify what a trim item actually IS before gating it.
- Persona module cannot be gated as a unit: /workflow dispatch and the
  doom-banner Plan action route through persona_tx/PersonaResult. Only
  the /plan /explore /coder entry commands are gated; /implement stays
  (plan-mode exit).
- `complete_command`/`help_text` now take `&[String]` extras, NOT
  &Config: complete_slash mutates state after the call, and a held
  RwLockReadGuard borrow would conflict. Clone the Vec out of the lock
  first (empty by default — cheap).
- `for (trigger, _) in EXTRA_KEYS` binds trigger as &&str (slice-of-
  tuples iteration), while `for t in ["a","b"]` binds &str — Vec::<&str>
  ::contains needs &&str in both cases. One E0308 taught it twice.
- Editing test bodies by replacing whole functions left stale duplicate
  closing braces once (rustc "unexpected closing delimiter" pointed at
  the NEXT test). Diff-review each test edit before compiling.
- scope note: gate plumbing necessarily touched files beyond the WO's
  `src/tui/commands/**, src/tui/widgets/**` list — dispatch/help/
  completion live in tui/keys/, config plumbing in shared/config +
  session/config. Disclosed in WO Done. Line-mode /carryover (src/main/
  line_mode.rs) left ungated — out of scope, disclosed as follow-up.
## WO 47.14 session (verifier trait unification, step 1: plugins bus-only)

- Coordinator premise drift again: "WO 47.1 already merged" was false on
  this branch (WO file + README row said Planned, init_default_verifiers
  still had the 14x boilerplate, no wo47.1 merge in git log). Always verify
  claimed-merged prerequisite WOs against the worktree's own git log before
  building on them.
- The plugin-verifier dual registration (slots adapter + bus) meant every
  plugin verifier subprocess ran TWICE per file-modifying tool call and a
  failing one produced two CorrectionResults. Deleting the legacy adapter
  (`PluginVerifierAdapter` + `verifiers_from_registry` +
  `rebuild_plugin_verifiers` + BUILTIN_VERIFIERS allowlist) was the
  correct first consumer migration onto the surviving `BusVerifier` trait.
- Deleting `rebuild_plugin_verifiers` also structurally eliminated the
  WO 44.29 allowlist-drift hazard — the regression test guarding it was
  reworked to drive `reload_plugins` instead (kept the invariant, lost the
  dead allowlist).
- Security-pin ports: when deleting a path that carried a security
  regression test (env-leak), re-land the pin on the surviving path —
  `add_plugin_verifier_does_not_leak_session_env` now covers the bus path.
- `TsOrchestratorBridgeVerifier` has NO production registration site —
  TECHNICAL.md claimed "built-in verifiers register directly" on the bus,
  which was false (bus starts empty in production). Fixed in the same
  doc-sync commit; it's the intended landing spot for the built-in
  migration (WO 47.14 remaining step 3).
- The edit tool fuzzy-matched an oldString containing a typo I introduced
  (`slots.write().unwrap().unwrap_or_else` vs actual
  `slots.write().unwrap_or_else`) and still applied correctly — ALWAYS
  re-read the edited region after a large edit; a fuzzy match could just as
  easily have landed somewhere unintended.
- Cold `cargo check -p kf-code --lib` in this worktree: 7m40s at load ~7.
  Full workspace check after (warm): 5m23s. Background setsid + log-file
  polling is mandatory; the 120s tool timeout kills foreground cargo.
- scope creep: src/session/executor/mod.rs + executor/tests/verifier_cross.rs
  + docs/TECHNICAL.md beyond the WO's "src/session/verifier/**, AGENTS.md"
  file list — the consumer registration sites live in the executor;
  doc-sync rule 9 (verifier bus + plugin system) requires TECHNICAL.md.
  AGENTS.md NOT touched (coordinator: only when the old trait is fully
  deleted).

## Session 2026-08-28 — stability cleanup (flake stab + state truth pass)

- **Wave-merging lesson: README-row sync must be wholesale-scripted.**
  Merging a wave of workorders by hand-editing README index rows per-WO
  drifts — the WO 47 final-wave merge missed the 47.1-47.5 rows (fixed
  afterward in 33137100846 "dead merger missed them"). The reliable shape:
  one script/one pass that regenerates every row from the WO file headers
  (the same source `adr_xref_drift` checks), applied at wave close — never
  incremental per-WO edits across parallel merges.
- **Wave-merging lesson: keep-both conflict resolution loses source intent
  on rewritten files — take theirs.** When a wave rewrites a file (state.md
  Shipped section, README index) and a sibling branch also touched it,
  "keep both sides" produces a document that is neither: stale blobs from
  the pre-wave shape survive next to the new truth. For files a wave
  deliberately REWROTE, resolve with "take theirs" (the wave's version);
  keep-both is only for append-only files (lessons.md itself, CHANGELOG).
  Root cause of the stale state.md this session had to truth-pass.
- Test-flake triage: nextest per-test overrides in .config/nextest.toml
  (`[[profile.ci-fast.overrides]]`) are the cheap, centralized fix for
  wall-clock-budget flakes — but they only help where the in-test bound
  isn't the binding constraint. For each flake, find WHICH deadline fires
  first (in-test assert vs profile slow-timeout) and raise that one; the
  other needs matching headroom or nextest kills the test before its own
  diagnostic assert can report.
- gitnexus impact does not index #[cfg(test)]/#[tokio::test] symbols —
  for test-only edits, "no production callers possible" is the impact
  answer; don't burn time on re-indexing.

## WO 48.2 session (2026-08-28)

- Phase-3 pre-tool hook re-run existed because the resolved path was only
  substituted into hook args at record time; Phase 1 computes
  `file_resolved` BEFORE its hook fires, so the substitution moved there
  and the Phase-3 run was pure duplication. Lesson: when a "later phase
  has better data" gate is found, check whether the earlier phase already
  holds that data — the WO 43.30 fix (hook moved to Phase 1) left the old
  Phase-3 site behind as a stale sibling.
- Parallel worktree sessions make cold cargo builds take 10-15+ min (3+
  concurrent rustc sets). Budget 30-45 min timeouts for check/test/clippy
  in woXX worktrees; the 5-min default will kill legitimate builds.
- gitnexus detect_changes can't see .worktrees/woXX diffs (index tracks
  the main checkout) — use `git status --short` + `git diff --stat` in the
  worktree for scope verification.
- `printf x >> {path:?}` in a bash hook script is the cheapest invocation
  counter for "hook ran exactly N times" tests — file length = count, no
  flock needed for single-invocation assertions.
## WO 48.3 session (2026-08-28)

- The 47.12 regression mechanism: `open_overlay`-family tab switches prime
  cold data via dirty FLAGS consumed by the draw loop — the fix point for
  "tab X empty" bugs is the flag trip, not the fetch (the fetch already had
  the disk fallback). There were THREE sibling switch sites (open_overlay,
  palette Enter Overlay arm, F-key arm), all duplicating the Jobs-only
  priming; extracted `prime_overlay_cold_data` so Jobs/Sessions can't
  diverge again. When adding a new cold-primed overlay, extend that one
  helper — do not inline another `*_dirty` trip at a switch site.
- Startup picker path was never broken (run_session.rs routes through
  `try_list_recent` which has the disk fallback) — only the in-TUI tab was.
  Auditing the actual data path first shrank this WO to a flag trip.
- gitnexus index is main-checkout-scoped: worktree edits made `impact`
  return "not found". Manual grep impact (callers of the flag consumers)
  is the fallback; risk was LOW and stayed LOW.
- `cargo test -p kf-code --lib <filter>` needs ~19 min on this machine for
  the first test-profile link — budget accordingly, then filtered reruns
  are seconds.
## WO 48.4 session (2026-08-28) — session-picker modal at startup

- **`SessionState::session_picker` was a dual-purpose field and that was
  the whole bug**: one `Option<SessionPicker>` served as BOTH the data
  source for the Sessions tab / welcome screen AND the modal-open flag
  consumed by render + key capture. Any writer of "data" implicitly
  opened a full-screen modal. Fix pattern worth reusing: when an
  `Option<T>` is read by both data consumers and presentation consumers,
  split into `data: Vec<Entry>` + `modal: Option<T>` — the data field
  can't render itself.
- **The `take()?`-without-restore bug class in key handlers**:
  `handle_session_picker_keys` took the picker, returned `Some` only on
  confirm/cancel, and dropped it on every other key — advertised nav
  silently closed the modal. Any handler that does
  `let x = state.field.take()?` must restore `x` on every path where the
  interaction continues. Grep for `take()?` in keys/ before adding new
  modal handlers.
- **TUI key handlers self-dirty**: the event loop does NOT mark dirty
  after dispatching a key — every handler that changes visible state
  must call `state.mark_dirty()` itself. The old picker code got away
  with it because its bug made the modal close via other handlers.
- Cold-worktree cost: first `cargo check -p kf-code --all-targets` in a
  fresh worktree is ~14 min (deps), first lib-test link another ~15 min.
  Budget gates accordingly or reuse the main checkout's target dir.

## WO 48.9 session (2026-08-28)

- `#[cfg(...)]` on a trailing expression (e.g. a cfg'd `return` followed by a
  cfg'd tail expr) is E0658 unstable. Pattern that works: cfg'd `{ }` blocks
  as statements — with one cfg'd out, the survivor is the tail expression.
- `budget = ["dep:kf-budget-core"]` means --no-default-features removes the
  crate entirely; the right gate shape for a widely-called helper is a cfg
  pair INSIDE one choke-point fn, not gating every caller.
- The full check/clippy gates on this worktree are COLD (own target dir):
  check --no-default-features ~11min first run (timed out at 10min), check
  --all-targets ~9min, clippy -p kf-code ~21min, test-profile build ~10min.
  Budget ~1h for the fast-gate set from scratch.
- docs/workorders/README.md has pre-existing duplicate rows (48.2 ×3,
  48.3 ×3, 48.4 ×3) from parallel-worktree merges — predicted by the AGENTS.md
  lessons entry. Left alone (out of scope); merge-resolve by keeping one.
- scope creep: none.
## WO 48.10 session (2026-08-28)

- The 47.12 disk fallback was only ever wired into `unix_imp`; the
  `windows_imp` sibling kept `Ok(None)` stubs. Platform-split modules
  (`cfg(unix)` / `cfg(windows)` twins) are another instance of the
  duplicated-pattern divergence theme — when a fix lands in one arm,
  grep the other arm.
- The fix was genuinely just "let Windows call the same code": the whole
  session-index path (`session_index::list_sessions`,
  `resolve_session_id`, `RECENT_SESSIONS_LIMIT`, `test_data_dir_lock`,
  `EnvGuard`) is platform-neutral std-fs. The non-unix `DaemonState`
  already scanned disk the same way.
- gitnexus `detect_changes` without the `worktree` param diffs the MAIN
  checkout and reports "no changes" — always pass
  `worktree: <worktree-path>` when working in `.worktrees/woXX`.
- gitnexus impact on a symbol implemented per-platform reports the
  Unix-side blast radius (the Linux symbol graph). cfg(windows)-only
  body edits with unchanged signatures show as 0 changed indexed
  symbols — expected, not a tool failure.
- Fresh worktree = cold target dir: `cargo test` took >10 min,
  Windows cross clippy 25 min, workspace check 8 min. Budget long
  timeouts (or pre-warm with a plain `cargo check` first).
- `cargo fmt --check` on a cold worktree can exceed 60s; it is not
  instant until the toolchain metadata is warm.
- WO status flip is two-source-of-truth like ADRs: WO file `## Status`
  AND docs/workorders/README.md row must agree
  (adr_xref_drift.rs WO-status test enforces it).

## WO 48.12 session (2026-08-28, branch wo48.12)
- The minify→expand write-back chain has TWO regex-blind scanners, not
  one: `minify_js_like` (lang.rs) AND `fallback_c_like` (expand.rs). A
  fix in only the minifier still ships corruption — the expander's `:`
  arm inserts a space inside regex bodies. When fixing a minifier bug,
  grep expand.rs for the same language in the same commit; the
  round-trip test (minify → wrap_minified_envelope → expand_minified)
  is what catches it, not a minify-only assert.
- This box has no prettier/deno, so `expand_minified` for js always
  exercises `fallback_c_like` in tests — good: the fallback is the
  corruption-prone path.
- `//` and `/*` must WIN over regex-open even at regex position
  (`x = // c` is a comment; a JS regex body can't start with `/` or
  `*`) — checking the prev-token heuristic only for `/` followed by
  something else keeps comment stripping intact.
- scope creep disclosed: expand.rs touched though WO named only
  lang.rs — required by the WO's own round-trip gate (sibling site,
  same root cause).
## WO 48.16 session (2026-08-28)

- Both `mark_read` sites already had the tool outcome in scope — the WO's
  "dispatch may mark before the body runs" worry didn't materialize; no
  48.2-style timing move was needed, just `tool_outcome_success` in the
  condition. Read the site before planning a move.
- `ToolDef.name`/`description` are `&'static str` (not String) — `.into()`
  on literals in test ToolDefs trips `clippy::useless_conversion`.
- Contended box: sibling worktrees running parallel cargo builds made a
  cold `cargo test` exceed 30 min; workspace check alone took 12 min.
  Budget long timeouts, and prefer `--lib <filter>` first for signal.
- gitnexus detect_changes/MCP tools index the MAIN checkout — worktree
  diffs show as 0 changed symbols (same as WO 48.10). `git diff --stat`
  is the honest scope evidence in a worktree.
- WO status flip reminder (confirmed again): WO file `## Status` AND
  docs/workorders/README.md row — both updated for 48.16.
## WO 48.17 session (2026-08-28)

- `PathGuard::check_write` returns the RAW literal as its "resolved"
  path (`GuardVerdict::Allowed(path.to_path_buf())`) — the sandbox-branch
  `canonicalize` is only used for containment comparison. `check_read` →
  `check_traversal` DOES return the canonical path. So "Phase-1 resolved
  path" means canonical for read tools but identity for write tools
  (write_file/edit_file/notebook_edit): the pre_run hook substitution is
  a no-op for writes, and the Phase-2.5 symlink walk therefore denies ANY
  symlinked parent component on a write path (walk stats the raw path's
  prefixes). Test-writing consequence: you cannot demonstrate
  "hook sees canonical ≠ raw" for a write tool — the resolved==raw.
  Fixing check_write's return is a cross-tool behavior change (candidate
  WO, noted in 48.17's Done section).
- The Phase-2.5 deferral, symlink walk, and body-opens-resolved-path are
  ALL driven by `resolved_path.is_some()` — one pre_run list entry
  activates the whole chain. The mirrors that need manual sync: dispatch
  `needs_read_gate`, dispatch AccessDenied audit list, turn.rs
  `should_audit` + file-tool arm + defensive read gate, pre_run
  `is_destructive`.
- WO sweep-audit line numbers drift: WO 48.17 cited "src/tools/mod.rs:245"
  but the registry line is 217. Trust grep, not the WO's line refs.
- Scope creep avoided: helpers/mod.rs `check_deny_list` file-tool arm
  still lists only the 4 siblings, but `PathGuard::check_write` itself
  enforces `deny_list.is_path_denied` — the helpers arm is a
  pre-approval duplicate, so notebook_edit deny-list coverage is already
  closed via the pre_run listing. No edit needed.

## WO 48.25 session (2026-08-29, branch wo/wo48.25)

- Worktrees created from pre-sweep commits don't carry the WO file/README row
  for their own task — copy both from the main checkout, then flip Status.
- Under parallel WO load (3+ simultaneous cold worktree builds), a plain
  `cargo test` can blow a 20-min timeout with zero output. Run gates via
  nohup + log file and poll; incremental artifacts survive the kills.
- The gitnexus index doesn't cover private fns like `shell_heredoc_opens`;
  impact on the enclosing public entry (`minify_content_by_ext`) reports the
  hub's CRITICAL fan-out, which is breadth, not change risk, for scanner-only
  edits. detect_changes with the `worktree` param works from the main index.

## WO 48.32 session (2026-08-29, branch wo/wo48.32)

- The workflow→TaskSpawner cancel bridge mirrors task_tool.rs's foreground
  path exactly: derive `TaskCancel{flag, token}` + `cascade_parent_cancel`
  + `done.notify_waiters()` after the await. The cascade fn was private to
  `tools::task`; one `pub(crate)` widened it — no new abstraction needed.
- `run_task_error_return_still_cleans_temp_dir` (task_spawner tests) flaked
  once under parallel cold-build load (real-time retry backoff + shared temp
  namespace scan); 3 consecutive green reruns. Not related to a 1-word
  visibility change — pre-existing wall-clock sensitivity, watch under load.
- Cold-worktree `cargo check -p kf-code --lib` alone is ~15 min under
  parallel WO load; `cargo test` codegen adds more. Budget 40-min timeouts.
