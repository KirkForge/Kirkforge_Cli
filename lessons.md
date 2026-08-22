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
