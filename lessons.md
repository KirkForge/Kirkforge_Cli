# Lessons — WO 15.14 session (split plugin3-cli/src/main.rs by feature)

## What I learned about this codebase
- `crates/plugin3-cli/src/main.rs` was a 7,706-line monolith: ~550
  lines of production (CLI def + `main()` + `self_check` + shared
  helpers) + ~7,150 lines of 4 inline `#[cfg(test)]` modules. The
  production helpers were already partially split (`commands/`,
  `hooks/`, `exit.rs`, `json_out.rs`, `precedence.rs`) per ADR-0002,
  but the budget/recent/stdin helpers and ALL tests stayed inline.
  ADR-0002 § Crate layout is the authority for the split — reference
  it in the new module headers.
- The 4 inline test modules (`tests`, `validate_tests`,
  `adr_0015_validate_tests`, `recent_outputs_tests`) are each
  self-contained: own helpers, own `use super::*;`, NO cross-test-
  module calls (verified by grep). Each module's `super` = crate
  root (main.rs). Moving them to sibling files declared as
  `#[cfg(test)] mod tests_x;` in main.rs preserves `use super::*;`
  semantics exactly — DO NOT nest them under a `tests/` mod (that
  changes `super` to the `tests` mod and breaks every `super::*`
  reference). This is the cleanest pattern for splitting inline
  `#[cfg(test)] mod` blocks out of a bin root.
- `ponytail:` annotations are load-bearing and must move verbatim.
  When extracting test module bodies via `sed`, the `// ponytail:`
  HEADER comments above each `mod xxx {` line are NOT captured (they
  live between `}` and `mod`, outside the body range). I dropped two
  (`ADR-0014 § Recent outputs file — pins the` above
  `recent_outputs_tests`, `ADR-0015 § Exit codes — exercises the
  binary` above `adr_0015_validate_tests`) and caught it only via a
  whitespace-normalized `comm -23` diff of ponytail lines before vs
  after. ALWAYS run that diff after a test-module extraction: 
  `git show HEAD:file | grep 'ponytail:' | sed 's/^[[:space:]]*//' | sort -u`
  vs the same across the new files. The raw count lies (rustfmt
  re-indents, so un-normalized diff shows every line as
  changed/missing).
- `pub(crate) use` re-exports that are ONLY consumed by `#[cfg(test)]`
  modules (via `super::*`) trigger `unused_imports` in non-test
  builds under `-D warnings`. Fix: gate those re-exports with
  `#[cfg(test)]`. Same for crate-root `use` imports that exist only
  so test `super::*` can reach them (e.g. `BudgetConfig`,
  `ConfigFile`, `Paths`, `UsageRecord`). The split: production
  re-exports un-gated; test-only re-exports `#[cfg(test)]`-gated.
  `plugin3_binary_path` was already `#[cfg(test)]` in its module.
- A `use` import that is ONLY referenced inside string literals
  (assertion messages) is genuinely unused — `VecDeque` appeared in
  4 test assertion strings but never as a type after the
  `RecentEntry`/`load_recent_outputs` code moved to `recent.rs`
  (which has its own `use std::collections::VecDeque`). Drop it
  rather than gating it.
- `SlicingTransform` is imported in main.rs but never named anywhere
  in the crate (only `HeadTailSlicer` is used in `self_check`).
  rustc does NOT flag it `unused_imports` — empirically, a name in a
  grouped `use` whose sibling is used is not flagged even if the
  name itself is unreferenced. Don't "fix" it by removing it; the
  baseline clippy is green with it present.

## What I tried that didn't work and why
- Naive Python brace-counting to find test-module end lines: fails
  because `{`/`}` appear in string literals and char literals inside
  the test bodies (the count never returns to 0). Reliable approach:
  top-level test modules close with a `}` at column 0 alone on a
  line — `grep -n '^}$'` + pair with the nearest `^}$` after each
  `^mod xxx {`. (A full Rust-aware tokenizer that skips
  strings/comments/char-literals also works but is overkill.)
- `git stash -u` + `git stash pop` round-trip to verify a pre-existing
  error: the pop left the working tree in a confusing state (a
  spurious `event.rs` deletion appeared, my untracked new files
  vanished). The work was safe in `stash@{0}` (verified via
  `git stash show -u stash@{0}`) and re-popping after `git checkout
  -- <file>` restored everything. Lesson: before stashing untracked
  work for a quick HEAD comparison, `git stash push -u -m "wo15.14
  wip"` with a label, and verify the pop fully restored with
  `git status` before proceeding. Safer alternative for "is this
  pre-existing": `git show HEAD:<file> | grep <pattern>` avoids
  touching the working tree at all.

## Scope creep I took (documented)
- `src/session/verifier/security.rs`: 6 `FileWriteEvent` test
  construction sites (lines ~560/579/602/625/646/722) were missing
  the `content_hash` field that WO 15.8 added to the struct. This
  was a PRE-EXISTING compile error on clean HEAD (verified:
  `git stash -u && cargo check -p kirkforge --tests` showed the same
  6 `E0063 missing field content_hash` errors without any of my
  changes). It blocked the WO 15.14 workspace gate
  (`cargo clippy --all-targets` + `cargo test --workspace`). Fix was
  mechanical: add `content_hash: 0,` to each, matching the 11
  already-updated sites and the struct doc comment "tests may leave
  it 0." Took the fix to unblock the gate rather than escalate,
  since it's 6 lines, obviously correct, and the root cause
  (incomplete WO 15.8 test update) is unambiguous.

## What I'd do differently next time
- Before extracting test module bodies with `sed <start>,<end>p`,
  also capture the header comment block immediately above each
  `mod xxx {` line (the lines between the previous `^}$` and the
  `#[cfg(test)]`/`mod` lines). Those headers are usually
  `ponytail:` spec pins and MUST move with the module. I'd extract
  them as the file's leading doc comment.
- Run the whitespace-normalized ponytail diff as a gate step in
  workplan.md for any "move code verbatim" refactor — it catches
  dropped comments the raw line count misses.