# Lessons — WO 42.12 session (worktree wo42.12)

## What I learned about this codebase

- `Message.token_count` existed but was only populated from the API
  `usage.completion_tokens` path (`turn.rs:1134`). The field was added
  speculatively but never wired up at append time — classic "field exists,
  nobody writes to it" gap.
- There are FOUR copies of `estimate_message_tokens` (`PromptBuilder`,
  `compaction`, `microcompaction`, `summarizer`), all identical. I added
  a shared free function in `prompt/mod.rs` and made all four delegate to
  it. Consolidating them into one was the lazy path — one cache check
  instead of four.
- Inside `impl PromptBuilder`, a method named `estimate_message_tokens`
  shadows the free function of the same name. `estimate_message_tokens(m)`
  would recurse; must use `crate::session::prompt::estimate_message_tokens(m)`
  to reach the free function. Same-module name shadowing gotcha.
- Content mutation sites that needed cache clearing: `truncate_tool_results`,
  `dedup_adjacent_tool_results`, `minify_old_messages`, `stub_old_tool_results`
  (all in `prompt/mod.rs`), plus compaction's stub + condense paths. Six sites
  total — grep for `.content =` to find them all.
- The `adr_xref_drift` test has a pre-existing failure (WO 41.7/41.8 file
  headers say "Done" but README says "Pending"). Verified by stashing my
  changes and re-running — the failure is on origin/dev, not introduced
  by WO 42.12.

## What didn't work / would do differently

- First test for `truncate_tool_results_clears_token_count_cache` used
  `tool_name: "bash"` with 30k chars — but bash's cap is 60k, so no
  truncation happened. Switched to `grep` (cap 15k) to actually trigger
  the truncation path. Read the per-tool caps before writing the test.

---

# Lessons — WO 41.6 session (worktree wo416)

## What I learned about this codebase

- `detect_changes()` with the `worktree` param works — it correctly
  reported only the 6 changed files, low risk, 0 affected processes.
  This is the first session where the worktree-aware detect_changes was
  used and it worked (previous sessions noted it indexed the main
  checkout only).
- The `list` function in `permissions.rs` is pure and returns a String —
  adding diagnostics after the rule rows is a 3-line insert. The pure
  ops-layer split (WO 11.0 pattern) makes this trivially testable.
- `clippy::needless_range_loop` fires on `for m in 0..n` even when the
  index is used to build the result tuple — use
  `rules.iter().enumerate().take(n)` instead. One lint cycle cost.
- The shadowing subsumption check `glob_match(M.pattern, N.pattern)` is
  sound but incomplete: it never false-positives (if M's glob matches
  N's pattern string, M matches every value N does) but may miss true
  shadowings where the subset relation isn't expressible as "M matches
  N's pattern string." This is the right tradeoff for an advisory
  diagnostic — documented in the function doc comment.

## What didn't work / would do differently

- Nothing significant. The task was small (fast-path exemption
  territory: pure function + wiring + tests).

---

# Lessons — WO 37.2 session (worktree wo37-b)

## What I learned about this codebase

- `lessons.md` is TRACKED here despite AGENTS.md calling it gitignored — it's
  a newest-first per-session log. Append your section at the top; never
  overwrite. (I clobbered it once; `git checkout -- lessons.md` + prepend.)
- `ReducedStatePacket` in `kf_routing::correction` was already the complete
  shape — the WO's "if it's a placeholder, extend it" concern was false.
  Thirty seconds of reading types.rs saved an additive-struct detour.
- Written-file signal paths are RELATIVE (mode executors record the bare
  `name`); any scan must join against the delegation cwd. The correction
  loop's own R7 re-scan does NOT join — pre-existing latent gap; decision
  actions unaffected (analysis in ADR-076 consequences).
- The root `Cargo.toml` `kf-routing = {...}` line at ~70 is the
  `[workspace.dependencies]` table — the binary itself does NOT depend on
  kf-routing. Consumers of packet types need kf-orchestrator re-exports
  (added `OverallVerdict`, same pattern as `DelegationMode`).
- GitNexus index for this repo is STALE (points at `npm/kf-plugin/` TS files
  deleted in WO 29.9) — impact() returns targets in dead files. Cross-layer
  grep is the reliable blast-radius method until re-indexed.
- `cargo check -p kf-code --lib` cold build exceeds 5 min in a fresh
  worktree; budget 10-25 min for binary gates. Workspace clippy: ~7 min.
- rustfmt explodes table-driven test literals one-tuple-per-line — write the
  table, run `cargo fmt` immediately, don't hand-fight it.

## What didn't work / would do differently

- Referenced `kf_routing::correction::OverallVerdict` from the binary's e2e
  test before checking the dep edge — cost one full lib-test compile cycle
  (~5 min). Check `grep -n "^\[" Cargo.toml` section context first.

---

# Lessons — WO 36.3/36.4 session (worktree wo36-c)

## What I learned about this codebase

- The cancelled-flush block in `stream_iteration` sits INSIDE the loop
  body; a `break` out of the stream loop skips it (partial content lost,
  `Finished(Stop)` instead of `Finished(Error)`). Correct shape: hoist the
  flag check to the loop HEAD, let the cancel-token select arm `continue`
  into it. First draft used `break` — test caught it (conversation had no
  assistant message). The conversation-log assert was worth the Arc<tokio
  Mutex> wrapper it cost.
- `detect_changes()` indexes the MAIN checkout, not worktrees — it reports
  "no changes" for worktree edits. Phase B impact analysis is the real
  tool here; note it in workplan.md.
- Shared `CARGO_TARGET_DIR` across worktrees produces cross-contaminated
  error output occasionally (a phantom `TaskRequest { owner }` E0063 from
  the WO 36.2 worktree's concurrent build). Symptom: errors about code
  that doesn't exist in this tree. Fix: rerun; "Blocking waiting for file
  lock on build directory" in the output confirms contention.
- tests/loop_.rs harness pattern for driving the FULL `Executor::run`
  loop: one unbounded channel per control input; keepalive tuple holds the
  channel ends `run` does NOT consume (run takes approval_tx/event_tx —
  those move in; the approval RECEIVER must be kept alive or sends fail).
  `clippy::type_complexity` needs an allow on the 8-tuple.
- `tokio::sync::oneshot::Sender::send` consumes self — incompatible with
  `Tool::run(&self, ...)`. The codebase pattern is
  `Arc<Mutex<Option<Sender>>>` + take() (see SleepingTool); match it.
- Child modules see ancestors' private `use` imports via glob
  (`use super::super::*` in tests pulls executor/mod.rs's `Role`,
  `mpsc`, `ModelAdapter`...). Sibling test modules do NOT see each
  other's imports — shared mocks go in tests/common.rs (pub(super)).
- `CancellationToken` is one-shot and forever-cancelled once fired —
  per-turn tokens must be freshly installed each turn; the watcher cancels
  via a shared `Arc<Mutex<CancellationToken>>` slot the input arm swaps.
  Esc racing the swap is covered by the pre-existing iteration-start flag
  check.

## What didn't work / would do differently

- First test draft asserted on the spawned task's return value — but
  `run_turn` returns `Result<()>`, so there's no event vec to assert on.
  Drain the event channel with try_recv instead (TurnComplete is sent
  before run_turn returns).
- Initially left StalledStreamAdapter in tests/turn.rs with pub(super);
  moved to tests/common.rs — it's shared by two test modules and
  common.rs is the documented shared-mock home.

## Permanent conventions to fold into AGENTS.md (candidate)

- Worktree + shared target dir: phantom compile errors ⇒ rerun before
  diagnosing; check for lock-wait lines.
- tests/common.rs is the home for mocks shared across executor test
  sub-modules.
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

# lessons.md — WO 36.2 session (bash-job owner tracking)

## What I learned about this codebase

- **The bash-job watcher parks on the child mutex for the job's whole
  lifetime**: the watcher task holds the `Arc<Mutex<Child>>` guard across
  `child.wait().await`. Any code path that locks that mutex (the old
  `cancel()`, `remove()`, `clean()` for running jobs, the spawn-eviction
  pass) serializes behind the process's NATURAL exit — it cannot kill a
  watcher-parked long-running job at all. This is the real story behind the
  ignored `#[ignore = "timing-sensitive job-cancel race"]` TUI test. WO 36.2
  fixed `cancel()` (flip status first + kill by pid on contention);
  `remove()` on a still-running job still has the flaw — future work.
- `tokio::sync::Mutex::blocking_lock()` panics on current-thread test
  runtimes (same family as `block_in_place`, AGENTS §7). Grab `child.id()`
  before moving the Child into the map instead.
- gitnexus impact on `spawn`/`cancel`-style common names collates
  same-name symbols across the repo ("96 direct callers, CRITICAL" for
  BashJobRegistry::spawn) — grep the real callers before believing the
  blast radius.
- `TaskManager::cancel` must stay sync (sync #[test] callers); the async
  registry kill is fired via `Handle::try_current()` + detached spawn —
  silently skipped outside a runtime, which preserves old behavior in sync
  tests.
- Pre-existing leak (NOT mine, out of scope): `BashJobRegistry::spawn`
  inserts the Running job record BEFORE `proc.spawn()?` — a spawn failure
  leaves a phantom Running job with no watcher/watchdog. Note for a future
  pass.
- Owner-tag collision ceiling: TaskManager ids ("task-N") are per-manager
  (Task tool, orchestrator, each subagent executor each have one), so
  nested background subagents can mint duplicate owner tags; a cancel then
  reaches same-tagged jobs of another manager (cascade-like). Documented
  with `ponytail: ceiling` in cancel_by_owner + TaskManager::cancel.

## Scope deviations / deferrals (disclosed)
- Skipped a dedicated `ToolContext::with_task_owner()` constructor — would
  be dead code (AGENTS §5 "no dead code"); the executor setter
  (`set_task_owner`) is the single writer. Add the constructor when a
  second construction site needs it.
- Scope creep: src/session/process_group.rs (+`kill_process_group_by_pid`
  helper) — required by the root-cause cancel fix, which the WO gate test
  (a) cannot pass without: cancel-by-owner of a parked job would block
  behind natural exit and never kill.

## WO 36.1 — Binary-size measurement (2026-08-20)

### What I learned
- Fat LTO + `opt-level = "z"` + `strip` makes unreachable `pub` code in
  statically-linked workspace crates effectively free: the whole
  kf-orchestrator chain (incl. rusqlite with bundled SQLite C) costs
  16,384 B raw / 5,502 B tar.gz (0.08%) because nothing in the binary
  constructs `SqliteAdapter` — the linker never pulls the bundled C
  archive objects. "Drags X into the binary" is a compile-graph claim,
  not a size claim; only the measurement tells you.
- kf-code has TWO unrelated `MemoryStore` types: its own JSON-file
  `crate::shared::memory::MemoryStore` (used by the remember tool) and
  kf-memory-store's facade. Grepping `MemoryStore` in src/ hits the
  local one; the kf-memory-store one is only reached via kf-orchestrator
  (types + InMemoryAdapter in tests) — never constructed in the binary.
- Release.yml packages `tar -czf` of the bare kf-code binary (gzip
  default level); `--workspace` build but only kf-code ships. Replicate
  with tar -czf for the honest "what ships" number.
- Measurement build times on this 8-core box: full clean release build
  ~19 min; rebuild after removing one workspace dep ~12 min (LTO link
  dominates). Budget accordingly.
- Removing a workspace dep from Cargo.toml regenerates Cargo.lock —
  revert it along with the scaffolding (`git checkout -- Cargo.lock`)
  or the "clean tree" check fails.

# Lessons — WO 36.5/36.6 session (worktree wo36-d)

## What I learned about this codebase

- `tokio-util` in the root Cargo.toml is a PACKAGE dep, not a
  workspace.dependencies entry — crates must declare `tokio-util = "0.7"`
  directly. `tokio-util.workspace = true` fails with "was not found in
  workspace.dependencies".
- The gitnexus index lags the session-layer split badly (spawn_role,
  run_task_detailed, ParallelOrchestrator all unresolved; TaskBrief
  resolved only to an npm TS interface). For WO 35+ session-layer work,
  manual grep caller analysis is the reliable Phase B; note the staleness
  in workplan.md and proceed.
- ADR-075's wording "content = the final assistant message (same
  extraction the task tool's summary uses)" already covers the WO 35.2
  patch marker riding in content — routing the pipeline through the
  adapter needed NO Emission extension and NO ADR touch. Read the pinned
  spec literal before extending types "per the WO's suggestion".
- The "likely fault line" the WO 36.5 spec feared (per-call executor
  construction fighting worktree lifecycle) is a non-issue:
  run_task_detailed owns the entire worktree lifecycle per call (create →
  snapshot cfg → diff_patch → Drop removes). The adapter is a stateless
  mapper; only undo_stack/supports_images forwarding was missing.
- kf-code's root [dependencies] includes tempfile = "3" — usable from
  lib tests without a dev-dep addition.
- wiremock Mock mounts respond to EVERY matching request (not a queue) —
  a single mount_reply covers multi-turn sessions; only argument-varying
  tests need the VecDeque harness in tests/common/mod.rs.
- EmitError imported for a test-only `let err = ...` line triggers
  unused_imports in the non-test lib build (clippy --all-targets builds
  both) — don't import a type just to name it in a smoke line.

## Scope creep log

- None. Files touched are exactly the WO's list (crate model/lib/delegate/
  decompose/Cargo.toml, executor_adapter, parallel_orchestrator,
  event_sink_bridge [new], session/mod.rs [module reg], TECHNICAL, 3
  workorder statuses, CHANGELOG, lessons, Cargo.lock).

# lessons.md — WO 37.1 session (worktree wo37-a)

## What I learned
- The WO 36.2 phantom-job note was real but had TWO trigger paths, not
  one: unresolvable workdir (canonicalize `?` at old :215) fails after
  insert too, not just `proc.spawn()?`. Insert-after-spawn fixes both;
  test both (nonexistent dir vs regular-file workdir — a file resolves
  but chdir fails, exercising the true `proc.spawn()` error branch).
- `check_bash_command_str` only rejects unresolvable workdirs when
  `bash_sandbox_workdir=true` with a scoped PathGuard; with
  `PathGuard::default()`/false the gate passes them through, so the
  registry's own canonicalize is the real check.
- Insert-after-spawn also let pid be set on the job BEFORE insert,
  deleting the post-insert pid-update lock round-trip — reordering for
  correctness sometimes pays for itself.
- Global-counter ids broke one test's absolute-literal expectations
  (`task-1..task-12`); rank-based ordering assertions are the durable
  shape. `task_id_rank("task-2") < task_id_rank("task-10")` pins the
  lexicographic trap directly.
- gitnexus impact on common method names (`remove`, `spawn`) reports
  name-collision noise (CRITICAL, 17 modules) — the real caller set is
  the file-scoped resolution (jobs/runner.rs:171,183 for remove). Trust
  the `Function:file:path` target line, not the risk badge, for common
  names.
- Clippy over the shared CARGO_TARGET_DIR across worktrees can exceed
  10min (lock waits); it does finish in the background — a fast
  "Finished 1.29s" rerun is the honest gate, exit code included.

## Scope deviations
- state.md + TECHNICAL.md beyond the WO file list: AGENTS §task-mgmt 4/9
  mandates both on registry-behavior changes — disclosed, not creep.
