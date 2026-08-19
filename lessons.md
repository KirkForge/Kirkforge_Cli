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

## WO 35.6 — ExecutorAdapter wiring (2026-08-19)

### What I learned
- The ollama NDJSON stream parser (`ollama_ndjson.rs:216`) only decodes
  `\n`-terminated lines — a final unterminated line is silently dropped at
  EOF. Wiremock fixtures MUST end with a trailing newline or the model
  "returns nothing" with zero events and no error. Cost me a debug cycle
  because "(no assistant response produced)" is non-empty and passed a
  lazy `!content.is_empty()` assert — assert the exact expected content
  in mock-backed tests.
- The `ignore` crate honors `.gitignore` only inside a git repo
  (require_git default). Tests that assert gitignore behavior on a
  tempdir must `git init` it first.
- `run_turn_collecting` discards FinishReason; the only structural
  truncation signal in the event stream is `ContinuationRound { round,
  max }` with round > max (emitted before the exhaustion check). That is
  how run_task_detailed derives finish_reason "length".
- kf-orchestrator drags kf-memory-store → rusqlite (bundled SQLite) into
  the kf-code binary. regex/base64/sha2/hex were already deps. Owner
  accepted; disclosed in workplan + report.
- gitnexus index (main checkout) predates task_spawner.rs/plugin_tools
  — impact() not-found for their symbols. Grep cross-layer check was the
  fallback; detect_changes saw only doc + comment edits.
- Scope creep log: src/session/mod.rs (module registration for the new
  file), src/main/run_session.rs (stale comment doc-sync), Cargo.toml
  comment. All mandated by the WO's doc-sync rules.

### Bugs found that are NOT mine to fix here
- The NDJSON trailing-line drop above is arguably spec-noncompliant
  (NDJSON allows the last line to omit the separator). Real Ollama
  always sends the newline, so impact is mock/proxy-only. Note for a
  future hardening pass: flush the residual buffer at EOF.
