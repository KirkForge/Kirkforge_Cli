# lessons.md — WO 27.2 e2e hang fix session (2026-08-12)

## Session 2026-08-13 — WO 31.1 + 31.4 Python verification loop (wo31 worktree)

### What I learned about this codebase
- **Two coexisting Python-detection sites already existed.** `tui/commands/init.rs:65`
  has `detect_project(cwd) -> ProjectType` (single language, returns first hit).
  The WO 31.4 spec asked for a `Vec<ProjectLanguage>` (multi-language aware —
  real workspaces mix Cargo + pyproject). I added the Vec variant in
  `verifier/detect.rs` rather than refactoring `init.rs` — the two serve
  different purposes (init picks ONE gitignore template; verifiers fire ALL
  relevant tools). Don't unify them.
- **`Verifier` registration has TWO places that must stay in sync:**
  `init_default_verifiers` (registers at startup) AND `BUILTIN_VERIFIERS`
  const in `rebuild_plugin_verifiers` (the retain-list that survives plugin
  reload). Forget the second and your verifier works once, then vanishes on
  the first `/reload`. Grep `BUILTIN_VERIFIERS` before adding any new
  built-in verifier.
- **Python verifier self-gating pattern:** each `verify_X` does
  Edit/FileWrite match → `.py` ext check → `find_python_root` →
  `detect_project_languages(root).contains(Python)` → spawn tool. Three
  gates before any subprocess; Rust verifiers do the same with `find_cargo_root`,
  so registering BOTH sets is safe — they never double-fire.
- **`python -m pytest` distinguishability:** pytest-missing vs tests-failed
  both exit non-zero. The seam is stderr containing `"No module named pytest"`
  (Python interpreter's own error) → `Skipped`; anything else non-zero →
  `Fixable`. Without this, a host without pytest would emit `Fixable` with a
  confusing "No module named pytest" body.
- **Pre-existing RED at HEAD (4542e81) blocked the gate:**
  - `src/main/line_mode.rs:720` test called
    `spawn_non_interactive_approval_handler(rx)` with the old 1-arg
    signature; the fn gained `auto_approve: bool` in `958e4f2` but the test
    wasn't updated. The test's own comment ("deny even when auto_approve is
    true") made the fix obvious: pass `true`.
  - `cargo fmt --check` drift in `line_mode.rs` + `task_spawner.rs`
    (flagged by the WO30.4 worker as not-hers). Mechanical `cargo fmt` fix.
  - `Cargo.lock` had `kf-code = "0.3.6"` while `Cargo.toml` says `3.8.0`
    (commit `6e2e0d4` bumped one, not the other). `cargo check` regenerated
    it. All three are disclosed scope creep per AGENTS.md §7/§11 — same
    precedent the WO30b worker set.
- **`cargo test --lib -p kf-code session::executor::` is SLOW** (the full
  suite includes `wiremock_integration` + the `loop_` tests which spawn mock
  HTTP servers). Sub-filter to the specific module (`dispatch`, `turn`,
  `coverage_gaps`, `verifier_cross`) — each finishes in 2-4s. The workorder
  gate is `verifier::` only (235 tests, ~3s), not the executor suite.

### What I'd do differently
- The `[tool.mypy]` detection is a substring scan over pyproject.toml
  (`text.contains("[tool.mypy]")`) rather than a TOML parse. Marked with a
  `ponytail:` comment naming the ceiling (false-positive risk is negligible
  — the literal only appears under that section) + upgrade path (parse with
  `toml` if section detection ever gets ambiguous). Resist pulling a TOML
  parser into the verifier hot path for one substring check.

## Session 2026-08-13 — WO 30.2 TaskManager lifecycle (wo30b worktree)

### What I learned about this codebase
- **Two parallel background-job systems, don't conflate them.** `TaskManager`
  (`tools/task.rs`) = subagent tasks, string ids `task-N`, per-session
  (`Arc<Mutex<..>>` created fresh inside `all_tools()` at `tools/mod.rs:204`).
  `BashJobRegistry` (`session/bash_jobs.rs`) = bash commands, numeric ids,
  **global singleton** (`global_registry()`). The `/jobs` TUI command only
  reads the bash registry. The right layer for subagent lifecycle is
  `TaskManager`; `bash_jobs.rs` needed zero changes.
- **`TaskManager` is unreachable from the TUI.** Unlike `BashJobRegistry`,
  it's per-toolset, not in `AppState`. So "make metadata available to /jobs"
  meant shipping `list()`/`format_task_entry` (done) — the actual `/jobs`
  rendering is a cross-layer plumbing task (global singleton vs `ServicesState`
  field), deferred + disclosed.
- **`InProcessTaskSpawner::run_task` creates its OWN `cancelled: Arc<AtomicBool>`
  internally** (`task_spawner.rs:224`) per call — it does NOT receive one from
  the caller. So "surface the executor's AtomicBool to the TaskManager" can't
  be done without changing the `TaskSpawner` trait (blast radius: trait + all
  impls + test mocks + workflow runner). Lazy alternative that preserves
  Cancelled-vs-Failed semantics: `tokio::select!` race in the spawn closure.
- **Cooperative cancel can't distinguish Cancelled from Failed.** Threading
  the `&AtomicBool` into `run_turn_collecting` returns via `Err` on cancel →
  recorded as `Failed`. The `select!` drop approach marks `Cancelled` cleanly.
  Tradeoff: dropping `run_task` mid-flight leaks its temp dir (cleanup is at
  the end of `run_task`). Bounded; flagged with a `ceiling:` comment.
- **`TaskOutput::is_completed` had zero external callers** — safe to retarget
  to `is_terminal()` (so cancelled tasks stop polling).
- **AGENTS.md gotcha confirmed:** `cargo check`/`clippy` cold runs are 3-4 min
  each on this repo. Budget wall-clock; run the focused test first, then gates.

### Scope creep (disclosed)
- `src/session/task_spawner.rs`: 1-line `cargo fmt` fix (`.into()` line wrap).
  Pre-existing from `5fbd955` (WO 30 sibling), not my regression. Fixed so the
  `cargo fmt --check` gate is green. AGENTS.md §7/§11 bless disclosed scope
  creep that unblocks a hard gate.

### What I'd do differently
- For the TUI integration: the "make available to /jobs" constraint was
  ambiguous given the per-toolset `TaskManager`. Next time, surface this
  tension in `workplan.md` BEFORE implementing and either (a) confirm with the
  owner that `list()` alone satisfies "available", or (b) pick the plumbing
  approach up front. I chose (a) + explicit deferral, which is defensible but
  leaves the visible `/jobs` integration as a follow-up.


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

## Session 2026-08-13 — WO 30.4 seccomp syscall filter (wo30c worktree)

### seccompiler 0.5.0 API gotchas (docs.rs example hides these)
- `SeccompFilter::new` takes **`BTreeMap<i64, Vec<SeccompRule>>`** — NOT
  `HashMap<u64, _>`. The docs.rs example uses `.into_iter().collect()` which
  infers the type; spell it wrong (HashMap/u64) → E0308. Keys are `i64`
  (`c_long`); BTreeMap gives deterministic BPF.
- `SeccompAction::Errno(u32)` — cast `libc::EPERM as u32`.
- `seccompiler::apply_filter` internally calls `prctl(PR_SET_NO_NEW_PRIVS, 1)`
  then the `seccomp()` syscall. So no_new_privs is set for free (also blocks
  setuid gain in the sandboxed child — desirable). flags=0 → calling thread
  only (correct in pre_exec post-fork; child is single-threaded).

### pre_exec async-signal-safety split (mirrors landlock)
- `SeccompFilter::new` + `try_into()` to `BpfProgram` **allocate** (BTreeMap +
  Vec) → MUST run in parent before fork. `apply_filter` is allocation-free
  (prctl + seccomp syscalls + reads is_empty/len/as_ptr) → safe in pre_exec.
- Repo pattern: compute in `setup_rlimits` body, move owned data into the
  `move ||` closure, syscalls-only inside.

### A literal allowlist omitting glibc startup syscalls is DEAD-ON-ARRIVAL
- The WO 30.4 base list omits: `arch_prctl` (ld.so TLS/ARCH_SET_FS — block it
  and NO ELF execs), `set_tid_address`, `set_robust_list`, `rt_sigreturn`,
  `mremap`, `sigaltstack`, `madvise`, and modern `at`-variants glibc routes
  through (`newfstatat` = stat/fstat/lstat on x86_64; `faccessat` = access).
- Without these ld.so/bash get EPERM before any output → false confidence.
  ALWAYS augment a userspace-tool allowlist with glibc-runtime essentials.
  Workorder explicitly deferred "tune against real workloads", so augmenting +
  disclosing is correct, not scope creep.

### Gotchas
- `.github/workflows/ci.yml` runs ubuntu-latest (x86_64) + windows-latest
  only — no aarch64. x86_64-tuned allowlist is gate-safe; legacy syscalls
  (stat/fstat/lstat/access/pipe/dup2/fork/vfork/umount2/mount/getdents/
  arch_prctl) are x86_64-only → aarch64/riscv64 needs them dropped/cfg-gated.
- `cargo fmt --check` has a PRE-EXISTING drift at `src/session/task_spawner.rs:211`
  (not mine, not WO 30.4). Don't "fix" it inside a WO 30.x commit (scope
  creep). Separate fmt-cleanup if it bothers you. `rustfmt --edition 2021
  <file>` formats one file without touching the rest.
- kf-code lib test binary with `--features seccomp` takes ~6 min to compile.
- `seccompiler` pulls only `libc` — no C toolchain, earns its place.
- `lessons.md` is TRACKED in this repo (not gitignored, despite AGENTS.md
  §3/§7). Append, don't overwrite — I nearly clobbered the WO 27.2/29.x log.
