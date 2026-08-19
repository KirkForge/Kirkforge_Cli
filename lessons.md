# lessons — WO 35.1 session (.worktrees/wo35-d)

## What I learned

- **WO specs written before sibling WOs land go stale fast**: 35.1's file list
  named 4 files; the double-wrap fix actually required touching
  `src/tools/workflow.rs` (TaskSpawnerStepRunner used by jobs/runner.rs) and
  `src/tools/task.rs` because 35.2/35.3 added raw-prompt `run_task` callers
  the spec didn't know about. Always re-grep call sites, trust the code.
- **`run_task`'s prompt contract is now verbatim** — any future caller must
  apply `build_task_prompt` (in `tools::task`, NOT task_spawner) or pass a
  complete role prompt. Missed callers silently lose the persona preamble.
- **The TaskSpawner trait's dyn dispatch is the sanctioned test seam** —
  "ponytail: single impl, dyn dispatch for test injection" is literal.
  `ParallelOrchestrator::with_spawner` (private) injects a probe that records
  start/end events; strict-order + prompt-content assertions in one test,
  no DI framework.
- **Sequencing tests without a model**: probe spawner + 10ms in-flight sleep;
  under a (wrong) join! fan-out all three `start:` events precede the first
  `end:` on the single-threaded test runtime — deterministic.
- **`str::as_str()` on a `&str` is unstable (`str_as_str`)** — when changing
  `let prompt = build_task_prompt(...)` (String) to a borrow, also drop the
  later `.as_str()` call sites.
- Fresh-worktree cold `cargo check` on this box: 7m50s+ (over a 7min timeout
  once). Budget ≥10min for first checks; incremental are ~15s.
- nextest filter syntax: multiple bare filters are OR'd
  (`nextest run ... session::parallel_orchestrator tools::task` works).
- Borrow gotcha: `let x = &vec.lock().unwrap().iter().find(...).unwrap().1`
  — the MutexGuard temporary dies at statement end. Clone out of the guard
  or bind the guard first.

## What I tried that didn't work

- Returning `&'static str` from a helper that maps unknown personas to the
  input `&str` — lifetime clash; return String in tests.
- First `cargo check` attempt hit the 7min tool timeout (cold build); rerun
  with 20min budget, fine after.

## Scope creep disclosure (also in workplan.md)

- `src/tools/workflow.rs`: not named by the WO; required because its
  TaskSpawnerStepRunner (also the path for jobs/runner.rs) calls run_task
  with raw prompts and the verbatim-prompt contract change requires it to
  wrap.
