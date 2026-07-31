# Lessons — WO 15.10 session (security scanner comments + git_sanitation cap + trufflehog timeout + docker expect)

## What I learned about this codebase
- `cargo test -p kirkforge --lib session::verifier tools` (the WO gate as
  literally written) is NOT valid cargo syntax: `cargo test` accepts
  exactly ONE positional TESTNAME (a substring filter), not a space-
  separated list. The intended gate is two separate invocations:
  `cargo test -p kirkforge --lib session::verifier` and
  `cargo test -p kirkforge --lib tools`. Running them as one command
  errors with `unexpected argument 'tools' found`. Worth noting for
  future WOs that list multiple module filters — split them.
- The `undo.rs` `#[cfg(not(test))] / #[cfg(test)]` const-override pattern
  (production value vs. a smaller test value for the same const) is the
  repo's established way to make a timeout/size cap testable without
  injecting a parameter. Used it for `TRUFFLEHOG_TIMEOUT_SECS`
  (60s prod / 2s test) so the timeout test runs in ~2s instead of ~60s.
  This is preferable to a `tokio::time::timeout` outer test budget that
  would have to be >60s to prove the inner timeout fires.
- `src/session/verifier/security.rs` dangerous-shell-pattern scan
  (`DANGEROUS_SHELL_PATTERNS`) is a substring `content.contains(pattern)`
  over the whole file. The entropy scan and secret-substring scan
  (`SECRET_PATTERNS`) ALSO run over the whole file — but per the WO,
  only the shell-pattern check needed comment-skipping (comments
  documenting `rm -rf /` were the false-positive source; secret
  patterns in comments are still real secrets the user should see).
  Scope discipline: I only added `is_comment_line` filtering to the
  shell-pattern loop (step 4), NOT to the entropy/secret-substring
  scans (steps 1-2), to match the WO's "skip comment lines for
  shell-pattern checks" wording exactly.
- `Bash::run_docker` is a private method on `Bash` (`src/tools/bash.rs`).
  Its test module uses `use super::*;` so the test can call
  `tool.run_docker(...)` directly without a `pub` change. This is the
  pattern for testing private methods in this repo: keep the test in the
  same module, rely on `super::*`.
- The worktree's `target/` dir compiles from scratch and is slow
  (~8 min for a full `cargo clippy --all-targets`, ~8 min for the first
  `cargo check --tests`) because it doesn't share the main checkout's
  `target/`. Competing worktree builds (wo-15.8, wo-15.11 were running
  concurrently this session) amplify the wall-clock time. Budget ~20-25
  min for the full gate (fmt + clippy + test + check) on a cold worktree
  with 2-3 concurrent neighbors. Using `setsid bash -c '... > log 2>&1'
  & disown` to launch long cargo jobs in the background and polling the
  log with `sleep N; tail` avoids the 600s tool timeout while the build
  runs.
- `git_sanitation::read_limited` tests pass an EXPLICIT `limit` arg
  (e.g. `read_limited(&path, 4)`) and never reference the module-level
  `SCAN_CAP_BYTES` const. So raising `SCAN_CAP_BYTES` 1 MiB → 10 MiB
  does not break any `read_limited` test. The const is only consumed at
  the one call site `read_limited(&path, SCAN_CAP_BYTES)` in
  `check_worktree`.
- `lessons.md` IS in `.gitignore` (line 23) but was force-added
  (`git ls-files lessons.md` shows it tracked) in a prior session. So
  `git check-ignore lessons.md` returns exit 1 (ignored) but `git
  status` shows it clean because it's already tracked — the tracked
  copy wins over the ignore once added. Per AGENTS.md §7, the convention
  is gitignored scratch; since it's already tracked here, updating it
  in-place is fine and will be committed.

## What I tried that didn't work
- First trufflehog-timeout test used a fake trufflehog that `sleep 120`
  and an outer `tokio::time::timeout(20s, verify_security(&event))`. The
  inner `TRUFFLEHOG_TIMEOUT_SECS` was 60s, so the inner timeout fired at
  60s — but my 20s outer test budget tripped FIRST, failing with
  `verify_security should resolve before the 20s test budget: Elapsed`.
  Fix: introduced the `#[cfg(not(test))] / #[cfg(test)]` const override
  (60s prod / 2s test) and shortened the fake sleep to 30s (still well
  over 2s). The test now runs in ~2s. Lesson: when an inner timeout
  needs testing, make the timeout VALUE test-overridable (the repo's
  established pattern) rather than sizing the outer test budget around
  the production value.

## What I'd do differently
- Nothing significant. The four fixes were small and independent; the
  only wrinkle was the trufflehog test timeout sizing (resolved with the
  cfg-test const override). The WO gate wording `session::verifier tools`
  should be read as two commands — flagging that for future WO authors
  would save a wasted compile cycle.

## WO 15.15 — split kirkforge-draw-core/src/state.rs (2026-07-31)
### What I learned about this codebase
- `state.rs` was 4,863 lines (1,832 impl + 3,031 tests, 161 `#[test]`s).
  The draw document model. It splits cleanly by state-domain into 8 impl
  submodules + 1 helpers + 1 tests file.
- **Splitting a single `impl DrawState` across files**: Rust allows
  multiple `impl` blocks for the same type across files in the same
  crate. The private struct fields are the constraint — sibling `impl`
  blocks in `state/foo.rs` CANNOT touch `self.private_field` unless the
  field is visible to them. Making the private fields `pub(super)` (visible
  within the `state` module tree) is the minimal-visibility fix. Don't
  make them `pub(crate)` — that leaks to the whole crate; `pub(super)` is
  scoped to the `state/` directory.
- **`mod tests` in a separate file vs inline**: when tests live in
  `state/tests.rs` (declared `#[cfg(test)] mod tests;` in mod.rs), the
  file must NOT wrap its contents in `mod tests { ... }` — that creates
  `state::tests::tests` (double nesting), and `use super::*` then refers
  to   `state::tests` (the file's own module), NOT `state` (mod.rs). The
  fix: `tests.rs` holds the `use` lines + `#[test] fn`s at the top level
  of the file (module `state::tests`), so `use super::*` refers to
  `state` (mod.rs). Dedent the original `mod tests { }` body by 4 spaces
  and drop the wrapper.
- **`use super::*` and private imports**: a child module's
  `use super::*` brings in the parent's `use` imports ONLY because child
  modules can see parent private items. But if the parent's `use` lines
  import types the parent no longer uses (because the impl moved to
  submodules), the lib build flags them as `unused imports`. Fix: keep
  only the imports the parent's own body uses in `mod.rs`; move the rest
  into the test module's own `use` lines. The tests need `Align`,
  `BoxObject`, `DistributeAxis`, `LineObject`, `SelectionMode`,
  `TextObject`, `MAX_UNDO`, `new_object_id` — import those in `tests.rs`
  directly.
- **`pub(super)` re-export gotcha**: `pub(super) use helpers::{...}`
  FAILS with E0364 ("is private, and cannot be re-exported") when the
  helpers are themselves only `pub(super)`. The cleaner path: import the
  helpers privately in `mod.rs` (`use helpers::o_id;`) for the impl's
  own use, and let the test module import them directly
  (`use super::helpers::MAX_UNDO;`).
- **Pre-existing `content_hash` compile error on base branch fb334cb**:
  WO 15.8 (commit 922af2d) added `content_hash: u64` to `FileWriteEvent`
  but only updated 11 of 17 `FileWriteEvent { ... }` literals in
  `src/session/verifier/security.rs` test fixtures. The other 6 were
  missed, leaving `cargo clippy --all-targets` and `cargo test --workspace`
  red on the base. This is a WO 15.8 regression, not mine. Fix: add
  `content_hash: 0,` to the 6 missed literals (the field's doc comment
  says "tests may leave it 0"). Scope creep, but necessary to unblock
  the WO 15.15 gate.
- **Pre-existing parallel-test flakes in the `kirkforge` lib**:
  `tools::glob::tests::glob_default_base_dir_is_cwd`, 15
  `tools::lsp_query::tests::*`, and
  `session::verifier::security::tests::test_shell_danger_in_star_comment_line_is_skipped`
  panic with "No such file or directory" under `cargo test --workspace`
  parallel load but PASS in isolation. Root cause: cwd / shared-temp-dir
  races (the security verifier scans `std::env::temp_dir()` and picks up
  leftover files from sibling tests). Same class of flake as the
  tarpaulin tempdir race in state.md. NOT my regression — I touched none
  of those files. Don't chase these; they're environment flakes.
- **3 competing worktree builds (wo-15.12, wo-15.14, wo-15.15) made cargo
  3-4× slower**: a `cargo clippy --all-targets` that normally takes 3-4
  min took 12+ min. Background `setsid bash -c '... > log' & disown` +
  poll-with-sleep is the only way to survive the 600s tool timeout.
- **`git mv state.rs state/mod.rs`** is the clean way to start a module
  split: it preserves git history for the renamed file, and `pub mod
  state;` in lib.rs works for both `state.rs` and `state/mod.rs` with no
  lib.rs edit needed.

### What I tried that didn't work
- First attempt put all tests in `tests.rs` wrapped in
  `#[cfg(test)] mod tests { use super::*; ... }` (verbatim from the
  original). This created `state::tests::tests` and `use super::*`
  resolved to the wrong module (456 errors). Fix: unwrap the
  `mod tests { }` wrapper and dedent, so the file is the `state::tests`
  module directly.
- First `mod.rs` used `pub(super) use helpers::{...}` to re-export
  helpers for the tests. E0364 (can't re-export `pub(super)` items at
  `pub(super)`). Fix: private `use helpers::o_id;` in mod.rs for the
  impl, and `use super::helpers::MAX_UNDO;` in tests.rs.

### What I'd do differently
- Nothing significant. The split is clean and the test count is
  preserved. The only unplanned work was the 6-line `content_hash` fix
  in security.rs, which was a pre-existing base-branch regression that
  had to be cleared to get a green workspace gate.

## WO 15.26 Batch D — docs + polish (2026-07-31)

### What I learned about this codebase
- **`readme_drift` counts `#[test]` STRICTLY.** It requires `#[test]`
  on its own line AND the next line to start with `fn ` (filters
  `#[test]` followed by `#[ignore]`/`#[should_panic]`/other attrs). A
  loose `grep -rn "#\[test\]" crates/` overcounts: it gave 1674 but the
  test's own counter gave 1652. The binding number is the test's
  counter, NOT a plain grep. I bumped the README 1652→1674 (wrong),
  `readme_drift` failed with "drift 22", and I reverted. Always trust
  the drift test's own counter when bumping the State table.
- **Self-referential drift tests give false confidence.** `gaps.rs`
  had `default_thresholds_match_ci` that asserted the const against its
  OWN hardcoded literals — so it passed while the const (68.0) had
  drifted from ci.yml (68.5, raised in WO 12.9). A real drift test must
  parse the external source. Replaced it with one that parses the ci.yml
  `targets = { ... }` dict. The loose-grep-1674 vs strict-1652 lesson
  (above) is the same class of error: the test's contract is the truth.
- **`cargo fmt --check` exit code via a pipe lies.** `cargo fmt
  --check 2>&1 | tail` makes `$?` capture `tail`'s exit (0), masking
  the real fmt failure. Run `cargo fmt --check` WITHOUT a pipe to read
  its true exit, or just run `cargo fmt` to auto-fix then verify.
- **ADR-066's "30 tasks" is historically accurate, not stale.** WO 9.9
  reached 30 tasks; ADR-066 adds the signature challenge as the 31st.
  So the four "30"s in ADR-066 are the PRE-signature count. The WO
  4.1 premise ("30→31 in four places") conflated pre-ADR and post-ADR
  state. Lesson: don't blindly apply WO-prescribed numeric edits —
  verify the semantic/time context first. The REAL arithmetic error
  (4.2) was "~9/~10 planned" (actual: 19 planned, 18 covered, 3 gap).
- **KIRK-BENCH mapping is many-to-one.** The 31 existing task files map
  to only 18 of the 40 spec slots; 19 spec tasks are planned; 3 (Fix
  Compilation Error, Fix Integration Test, Implement Missing Trait) are
  neither mapped nor planned. So "31 existing + ~9 planned = 40" was
  wrong on two counts.
- **Verify-first paid off (AGENTS.md §7).** 3.13
  (`ConnectionState::Connecting` — already has a ceiling comment at
  app.rs:22-25), 4.9 (all plugin3-hosts shims already carry `ponytail:
  stub-only`), and 4.18 (TECHNICAL.md already documents ADR-050
  two-path at L84/104/430/441/732) were all already done. Thirty
  seconds of grep saved three duplicate edits.
- **Several Batch D 4.x items live in Batch A/C files.** 4.10
  (src/adapters/m5_tests.rs), 4.11 (src/session/executor/mod.rs), 4.13
  (executor/turn.rs), 4.14 (executor/loop_.rs), 4.15 (executor/scout.rs),
  4.16 (src/adapters/bedrock_signing.rs) all touch out-of-scope
  subsystems. Per the hard scope rule these are honest deferrals, not
  fixes — recorded in state.md, not silently skipped.
- **`src/session/worktree.rs` is NOT in any guarded subsystem**
  (not config/, not verifier/, not executor/), so 4.12 (session_id
  validation) was in-scope even though it's under `src/session/`. The
  scope boundary is the subdirectory, not the parent.
- **`new_session_id()` produces `YYYY-MM-DD-session-N`** (hyphens only),
  so adding "no `/`, no `\`, no `..`" validation to
  `WorktreeSession::create` can't break the sole caller — safe
  defensive hardening.
- **ci.yml `--skip test_build_fork_tree_nests_children` targets the
  WRONG test.** The historical tarpaulin flake was
  `test_build_fork_tree_orphan_fork_is_a_root`. WO 12.0 fixed the root
  cause; reconciling/removing the skip needs a verified tarpaulin run,
  so it's deferred (4.6/3.53), not blind-fixed.

### What I tried that didn't work
- **4.3 README 1652→1674** (based on a loose grep). `readme_drift`
  failed (drift 22). Reverted to 1652 — the test's strict counter is
  the binding value.
- Considered deleting `ManifestError::UnsupportedApiVersion` (3.16),
  but it's matched in `src/main/error.rs:182` as forward-compat for a
  future V2 and `ApiVersion` only has `V1` (serde rejects unknown
  versions at parse → `ManifestError::Parse`, so it's never
  constructed). Wiring is impossible without a 2nd variant; removal
  touches error.rs (out of clean scope). Deferred honestly.

### What I'd do differently
- For any "update test count" item, FIRST run the relevant drift test
  to learn its counting method, then bump to match — don't grep.
- For "fix N→M in K places" WO items, read the target doc's
  time-context before editing; the prescribed number may be wrong.
