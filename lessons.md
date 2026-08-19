# lessons — WO 35.2/35.3 session (.worktrees/wo35-c)

## What I learned about this codebase

- **`Executor::with_log_and_undo*` re-derives its PathGuardTower from the
  SharedConfig you pass it** — building subagent tools from a local modified
  config is NOT enough; the executor must get a config clone with the same
  `sandbox_dir` or its tower denies worktree writes. run_task now passes a
  frozen clone (also better isolation semantics: parent config edits can't
  move a subagent's sandbox mid-run).
- **WorktreeSession derives its path from the session id alone**
  (`$TMPDIR/kf-code-session-<id>`), globally namespaced, NOT per repo or per
  pid. Tests using fixed ids poison later runs if a worktree outlives the
  repo (`git worktree remove` needs the repo alive as CWD). Always: unique
  per-pid ids in tests + drop the guard BEFORE deleting the test repo.
- **`git add --intent-to-add` + `git diff HEAD` is the one-liner that folds
  untracked files into an appliable patch** — no porcelain parsing needed.
- **clippy `await_holding_lock` is crate-denied** — to serialize async test
  bodies use `tokio::sync::Mutex` (guard legal across await), not std Mutex.
- **`TaskStatus` precedence is result → error → cancel_requested** — keeping
  a cancelled task's status honest while retaining its output needs a third
  slot (`cancelled_result`), not reordering.
- tokio "full" does NOT include the `test-util` feature → no
  `#[tokio::test(start_paused)]`; dead-host tests pay real retry backoff
  (~3s). Budget for it or pick a non-retryable failure mode.
- Fresh-worktree first `cargo check` ≈ 3m40s (full dep tree); budget gates
  accordingly. Nextest = process-per-test, which makes pid-namespaced temp
  scans safe; libtest (threaded) needs an explicit lock (see task_spawner
  RUN_TASK_TMP_LOCK).
- Executor tests are ~5s each (checkpoint I/O); adding one test to
  tests/dispatch.rs costs little wall time because nextest parallelizes.
- Pre-existing flaky `valid_tier_sends_resolved_model` currently fails
  deterministically on dev tip e82e305 in this environment (verified via
  stash). Not ours.

## What I tried that didn't work

- snapshot-paused tokio test for the dead-host error path (no test-util
  feature) — fell back to real-time backoff, ~2.7s, acceptable.
- First version of the patch test deleted the test repo before the
  WorktreeSession dropped → `git worktree remove` failed silently (warn
  only) → leftover /tmp dir failed the NEXT test run with "already exists".
  Fixed by explicit drop-before-cleanup + per-pid worktree ids.

## Scope creep (disclosed)

- src/tools/workflow.rs + src/tui/commands/workflow.rs: TaskRequest literal
  updates only (new `cancel: None` field) — forced by the field addition,
  no behavior change.
- src/session/executor/tests/dispatch.rs: new test file edits (not in WO
  file list, but the WO gate demands exactly this test).

## What I'd do differently

- Grep for `TaskRequest {` across src/ BEFORE deciding to add a field —
  counted 11 literal sites; a trait-signature change would have been worse,
  but the count should be in the workplan up front.
- The `Helpers::tool_cancel_token` split (parent snapshot vs subagent live
  child) could have been one commit-internal helper method on Executor from
  the start instead of a map_or_else inline — fine at this size.
