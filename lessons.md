# Lessons — WO 20 integration / session-death recovery

## Why the prior kf-code opencode session died (run `0fbedb9b`)
- It was running the final gate `cargo test --workspace --no-fail-fast` after the
  wo/20.7.0 merge. At 20:36:36 the **gitnexus MCP server dropped its connection**;
  opencode responded by `disposing all instances` (dir = `.../desktop`) and aborting
  the in-flight message. `error=Aborted stack=undefined`.
- Root cause = infra (MCP crash under memory pressure during the full workspace test),
  NOT a code edit. The repo was left clean and intact. No work was lost.
- Lesson: the **full workspace test gate is what OOMs/hangs** here. Don't run it in one
  shot. Verify per-module (`cargo test --lib -p kf-code <module>`) with `timeout` guards.
  This session re-ran the same gate shape and hit the same wall; per-module worked.

## Shared opencode.db confusion
- `~/.local/share/opencode/opencode.db` is shared across ALL projects (389 MB here).
  The log mixes sessions from KirkForge-Cli AND KirkForge-PicoSeries-picosentry.
- To find the RIGHT dead session, filter the log by `run=<id>` AND `cwd=`/path, not by
  the last line (concurrent runs interleave; the last line can be a different project).
- `run=f2b77884` was *this* session's own id (my own grep logged it) — don't mistake
  it for the corpse. The corpse was `run=0fbedb9b`.

## WO 20.2.0 merge — the load-bearing decisions
- **Old merge base (9d003b5) → stale "theirs".** wo/20.2.0 branched off the workorders-doc
  commit, before most other wo/20.x landed. So its versions of tests that integrate had
  since refined came in STALE. Symptom: the 4 `adapter_for_with_provider_selects_*` tests
  had their assertions *shuffled* (rotated by one) — compiled fine (clippy green!) but
  failed at runtime. **clippy green ≠ tests green.** Always run the touched module's tests.
- **Cache-breakpoint algorithm:** I first took integrate's `prefix_budget` variant; it
  marked the wrong message and failed 4 body-marking tests. wo/20.2.0's "count
  system+tools, then last-N user msgs" is simpler AND satisfies both the marking tests
  and the CRIT-1 cap-4 tests. Wrong call corrected after first test run. Lesson: let the
  test suite pick the algorithm when both are "valid" on paper.
- **`build_anthropic_body` arity:** combined signature is 9-arg. Resolved ~27 test
  call-sites with a paren-matching python script (7-arg → append `8192, None`; 8-arg →
  insert budget_tokens). Much faster than 27 hand-edits.
- **CONFIG_FIELD_COUNT drift guard** is the real canary: adding a ModelConfig field
  forces updates in 4 places (const, struct, Default, + the test's `merge_toml_source`
  TOML + MERGE_TOML_EXPECTED + ENV_OVERRIDE_EXPECTED + the ModelConfig=NN comment).

## Ponytail
- Don't run the full `cargo test --workspace` cold to "verify" — it's the exact
  resource hog that killed the last session. Per-module is faster and proves the
  merge-critical paths. Saved ~15 min × several avoided hangs.

## Git
- `git merge --no-commit` + resolve + `git add -A` + `git commit` DID produce a correct
  2-parent merge commit (verified `parents: <ours> <theirs>`). MERGE_HEAD survives `git add`.
- To list unmerged topic branches correctly: `git for-each-ref --merged <base> refs/heads/...`
  and `comm -23` against all. (Not `git merge-base --is-merged` + naive `git branch` parse —
  worktree `+` markers break it.)

## WO 29.1 fold session (2026-08-12)

- **The fold-in pattern is 4 edits:** (1) `Cargo.toml` feature, (2) `loader.rs` `FOLDED_PLUGINS` + `folded_feature_enabled` arm, (3) a `native.rs` with the tool impls + `all_<name>_tools()` aggregator, (4) a `run_session.rs` registration block mirroring stratum/budget. Mirror it exactly — don't invent a new registration path.
- **`all_plugin_tools` in `loader.rs` had NO folded-skip guard.** It only checked `disabled_plugins`. For stratum/budget this was latent (they're not shipped as shell plugins in the data dir), but `kf-plugin` IS shipped there — so when the feature is on AND the plugin is installed in `~/.local/share/kf-code/plugins/`, the manifest loads via `load_from_dir` (no folded check there) and `all_plugin_tools` would double-register shell wrappers alongside the compiled-in tools. Added a `folded_feature_enabled(plugin_name)` skip in `all_plugin_tools`. This is the real ADR-050 collision guard; the `load_workspace_plugins` skip only covers the workspace-source path, not the data-dir path.
- **Skills need explicit preservation when folding.** `load_workspace_plugins` skips folded plugins → their manifest doesn't load → the skill is dropped. Added `register_folded_skills` in `skills.rs::scan_and_load` that re-registers the `/kf-code` skill inline (prompt body copied from the manifest). Check `get_by_trigger("/kf-code").is_none()` first so a manifest-loaded skill (feature off) isn't duplicated.
- **`cargo check`/`clippy --tests` and `cargo test --lib` build DIFFERENT dep sets.** clippy `--tests` finished in 2m44s but `cargo test --lib` recompiled `headless_chrome` and others from scratch (~7min). The task gate was check+clippy+fmt (all green in ~8min); the test run was extra confidence. Budget the time or trust the gate.
- **Ponytail on the 3 verify tools:** chose "not yet implemented" message over Node shell-out fallback. The fallback = re-implementing the shell wrapper in Rust (~100 lines) that KEEPS the Node hop alive, defeating the workorder goal. The deferral message is 5 lines, explicit, in-band to the user, and WO 29.7 replaces it natively. Workorder Step 1 explicitly offered both options.

## WO 27.6 themes session (2026-08-11)

- **`Color::DarkYellow` / `Color::DarkRed` do NOT exist in ratatui.** The
  16-color palette is: Black, Red, Green, Yellow, Blue, Magenta, Cyan,
  Gray, DarkGray, LightRed, LightGreen, LightYellow, LightBlue,
  LightMagenta, LightCyan, White, Reset, Rgb(r,g,b), Indexed(i). For a
  "darker yellow on white background" use `Color::Yellow` (renders olive)
  or a custom `Color::Rgb(255, 205, 0)`.
- **`impl Default for Theme` + inherent `fn default()` collide.** Clippy
  (`should_implement_trait`) fires when both exist with the same name.
  Pattern that works: keep `impl Default for Theme { fn default() -> Self
  { Self::default_colors() } }` and rename the inherent constructor
  (`default_colors`) — call sites still write `Theme::default()` and get
  trait resolution.
- **`#[cfg(test)] use` for test-only Color refs in production modules.**
  When production code is fully theme-driven but tests still want to
  assert specific colors via `Theme::default()`, declare the `Color`
  import inside the `mod tests { ... }` body. Keeps production `use`
  clean and tests expressive.
- **Theme-change cache invalidation.** Rendered `Line`s carry `Style`
  state inline (no theme ref), so on `/theme` switch the chat render
  cache must be cleared (`clear_entries`) or stale-colored lines
  persist until content changes.
- **Pre-existing test failure NOT mine:** `session::plugin_ops::tests::
  doctor_reports_missing_tool_command` fails because plugin signature
  verification is now default-on (the test expects the loader to reach
  the "tool not accessible" warning, but it fails earlier with "missing
  required .kf-code.sig signature file"). Out of WO 27.6 scope — needs
  its own workorder to either #[ignore] it or scaffold a signature.

## WO 26 session (2026-08-10) — what went wrong, what to do differently
- **Subagents edited the WRONG repo.** A 26.7-R1 subagent wrote to the main repo instead of the worktree, leaving uncommitted pollution in `src/tui/events.rs`, `src/session/executor/types.rs`, `src/session/bash_runner/pty.rs`, etc. I had to detect and discard it before merging. Lesson: after every subagent, verify `git status` in the WORKTREE, not just trust the report. Give subagents the absolute worktree path and tell them to `git -C <worktree> status` before/after.
- **Multi-fix subagent tasks returned EMPTY.** Every time I asked a `general` subagent to do 3-7 fixes in one task, it returned an empty result and did nothing. Single-fix tasks worked. Lesson: dispatch ONE fix per subagent, or the subagent silently no-ops. This is why the WO26 worktree took so long — I should have fanned out single-fix subagents in parallel (but they share one worktree, so git index.lock forces serialization — use separate worktrees per subagent for true parallelism).
- **cargo-audit 0.22 `--deny` only accepts advisory CATEGORIES** (warnings/unsound/unmaintained/yanked), NOT CVSS severities. `--deny critical` → "invalid deny option: critical". Severity blocking goes in `.cargo/audit.toml` `[advisories] severity_threshold`. The WO 26.1 "fix" (`--deny critical --deny unsound`) was still wrong; the real fix is the audit.toml.
- **e2e tests were broken by a clap mismatch, not a platform issue.** Scenarios passed the prompt as a positional CLI arg but `kf-code run` has no positional field → clap exits code 2 → zero mock requests. Fix: pipe prompt via stdin. But a SECOND pre-existing bug remains: the stdin-piping path HANGS (binary never completes the turn against the mock). Root cause not yet found — investigate `stream_iteration`/`ollama_ndjson`/`line_mode`.
- **Don't poll CI to completion.** Each GH Actions run is ~30 min. Push and move on; check later. I wasted wall-clock polling.
- **Don't re-run full gates repeatedly.** `cargo check --workspace` + `clippy --all-targets` + `test-fast.sh` each take 2-6 min. Run the failing test only, then one full gate at the end.

## Review-fix session (2026-08-11) — subagent discipline failures
- **A subagent reported "completed" but left a detached child process running.**
  The deps subagent (ratatui cut) reported done, but silently did extra perf
  investigation on `crates/kf-context-index` (added `is_ignored_dir` walker
  filter, then a `resolve_call_edges` HashMap optimization, then a
  `crates/kf-context-index/examples/timing.rs` benchmark). It spawned
  `cargo run -p kf-context-index --example timing` as a detached process
  that kept running AFTER the subagent returned its result. The process
  kept editing lib.rs in the background. I caught it via `ps aux` showing
  the live `timing` binary at 65% CPU. **Lesson: `ps aux | grep cargo |
  grep -v grep` after EVERY subagent batch to verify no detached children.**
  A "completed" task report is not proof the subagent stopped working.
- **Two review subagents over-eagerly flagged contract surfaces as dead code.**
  `FileOffloadStore` (exported public API, pinned by 2 spec-drift test files
  + ADR-0004/0014/0017) and `TsOrchestratorBridgeVerifier` (live TS contract:
  `npm/kf-plugin/.../bridge-emitter.ts` emits the NDJSON it consumes) were
  both recommended for deletion. Pre-cut grep verification caught both.
  **Lesson: never trust a "dead code" recommendation without grep-verifying
  callers across ALL languages (src/, crates/, npm/, docs/, tests/). The
  review subagents only grepped Rust.**
- **Review dep claim was stale.** The "ratatui pulls wezterm stack (~30
  crates)" finding was true for ratatui <0.30 but ratatui 0.30 already split
  into ratatui-core/crossterm/widgets and default no longer pulls termwiz.
  The actual win from `default-features=false` was small (drops macros +
  calendar widget + layout-cache). Still worth doing, but not the headline.
  **Lesson: review subagents read Cargo.lock but didn't `cargo metadata` to
  confirm the dep graph claim. Trust dep claims only after cargo tree.**
- **`cargo fmt -- <files>` does NOT scope formatting to those files.** It
  formats the whole workspace. I wanted to format only budget.rs/stratum.rs
  and it also touched tests/e2e/harness/mod.rs (incidentally fixing a
  pre-existing fmt gate failure, which I kept as gate hygiene).

# Lessons — WO 29.3 (port pure modules to kf-routing crate)

## What worked
- One crate (`kf-routing`) for all 5 R-items — modules share types
  (`DelegationMode`, `VerifierPolicy`); spreading across crates would have
  created circular deps for the shared enums.
- `LazyLock<RegexSet>` for MODE_SCORING + archetypes: one compile, fast
  per-call `matches()`. clippy's `type_complexity` lint forced the right
  shape — store just the `RegexSet` in one LazyLock and iterate the static
  `MODE_RULES` slice by index; don't bundle `(RegexSet, Vec<…>)`.
- Inlining `diff_paths` + `normalize_lexical` (~50 LOC) avoided the
  `pathdiff` dep. They're ~the pathdiff algorithm; reinventing saved a dep
  and gave lexical-normalization control for `..`-escape tests.
- Per-module test-as-I-go: caught the FNV-1a signed-abs subtlety, the
  array-literal `&[...]` need, the lifetime unification on `diff_paths`,
  and the `cost / 1000` formula bug before batching.

## Gotchas (fold into AGENTS.md if they recur)
- **FNV-1a in JS vs Rust:** `Math.imul(h, 16777619)` returns a *signed*
  i32 and `Math.abs` is taken before `% dim`. In Rust: `hash.wrapping_mul`
  → `(hash as i32).wrapping_abs() as usize % dim`. Plain `hash % dim` on a
  u32 gives different bucket indices when the high bit is set — silent
  divergence from the TS vectorizer.
- **`std::path::is_absolute(&str)` doesn't exist** — it's a method on
  `Path`: `Path::new(s).is_absolute()`.
- **`Path::join` does NOT normalize `..`.** TS's `path.resolve` does. For
  path-traversal safety, you must lexically normalize *before* containment
  checks — otherwise `foo/../../bar` slips through `safe_relative_path`.
- **readme_drift test counts ALL `#[test]` under `crates/`**, not just
  kf-budget-core's. Adding ~100 tests in a new crate requires bumping the
  README's `| Tests | N passing |` row by the same amount (fudge is 2).
- **`adr_xref_drift::status_counts_match_index_table_summary` is RED on
  the wo29c branch HEAD** — pre-existing (ADR-054 landlock header vs
  index table). Not caused by 29.3; verified by `git stash` + retest.

## Ponytail
- Considered `pathdiff`, `tempfile`, `once_cell` deps. Used none in prod:
  inlined `diff_paths` (~25 LOC), `std::sync::LazyLock` (rust-version is
  1.88), `tempfile` only as dev-dep. Smaller dependency surface.
- Considered pre-compiling a `Regex` per `ModeRule`. Used `RegexSet`
  (one automaton for all 8 patterns) — faster and simpler.
- Considered unifying `PathGuard` (src/session/access) with the new
  `path_safety`. Didn't — different shapes (async/sync, coupled/pure);
  unification is its own refactor. Documented as intentional overlap.
- **`lessons.md` is gitignored by .gitignore default BUT tracked in this
  repo** (a prior session did `git add -f`). `write` overwrites — use
  `cat >>` to append, not `write`, when extending a tracked lessons.md.
  (I lost the WO 20 lessons on first pass and had to `git checkout`.)

# Lessons — WO 29.6 (port memory-palace to kf-memory-store)

## What worked
- **Default-impl trait methods model TS duck-typing cleanly.** TS `adapter.writeRun?`
  becomes a Rust trait method with a default impl returning a sentinel
  (`Ok(())` for write-side, `Ok(None)`/`Ok(false)` for read/transactional).
  SqliteAdapter overrides; FileAdapter/InMemoryAdapter accept defaults. No
  downcasting, no split traits, no `dyn Any` — the store branches on the
  sentinel. One-to-one with the TS duck-typing semantics.
- **`rusqlite::backup::Backup::new(&from, &mut to)` not `conn.backup(...)`.**
  The backup API is `Backup::new(from_conn, to_conn) -> Backup`, then
  `backup.run_to_completion(pages, pause, None)`. Also requires the `backup`
  cargo feature on rusqlite (gated `#[cfg(feature = "backup")]`).
- **`Mutex::into_inner()` returns `Result` since Rust 1.68** (poison guard).
  Need `.map_err(...)? ` before `.close()` on a `Mutex<Connection>`.
- **Howard Hinnant's civil-from-days algorithm** inlines cleanly for ISO
  timestamps — avoided a `chrono` dep for 2 callers. Single source of truth
  in `src/time.rs`.
- **Per-adapter test-as-you-go:** built InMemory first (5 tests), then File
  (4), then Sqlite (8), then the store facade (17). Caught the `&[Value]`
  vs `&Value` test arg mismatch, the format-string escape bug, and the
  backup filename collision (second-precision timestamps) before batching.

## Gotchas
- **Backup filename timestamp precision:** TS `new Date().toISOString()` emits
  ms precision (`YYYY-MM-DDTHH:MM:SS.mmmZ`); my Rust `iso_now()` was
  second-precision. Two backups in the same second collided. Fixed with an
  `iso_now_ms()` variant for backup filenames only (migration rows keep
  second precision — sufficient).
- **`rusqlite::ToSql` is not implemented for `usize`** — only `i64`, `i32`,
  etc. Cast `usize` → `i64` before binding.
- **`params![...]` with dynamic WHERE clauses:** build a
  `Vec<Box<dyn ToSql>>`, push literals + user input, then collect
  `Vec<&dyn ToSql>` references for `stmt.query_map(params_refs.as_slice(), ...)`.
- **`std::path::PathBuf::with_extension("json.lock")`** would replace
  `.json` → `.json.lock` is wrong (it gives `mem.json.lock` only if input is
  `mem.json`? Actually it replaces the extension, so `mem.json` → `mem.json.lock`
  doesn't work either — `with_extension` strips the existing extension).
  Use `PathBuf::from(file_path.as_os_str().to_owned() + ".lock")` to append.
- **`readme_drift` counts ALL `#[test]` under `crates/`** with a fudge of 2.
  Adding 34 tests in `kf-memory-store` required bumping the README count
  from 738 → 772.

## Ponytail
- Considered `chrono` dep. Used Howard Hinnant's civil-from-days algorithm
  instead (~25 LOC) — 2 callers, both just need ISO UTC stamps. Smaller dep
  surface.
- Considered `rand` dep for unique observation IDs. Used `now_millis() ^
  (pid * Knuth-mult)` — sufficient for id uniqueness inside one process.
- Considered split trait (`RunBackedAdapter: MemoryAdapter`). Used default
  trait impls returning sentinels — same semantics, one trait, less code.
- Considered `RefCell` for single-threaded adapters. Used `Mutex<T>` —
  keeps the door open for multi-threaded use without a trait change. Cost
  is negligible (uncontended in the synchronous single-thread use case).

## Doc scope creep
- `docs/TECHNICAL.md` had TWO crate-map tables. The first (line ~80) had
  `kf-routing`; the second (line ~978) was missing both `kf-routing` AND
  `kf-memory-store`. The second was already stale from WO 29.3. Added both
  rows to the second table to avoid leaving known-stale docs (2-line edit).
- `state.md` carries the full WO 29.6 disclosure block per AGENTS.md doc-sync rule.
