# WO 32 lessons

## What I learned about this codebase
- `std::fs::FileTimes::set_modified` (stable since Rust 1.75) is the clean way
  to force a distinct mtime in tests without `filetime` dep or wall-clock
  sleeps. The toolchain is 1.88, so it's available.
- The hooks test module had 3 near-identical marker-poll loops (300×50ms).
  Extracting `poll_for_marker` DRY'd them and made the bounded-budget intent
  explicit (15s cap with panic-on-timeout instead of silent assertion flake).
- `BlockingSpawner` in task.rs had a `finish: Arc<AtomicBool>` flag that NO
  test ever sets — the `while !finish { sleep(10ms) }` was a busy-wait for
  nothing. `std::future::pending()` parks cheaply; the flag is kept for
  struct-construction parity.
- The daemon `wait_for_socket` polls were already bounded (50×20ms=1s); I
  tightened to 5ms interval with the same 1s cap. Marginal but consistent.
- The caching forwarder tests needed care: the 50ms sleeps were "prove a
  negative" (cache stays empty). Replaced with `yield_now()` loops (8×) to
  let the forwarder task observe the closed receiver and abort. The forwarder
  only caches complete streams (Done event present), so a truncated stream
  never writes to cache regardless.
- `caching_adapter_aborts_forwarder_on_consumer_drop` uses a `CountingAdapter`
  with 5ms pacing sleeps inside the mock — those are emission rhythm, NOT
  test-sync sleeps. Left them; they're what makes the abort observable.

## What I tried that didn't work / pitfalls
- Initially tried to replace the hooks `test_run_hook_timeout_kills_descendants`
  2s sleep with a 6s bounded poll (to properly prove the descendant never
  touches the marker past the 5s hook timeout). Realized this ADDS wall-clock
  (6s > 2s) and the original test was already weak (2s < 5s timeout). Reverted
  to keeping the 2s sleep as a genuine production-timeout wait — documented
  why. The task says "keep genuine timeout tests as-is."
- `rustfmt <file>` standalone fails without `--edition` — use `cargo fmt` or
  edit manually.
- The worktree had pre-existing uncommitted changes from prior WO 32 work
  (jwt.rs, executor/mod.rs, turn.rs, task_spawner.rs) and a pre-existing
  fmt issue in config/mod.rs:160 (committed in 15dd08a). None are mine.

## What I'd do differently
- The `tui/commands` `#[ignore]`d cancel-running-job test (100ms sleep) was
  left as-is — it's a known-flaky subprocess test, not in the default gate,
  and modifying it risks the flake the ignore was added to prevent. Noted as
  deferred.
- The SSE mock helper sleep (http.rs:1054) was shortened 100ms→20ms but not
  fully eliminated — it's a cross-library read race (Windows), not a test-
  sync sleep. A oneshot wired through the mock would be fully deterministic
  but doubles helper surface for a Windows-only race.

## Sleeps removed (count)
- edge_cases.rs: 1s thread::sleep → FileTimes::set_modified (−1s)
- turn.rs: 50ms×40 poll → bounded 10ms poll (−~2s worst case, −~80ms typical)
- hooks.rs: 2× 50ms×300 poll → poll_for_marker helper (same budget, faster
  common case, panic-on-timeout); 100ms sleep → yield_now (−100ms)
- task.rs: 10ms busy-wait loop → pending() (−all); 50ms sleep → Notify
  (−50ms); 3× 20ms×50 poll → poll_until helper (−~1s worst case each)
- tui/commands: 3× 50ms×40 poll → wait_for_job_done helper (−~2s each)
- daemon client/server: 20ms×50 → 5ms×200 (same 1s cap, finer granularity)
- caching: 2× 50ms sleep → 8× yield_now (−100ms); 60ms sleep → stability
  poll (−60ms typical)
- mcp_client mod.rs: 50ms sleep → yield_now (−50ms)
- mcp_client http.rs: 100ms → 20ms (−80ms, Windows race, kept)

## Deferred (disclosed)
- `tui/commands/mod.rs:376` (handle_jobs_command_cancel_running_job_succeeds):
  100ms sleep left as-is. Test is `#[ignore]`d as "timing-sensitive job-cancel
  race" — a known-flaky subprocess cancel test, not in the default gate.
  Replacing the sleep with a poll-for-Running risks the flake the ignore was
  added to prevent. Remaining: replace with `wait_for_job_done`-style poll
  for JobStatus::Running; tracked in this workplan.
- `session/hooks.rs:966` (test_run_hook_timeout_kills_descendants): 2s sleep
  kept. This is a genuine production-timeout wait (the test waits for the
  hook's 5s execution timeout to fire and kill the pgrp). Replacing with a
  6s bounded poll would ADD wall-clock. The task scope says "keep genuine
  timeout tests as-is." Remaining: could shorten by making the hook timeout
  configurable via env var (scope creep, not done); tracked in this workplan.
- `session/mcp_client/http.rs:1054`: 20ms sleep kept (shortened from 100ms).
  Cross-library read race on Windows — not a test-sync sleep. A oneshot
  wired through the mock would be fully deterministic but doubles helper
  surface. Remaining: wire oneshot; tracked in this workplan.
## WO 33.14 phase 3 — verifier CommandRunner (session 2026-08-15, worktree wo-fakes)

### What I learned
- **`tokio::task::spawn_blocking` requires `'static`** on captured `&dyn Trait`.
  I first wrapped the `CommandRunner::run` call in `spawn_blocking` to keep
  the sync `SystemCommandRunner` off the async worker pool; the borrow
  checker rejected it (`runner` escapes the closure, `'1` must outlive
  `'static`). Fix: call `runner.run` directly in the async fn. The prior
  code blocked the worker on `tokio::process::Command::output().await` too,
  so this is no worse. A `spawn_blocking` wrap would need an
  `Arc<dyn CommandRunner + Send + Sync>` — not worth the indirection for a
  post-edit verifier that runs outside the hot path. Documented as a
  `ponytail:` ceiling with the upgrade path.
- **The verifier subsystem was already half-faked.** The pure parse helpers
  (`parse_build_json`, `parse_clippy_json`, `module_path_prefix`) were
  already unit-tested in-process; only the orchestration path
  (event → cargo_root → spawn → parse → Verdict) was `#[ignore]`d. The
  `CommandRunner` trait closes that gap — the orchestration is now
  unit-testable with a fake. Lesson: read the existing test coverage before
  assuming a full fake framework is needed.
- **WO 33.14's prior `#[ignore]` approach for items 4/5 was the pragmatic
  minimal.** The `BashJobRegistry::spawn` blast radius is CRITICAL (96
  callers, 18 modules). Faking it = the "full fake process framework" the
  workorder explicitly scoped out. The cap bookkeeping is pure HashMap
  logic already tested without subprocess. The 64-process test is a stress
  test, not a correctness test — `#[ignore]` is the right call there.
- **`rustfmt.rs` was in the grep hit list for `Command::new` but out of
  scope.** It spawns `rustfmt` (not `cargo`), has no `#[ignore]`d happy-path
  test, and its tests all skip before reaching the subprocess. The task
  scope is Cargo/Clippy. Leaving it untouched is correct, not lazy.

### What I'd do differently
- Considered making `CommandRunner::run` `async` for object-safety with the
  spawn_blocking wrap. Rejected: `async fn` in traits needs `async-trait`
  (a dep the repo avoids) and the sync `run` + direct call is simpler and
  matches the prior blocking behavior. The trait is object-safe as written.

## WO 32.5 — parallel orchestration (session 2026-08-15, worktree wo32d)

### What I learned
- `TaskHandle` has private fields (`started`, `cancel_requested`,
  `cancel_signal`) — can't construct it with a struct literal from outside
  `tools::task`. Use `TaskHandle::default()` + `TaskManager::get_mut` to set
  metadata after insert. Added `get_mut` for this (LOW risk, 0 impacted).
- `InProcessTaskSpawner::run_task` is a trait method (`TaskSpawner`), not an
  inherent method — must `use crate::tools::task::TaskSpawner;` to call it.
  LSP didn't flag this; cargo check did.
- `build_task_prompt` in `task_spawner.rs` was private; made it `pub(crate)`
  so the orchestrator's fallback role-prompt builder can delegate to it.
- The worktree's `target/debug/.cargo-lock` got held by orphaned `cargo`
  processes from timed-out build commands. `fuser` identified the PIDs; `kill`
  + `rm -f .cargo-lock` unblocked. Watch for this when builds time out.
- LSP diagnostics from other worktrees (wo32, wo32b, wo32c) bled into this
  worktree — exactly the AGENTS.md warning. Only trusted `cargo check`.
- An edit to `src/session/mod.rs` (adding `pub mod parallel_orchestrator;`)
  was silently reverted — likely the stale rust-analyzer LSP revert issue.
  Re-applied and verified with `grep`. Always verify edits took.

### What I'd do differently
- The `--parallel` flag parsing uses `strip_suffix("--parallel")` which is
  fragile (requires the flag at the very end, no spaces between name and
  flag if the name itself ends with "parallel"). A proper arg parser would
  be cleaner, but the workorder asked for minimal — this works for the
  documented `/workflow run <name> --parallel` form.
