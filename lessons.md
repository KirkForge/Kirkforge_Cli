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

## WO 43.15 — machine-greppable ADR predicate blocks

- The `implemented` vs compound-status cross-check was the trickiest part.
  "Accepted (amended)" does NOT mean partially implemented — an amendment
  replaces the original decision and is fully in force. Map "amended" →
  true, "partially implemented" → partial, "fully implemented" → true,
  "Superseded" → false. 004 and 054 both carry amendments but are fully
  implemented as amended; only 0044/073 are genuinely partial.
- `supersedes` is what THIS ADR replaces, not what replaces THIS ADR.
  048/049 are *superseded by* a removal decision, not by a named ADR, so
  their `supersedes: []`. Don't invert the direction.
- `crates/kf-budget-core/README.md` `| Tests | N passing |` was stale by
  32 on origin/dev (882 claimed, 914 actual) — `readme_drift` test was
  ALREADY red before my change. Bumped to 914 (accurate incl. my +1 test).
  This was pre-existing breakage, not mine, but leaving it red while
  adding a test would've made drift worse. AGENTS.md says bump the count
  when adding tests to crates/ — doing so also fixed the pre-existing
  red. Worth grepping `readme_drift` state before assuming a clean tree.
- The drift test build is slow (~18s) but the full `cargo clippy --all-
  targets` is 5-6 min even cached, and competes with other worktrees'
  builds for CPU. Budget 15+ min for the full gate when wo43.19 is active.
- `cargo fmt` reformatted my multi-line `panic!` into one line and rewrapped
  a long `unwrap_or_else` — run fmt before clippy, not after, or clippy
  passes then fmt fails CI.
