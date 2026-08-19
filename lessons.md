# lessons.md — WO 35.5 session

## What I learned about this codebase

- `src/lib.rs` exposes everything (`session`, `tools`, `shared`,
  `adapters`) — `tests/` integration files CAN drive the real
  `InProcessTaskSpawner` / `Executor` / `TaskManager`. The "no lib"
  assumption from reading `[[bin]]` in Cargo.toml was wrong.
- BUT key seams are `pub(crate)` or `#[cfg(test)]` from outside:
  `TaskHandle::cancel_handles` (pub(crate)), `ToolContext::with_spawner`
  (cfg(test) — but the fields are pub, so construct + assign works),
  `SUBAGENT_PATCH_MARKER` (pub(crate) — pin the literal in the test),
  `budget::clear_sliced_listeners` (cfg(test)). The TaskManager-cancel
  chain therefore goes through the REAL `task` tool (background=true).
- Best in-process harness pattern lives at
  `src/session/executor/tests/wiremock_integration.rs`: real adapter via
  `adapter_for_with_provider` + `adapter_routing {"e2e-": "Ollama"}`
  → wiremock NDJSON → `Executor::run_turn_collecting`. Copy that.
- Relative tool paths resolve against process CWD, NOT sandbox_dir —
  writes must use absolute paths. The subagent worktree path embeds the
  test pid (`kf-code-session-task-<pid>-<ms>`), so a wiremock responder
  can discover it by before/after temp-dir scan and substitute it into
  scripted tool args.
- A turn with tool calls makes MULTIPLE model requests inside ONE
  `run_turn` (iteration loop) — the tool result is visible in the NEXT
  request's recorded body. That's how to assert "model saw the denial".
- `max_tool_result_chars` (default 4000) truncates bash output BEFORE
  budget slicing — size fixtures so the slice still fires (remaining
  must be < 4000) and put "middle" markers inside [head, 4000-tail].
- wiremock closure responders that return non-200 + adapter retry can
  BURN queued replies (pop on attempt 1, fallback text on attempt 2) —
  mock symptom: "mock: no more replies queued" for the first reply.
  Only return 500 on paths that truly need it.
- `git apply` rejects a patch whose last hunk line lost its trailing
  newline — `trim()` instead of `trim_start()` on a diff is a bug.
- `scripts/test-fast.sh` = `--lib --bins` only; `tests/*.rs` integration
  files are NOT in it. Their gate is nextest per-file (as the WO says).
- `lessons.md` is NOT gitignored here (AGENTS.md says it is; the repo
  commits it per session — follow the repo).

## Operational self-inflicted wound (avoid repeating)

- After `git commit -m "x" --allow-empty` (stray shell chain), the
  `git reset --hard HEAD~1` used to drop it ALSO wiped uncommitted
  doc edits (CHANGELOG/WO status/lessons). Re-made them by hand. Use
  `git reset HEAD~1` (soft/mixed) to drop an empty commit when the
  tree has uncommitted work.

## Scope creep (disclosed)

- `src/session/executor/turn.rs` — one-condition fix (Phase 3 read-gate
  re-check with post-body state denied just-created new-file writes).
  The chain-2 test exposed it; AGENTS.md §6 root-cause rule applied.
  gitnexus impact: HIGH, single internal caller chain
  (dispatch_tool_call_batch → run_turn_inner → run_turn); full lib suite
  green after.

## Bugs found that are NOT mine to fix here (for state.md / future WOs)

- `Executor::set_budget_stores` / `set_stratum_store` have NO production
  call site; budget post-hooks can only register via
  `reload_plugins(registry)` — and the constructor's own hook
  registration is dead code (budget always None at construction).
  Budget slicing itself works when stores are set manually.

## What I'd do differently

- Read `tests/e2e/harness/mock.rs` BEFORE designing the mock — it
  already solved scripted replies + request recording; my common/mod.rs
  is a slimmed version of it plus the worktree scan.
- Debug-panic with full request/event dumps earlier (the dump beat 20
  minutes of code reading twice).
