# Lessons — WO 43 session

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
