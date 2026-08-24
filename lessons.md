# Lessons — WO 43 session

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
