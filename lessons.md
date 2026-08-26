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
