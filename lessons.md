# lessons.md — WO 27.2 e2e hang fix session (2026-08-12)

## What I learned

### The "ZERO output / never reaches adapter" e2e diagnosis was a false negative
- The prior session's conclusion ("daemon starts, session-index WARN, then
  SILENCE... binary never reaches the adapter") was misleading. The binary DID
  work past the daemon — the SILENCE was because `init_tracing` routes to the
  log FILE (`$DATA_DIR/kf-code.log`) by default, NOT stderr. The harness
  captures stderr, which was empty of trace. The log file had the real story.
- **Lesson:** when an e2e binary produces "no output," check `$DATA_DIR/kf-code.log`
  FIRST (or set `KF_CODE_LOG_STDERR=1`). Don't infer "binary is wedged" from
  empty stderr when tracing is file-routed. `RUST_LOG=info` alone does nothing
  if the subscriber's writer is the file layer.
- Definitive diagnosis needs `eprintln!` checkpoints (unbuffered, stderr) OR
  reading the log file. Tracing output is buffered/routed and lies to a
  stderr-capturing harness.

### The actual root cause: synchronous context-index build in startup
- `freeze_launch_sandbox` sets `sandbox_dir = cwd`. `ContextIndex::index_dir`
  then runs `WalkDir::new(cwd)` (NO `.gitignore`/`target`/`node_modules` filter)
  + tree-sitter parse of every `.rs`/`.ts`/`.tsx`/`.py`/`.go` file. On this
  repo (645 files) it took >90s — pathological for the file count, suggesting
  O(n²) in `resolve_imports`/`resolve_call_edges`, not just slow parsing.
  Biggest files were <100KB so it's not a single huge-file issue.
- The in-process `wiremock_integration.rs` test passed because it calls
  `run_turn_collecting` directly, bypassing the entire `run_session` setup
  (including the index build). That's why the in-process path was green while
  the binary-spawn path hung — they don't share the startup sequence.

### Secondary: daemon subprocess pipe inheritance
- `std::process::Command::new(exe).spawn()` with no stdio redirection
  INHERITS the parent's stdin/stdout/stderr. The long-lived `kf-code daemon
  --foreground` grandchild then holds the parent's piped write-ends open. After
  the parent exits, the harness's `read_to_end` blocks forever (no EOF until
  the grandchild dies). This affects ANY piped caller (CI, shell pipelines),
  not just tests.
- **Fix pattern:** any spawn of a long-lived helper (daemon) MUST set
  `.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())`. The
  daemon has its own tracing log; it doesn't need the parent's stdio.
- `std::process::Command` has no `kill_on_drop` (tokio-only); the stdio
  inheritance + no-kill_on_drop combo is the universal "orphaned subprocess
  hangs the test" pattern.

### Committed conflict markers — AGAIN
- The worktree HEAD (`7a0de4d`, WO 29.7 merge) had committed conflict markers
  in `Cargo.toml` + `Cargo.lock`. This is the SECOND time (first was `5a6c32d`
  per the prior lessons.md). The repo's `ci.yml` has a conflict-marker grep
  but it clearly didn't run / wasn't enforced on these merges.
- **Lesson:** ALWAYS run `grep -rln '<<<<<<<\|>>>>>>>\|^|||||||' Cargo.toml
  Cargo.lock` as the FIRST precondition step. Don't trust that the latest
  merge is clean. 30 seconds saves the build.

### Test-harness read_to_end is a latent hang
- A `try_wait` loop with a deadline is NOT sufficient if followed by an
  unbounded `read_to_end`. The read must ALSO be deadline-bounded (via a
  detached reader thread + `mpsc::recv_timeout`) so a grandchild holding the
  pipe surfaces as a named `TimedOut` instead of an infinite hang.

## What I'd do differently
- Read `$DATA_DIR/kf-code.log` BEFORE adding checkpoints — one log read would
  have shown the daemon-started/session-WARN trace and pointed at the
  post-daemon block immediately, saving the instrumentation cycle.
- Grep for conflict markers before `cargo build` — the first build died on
  the markers; 30s of grep would have caught it pre-build.

## Scope creep
- `Cargo.toml` + `Cargo.lock` conflict-marker resolution: required to build
  anything (precondition, not the e2e task). Took the wo29g side
  (`kf-orchestrator` dep + `thiserror 2.0.20`) — both confirmed present in
  the lock and crate list.
- `docs/TECHICAL.md` context-index note: AGENTS.md §9 mandates doc-sync for
  context-index changes. Added a 4-line note about the non-interactive skip.
