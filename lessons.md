# lessons.md — WO 28.10 / 28.11 / 28.12 session

## Session 2026-08-12 — ADR drift gate fix (adr-fix worktree)

### Scope creep (disclosed): Cargo.toml + Cargo.lock
- Task was scoped "docs only" but the workspace didn't parse: WO 29.7 merge
  `7a0de4d` re-introduced committed conflict markers in `Cargo.toml`
  (workspace.dep `kf-orchestrator`) + `Cargo.lock` (`thiserror` 2.0.19↔2.0.20).
  The gate (`cargo test ... adr_xref_drift`) was literally unrunnable.
- This is the **same regression class** the prior session fixed for WO 29.6
  (see "Precondition" below). WO 29.x merge series keeps doing this.
- Fix was unambiguous (kept `kf-orchestrator` dep — crate exists; took
  `thiserror 2.0.20`). Decision: unblock + disclose prominently rather than
  escalate, because (a) gate is a hard success criterion, (b) prior session
  set the precedent, (c) AGENTS.md §7/§11 anticipate + bless disclosed scope
  creep. Worked.
- **Lesson:** before any WO merge is committed, run
  `grep -rln '^<<<<<<<\|^=======\|^>>>>>>>\|^|||||||' Cargo.toml Cargo.lock`.
  This repo needs a pre-commit/pre-merge hook rejecting these. The `ci.yml:32`
  fmt job catches it on push but not on local commit/merge.

### The workorder's premise was stale
- Task claimed `adr_xref_drift::status_counts_match_index_table_summary` is RED
  on ADR-054 drift. It wasn't — already fixed in a prior merge; header + README
  row byte-identical. **Stale-cleanup-item risk again** (AGENTS.md §7): trust
  but verify. Thirty seconds of running the test beats an hour of "fixing" a
  non-existent bug.

## What I learned

### Precondition: merge commits with conflict markers
- The worktree HEAD `5a6c32d` ("merge: WO 29.6") had **three files with
  unresolved git conflict markers actually committed**: `Cargo.toml`,
  `Cargo.lock` (three regions), `docs/TECHNICAL.md` (two regions). The
  existing `ci.yml` fmt job at `:32` does `git grep` for conflict markers
  and would have rejected this commit on push. **Always grep for conflict
  markers as the first precondition step** before starting any WO work:
  `grep -rln '<<<<<<<\|>>>>>>>\|^|||||||' --include='*.toml' --include='*.lock' --include='*.md' .`
  Excluding the regex itself in `ci.yml:32`, the prose in `docs/adr/012`,
  and the test data in `src/session/git_sanitation.rs`.
- Cargo.lock conflicts are mergeable by hand when both sides add different
  packages — keep both, preserve exact versions, maintain alphabetical
  order. Deleting + regenerating would risk version drift and breaks
  `--locked` CI.

### WO 28.10 — feature-gating a `[[test]]` target
- `required-features = ["..."]` on a `[[test]]` block is the one-line knob.
  No need to move test files or set `harness = false`.
- `cargo check --workspace --all-targets` (no feature) is green; `cargo
  test --test <name>` (no feature) errors cleanly with
  `target '<name>' in package '<pkg>' requires the features: '<feat>'`.
- **Critical follow-up**: any CI job that lints or typechecks with
  `--all-targets` must pass `--features <feat>` or the gated crate stops
  being linted entirely. The failure mode is silent rot, not a red gate.
  Wired into both the `quality` job's Clippy + typecheck steps.

### WO 28.11 — bench schema validation
- The worker task brief listed `("name", "difficulty", "language")` as
  the required-key set. **Zero tasks have a `language` key.** Reality is
  `("name", "difficulty", "prompt")` per ADR-038 + the WO's own R2
  ("enumerate from an existing well-formed task"). Always grep the actual
  files before pinning a required-key set; the user prompt is a paraphrase,
  the WO R2 reality-check is the spec.
- WO R3 (name-set manifest) was deferrable: the TECHNICAL.md row check
  (R1) catches renames in practice because every task has a row keyed by
  basename. Disclosed per AGENTS.md §11.

### WO 28.12 — dead-ref firewall scope
- The existing check #5 grep (`grep -rn 'plugin3\|...'`) for `scripts/`
  + `.github/` works fine because those file types don't have legitimate
  historical references. But the same regex on `src/`/`crates/` floods:
  test fn names (`fn plugin3_legacy_alias_*`), comments, string literals.
- The fix is **identifier-position regex**, not allowlist:
  `^\s*(use|mod|extern\s+crate)\s+(plugin3|...)\b`. This naturally
  excludes everything that isn't a live import/declaration. Allowlist
  (R2) turned out to be unnecessary — the regex was sufficient.
- `stratum` MUST NOT be in the dead set — it's still a live feature.

## Scope creep / attribution mixing
- **Single-edit bundling**: WO 28.11 and WO 28.12 both add adjacent checks
  (#9 and #10) to `scripts/check-artifact-consistency.sh`. I wrote both
  checks in one edit window before realizing they belonged in separate
  WO commits. Rather than revert + reapply (busywork that risks the
  working verified state), I committed both checks under WO 28.11 and
  made the WO 28.12 commit only the status-flip + disclosure. Disclosed
  in the WO 28.12 status line + commit message. Next time: do one WO's
  edit + commit, then the next WO's edit + commit.

## What I'd do differently
- Before opening the workplan, run a single sweep:
  `git log --merges -1 && grep -rln '<<<<<<<' .` to detect broken merge
  commits up front.
- Write the per-WO commit boundary *before* opening any edit, especially
  when multiple WOs touch the same file.
