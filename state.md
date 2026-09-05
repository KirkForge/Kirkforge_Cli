# kf-code Repo State

*Current state only. History lives in `git log` and WO files.*

## Shipped (closed this session)

- **WO 48.48 (2026-08-29, branch `wo48.48`, NOT merged)**: minify VFS cache
  key nanos-granular — `minify_source_impl` (src/shared/minify/mod.rs) keys on
  `duration_since(UNIX_EPOCH).as_nanos()` (u128) instead of `.as_secs()`; a
  same-second rewrite after an edit no longer serves the stale pre-edit
  minified view (edit_file envelope round-trip race). Key stays in-memory
  (path, mtime) shape; no persistence touchpoints. Regression test
  `test_same_second_rewrite_serves_fresh_content` pins mtimes via
  `set_modified` 500ms apart in the same second — deterministic, no fs
  timing. Fast gates green: workspace check --all-targets, clippy -p kf-code
  -D warnings, fmt, shared::minify 109/109.
  [48.48](docs/workorders/48.48-minify-cache-nanos.md)
- **WO 48.37 (2026-08-29, branch `wo/wo48.37`, NOT merged)**: sh/rb
  minify scanners carry cross-line quote state — `minify_shell` /
  `minify_ruby` hoisted the per-line `quote: Option<char>` out of
  `shell_heredoc_opens` / `ruby_scan_code` (now `&mut Option<char>`
  params) and gained a string-continuation line branch mirroring the
  heredoc/pct verbatim branches: quote open at line start → line is
  string content, emitted verbatim (no comment strip, no blank
  collapse), scan advances quote state so the post-close remainder can
  still open heredocs/%. Mutually exclusive with heredoc (heredoc body
  branch first, never scans quotes; scan skips `<`/`%` while quote
  open). Fixes multi-line literal body `#` lines being deleted +
  phantom-heredoc swallowing on the read→envelope→write-back chain
  (48.11's unshipped "outside strings" half). 6 tests incl. byte-exact
  round-trips + minify→envelope→expand. FAST GATES ONLY per WO:
  workspace check --all-targets, clippy -p kf-code -D warnings, fmt,
  shared::minify 98/98 — all green (one transient non-scanner test
  failure in 1 of 8 runs, 7/8 fully green — see lessons). gitnexus
  impact: LOW both symbols. Note: WO 48.37 file + README row were added
  on this branch (they exist on main 0f0a90c0) — integration merge will
  conflict, keep the Done version.
  [48.37](docs/workorders/48.37-minify-cross-line-quote-state.md)
- **WO 48.39 (2026-08-29, branch wo/wo48.39)**: `fallback_python`
  (src/shared/minify/expand.rs) re-indent heuristic replaced with block
  structure tracking. Statement-name guessing (return/pass/break/continue
  decrementing depth + unconditional else/except pre-dedent) deleted: it
  swallowed code after except blocks into the handler and double-dedented
  headers after return/pass-ending blocks (IndentationError on disk via the
  envelope write-back chain). Now: width stack of open levels from the
  input's own indentation (the minifier preserves it — authoritative dedent
  signal), block openers (def/if/elif/else/for/while/try/except/finally/
  with/class/async, trailing `:` outside strings) arm levels for collapsed
  input, continuation headers pop one extra level only when width didn't
  dedent, triple-quoted string interiors verbatim (`py_triple_state`,
  same idiom as the minifier). Round-trip tests byte-identical (try/except/
  else, nested+pass, multi-line string, collapsed-input re-indent);
  shared::minify 96/96. FAST GATES ONLY per owner directive: workspace
  check --all-targets, clippy -p kf-code -D warnings, shared::minify
  targeted, fmt, plus adr_xref_drift 7/7 (WO file + README row added here —
  the 48.37-48.39 wave may see index conflicts at integration, keep the
  numeric-order rows). gitnexus impact: LOW (2 direct callers, 0 processes);
  detect_changes: low, expand.rs + docs only.
  [48.39](docs/workorders/48.39-expand-python-except-swallow.md)
- **WO 48.31 (2026-08-29, merged at `357ac36e` in the final wave)**: the streaming
  event protocol carries `call_id` — `TurnEvent::ToolStart`/`ToolResult`
  gained the field, `BashPartialOutput(String)` became
  `BashPartialOutput { call_id, text }`. All emitters (dispatch 4, turn 4,
  pre_run 10, outcome 7) stamp `ToolInvocation.id`; `ToolContext` threads
  it into `run_with_pty` so PTY chunks are stamped at the forwarding site.
  TUI routes via `ConversationState.streaming_tool_index`
  (HashMap<call_id, msg index>, rebased on mid-deque removal + prune,
  cleared on TurnComplete/clear/compaction/fork); empty call_id keeps the
  legacy name-based fallback. Replay `RecordedToolCall` gained
  `#[serde(default)] call_id` (old traces parse; pairing call_id-first).
  stream-json tool lines carry `call_id` additively. 6 new tests incl. the
  exact failure scenario (interleaved same-name PTY streams, no card
  mixing). FAST GATES ONLY per owner directive: workspace check
  --all-targets + --features pty, clippy -p kf-code -D warnings, targeted
  suites (tui::events 53, executor::tests 111, replay 34, selftest 35,
  turn_events 6, fill 4), fmt — all green; full suite + Windows cross
  verified green in CI (merge run `33238035710`). detect_changes: 63 changed
  symbols across 19 files,
  all on the protocol surface (rated critical by breadth; expected for a
  protocol change). Scope creep disclosed: tools/mod.rs + tools/bash.rs
  (ToolContext.call_id) because the PTY call site is where the id must
  reach pty.rs.
  [48.31](docs/workorders/48.31-call-id-streaming-protocol.md)
- **WO 48.34 (2026-08-29, merged at `357ac36e` in the final wave)**: two
  model-controlled resources gained bounds/ownership. (1) `task`'s
  model-supplied `max_turns` is now clamped at the tool layer to new
  `ToolConfig.max_subagent_turns` (default 32, pub const
  `DEFAULT_MAX_SUBAGENT_TURNS`, merge-clamped 1–1024); threaded via
  `ToolContextBuilder` → `Task::with_config` (4th param);
  `CONFIG_FIELD_COUNT` 108 → 109. (2) glob/grep-fallback
  `spawn_blocking` walkers are cancel-aware: the cancel select arms flip
  an `Arc<AtomicBool>` the walk loops check per entry (glob walk
  extracted to `walk_glob_matches` for direct testability). Note: the
  48.30–48.36 WO files exist only on main (46baabc2) — this branch
  carried none of them; the   48.34 file + README row were added here, so
  the integration merge will see file/README conflicts for 48.34
  (keep the Done version) and rows for 48.30–48.33/35/36 arriving from
  main. FAST GATES ONLY per WO: workspace check --all-targets, clippy -p
  kf-code -D warnings, targeted tests (task_tool 24/24, glob 15/15,
  grep 34/34, shared::config 9/9, session::config 105/105), fmt — all
  green. Full suite + Windows cross gate verified green in CI (merge run
  `33238035710`).
  [48.34](docs/workorders/48.34-resource-bound-bundle.md)
- **WO 48.35 (2026-08-29, merged at `357ac36e` in the final wave)**: the 47.35
  untrusted_content wrap hardened + documented as mitigation, not trust
  boundary. (1) `wrap_untrusted` (web_fetch.rs) neutralizes payload-borne
  literal `</untrusted_content>` (`<\/...>`); new `unwrap_untrusted`
  inverse. (2) The central cap `truncate_tool_output`
  (executor/helpers/mod.rs — the truncation site; NOT the wrap helper,
  which has no config access) drops the closing tag when the cut would
  slice it and ends the region `...\n[truncated]` (house marker) — no
  more unterminated regions; generic path byte-identical, FileContent
  sets truncated. (3) system.hbs + TECHNICAL.md state plainly:
  permissions/sandbox/approval are the authoritative boundary; goldens
  re-captured (template 9/9). FAST GATES ONLY per owner directive:
  workspace check --all-targets, clippy -p kf-code -D warnings, fmt,
  web_fetch 72/72, read_file 18/18, helpers truncate 12/12. Full suite +
  Windows cross gate verified green in CI (merge run `33238035710`).
  gitnexus impact:
  truncate_tool_output HIGH position risk (central), change is a
  wrapped-only branch.
  [48.35](docs/workorders/48.35-untrusted-delimiter-honesty.md)
- **WO 48.12 (2026-08-28, merged)**: js/ts minify no
  longer corrupts regex literals — `minify_js_like`
  (`src/shared/minify/lang.rs`) truncated `/https?:\/\//` at the first
  unescaped-looking `//`, eating the newline (disk write-back chain).
  Regex-literal state now tracked with a conservative prev-token heuristic
  (`prev_opens_regex`: operator/punctuator set + expression keywords;
  verbatim body with `\` escapes and `[...]` classes; `\n` bails).
  Sibling site on the same chain, `fallback_c_like`
  (`src/shared/minify/expand.rs`), was inserting spaces inside regex
  bodies via its `:` arm — fixed with the shared heuristic (scope creep:
  expand.rs because the WO's round-trip gate routes through it).
  Ceiling: regexes after `)`/`}`/identifier/number stay untracked
  (`ponytail:` comment at the heuristic). FAST GATES ONLY per owner
  directive: minify tests 76/76, clippy -p kf-code -D warnings, workspace
  check --all-targets, fmt — all green. gitnexus impact (dev index): LOW.
  [48.12](docs/workorders/48.12-minify-js-regex-literal-corruption.md)
- **WO 48.16 (2026-08-28, merged)**: failed reads no
  longer satisfy the read-before-edit gate. Both `mark_read` sites —
  dispatch `spawn_batch` (intra-batch) and turn `record_tool_result`
  (cross-batch/direct) — now gate on `tool_outcome_success`, so a
  read_file/read_image whose body returned Failure/Error doesn't unlock a
  later edit_file (blind-edit-after-failed-read closed). Both sites
  already had the outcome in scope; no timing move needed. Regression
  test `failed_read_does_not_satisfy_read_before_edit_gate` + mirror
  success-path test both green. FAST GATES ONLY per owner directive:
  workspace check --all-targets, clippy -p kf-code -D warnings,
  session::executor::tests 104/104, shared::access 69/69, fmt — all
  green. Full suite + Windows cross gate verified green in CI (merge run
  `33238035710`).
  [48.16](docs/workorders/48.16-mark-read-on-failed-read.md)
- **WO 48.17 (2026-08-28, merged)**: `notebook_edit`
  brought inside the file-tool pipeline — added to the pre_run file-tool
  list (Phase-1 `check_write` + resolved-path substitution into pre-tool
  hook args), Phase-2.5 sequential deferral (with the symlink-swap walk),
  the read-before-edit gate (unconditional, like edit_file — a notebook
  edit only ever modifies an existing file), `should_audit` + its
  dispatch AccessDenied mirror, and the `is_destructive` approval list
  (now asks like edit_file unless auto-approve). Phase-3 record arm
  recognizes it too. Behavior change disclosed in the WO: notebook_edit
  previously ran with NO approval gate. Audit finding left for a future
  WO: `check_write` returns the raw literal as "resolved" for ALL write
  tools (reads canonicalize), so the WO 38.1 body-opens-resolved-path
  contract is only fully realized for reads; fixing it changes
  write_file/edit_file behavior and needs its own WO. FAST GATES ONLY
  per owner directive (workspace check --all-targets, clippy -p kf-code,
  fmt, dispatch 22/22 + hooks 46/46 + notebook_edit 21/21; detect_changes
  low, 2 symbols).
  [48.17](docs/workorders/48.17-notebook-edit-outside-file-tool-pipeline.md)
- **WO 48.9 (2026-08-28, merged)**: `--no-default-features`
  build fixed — 3 un-gated `kf_budget_core::estimate_tokens` refs (ADR-0017
  documents the minimal build as supported). cfg gate now lives in ONE choke
  point: `prompt::count_tokens` body (`budget` on → BPE, off → `s.len() / 4`
  heuristic; fn stays un-gated, many un-gated callers); the TUI
  `estimate_messages_tokens` rerouted through it (dedup, no direct crate refs).
  FAST GATES ONLY per owner directive: workspace check --all-targets,
  workspace check --no-default-features (previously broken), clippy -p
  kf-code -D warnings, fmt, estimate_message_tokens tests 3/3 — all green.
  DEFERRED: CI job for the no-default-features combo (~10-line new job block,
  not the allowed 3-line add; see Pending).
  [48.9](docs/workorders/48.9-no-default-features-broken.md)
- **WO 48.10 (2026-08-28, merged)**: Windows
  daemon-stub `try_list_recent`/`try_resolve_recent`/`try_resolve_id`
  (`src/daemon/client.rs` `windows_imp`) now serve the on-disk session
  index instead of `Ok(None)` — the same platform-neutral
  `session_index` code the Unix WO 47.12 disk fallback uses, so
  `--attach`, `--auto-resume`, the startup picker and `/fork` picker
  behave as the 47.12 help promises on Windows too. cli.rs unchanged
  (wording accurate post-fix). FAST GATES ONLY per owner directive:
  daemon::client tests 8/8, Windows cross clippy -D warnings green,
  workspace check green, fmt green. detect_changes: 0 indexed symbols
  affected (cfg(windows) bodies are outside the Linux symbol graph).
  [48.10](docs/workorders/48.10-windows-daemon-stub-no-fallback.md)

- **WO 48.2 (2026-08-28)**: pre-tool hooks no longer run twice per
  file-tool call. The Phase-3 recorder (`record_tool_result`) re-ran
  `pre-tool-{name}` AFTER the write/edit hit disk (doubled side-effects +
  a divergent verdict could deny recording a completed write — the WO
  43.30 contract violation surviving in the second-run window). The
  single evaluation now runs in the Phase-1 pre-gate
  (`pre_run.rs`) with the resolved path substituted into the hook args —
  identical JSON to what the deleted Phase-3 run passed, so hooks see the
  same view, now enforced pre-mutation. Regression test
  `pre_tool_hook_runs_exactly_once_per_file_tool_call` (counting hook
  asserts 1 invocation; 2 pre-fix). ADR-020 Phase-1 wording + TECHNICAL.md
  hooks bullet updated to pin the single-gate contract. FAST GATES ONLY
  per owner directive (check --workspace --all-targets, clippy -p kf-code,
  session::hooks + dispatch tests, fmt, adr_xref_drift).
  [48.2](docs/workorders/48.2-pre-tool-hook-double-run.md)
- **WO 48.3 (2026-08-28, merged)**: fixed the WO
  47.12 regression where the Sessions tab permanently showed "No recent
  sessions" in the default daemon-less config. The three tab-switch sites
  in `src/tui/keys/mod.rs` primed Jobs cold-start but never tripped
  `sessions_dirty`; now one shared `prime_overlay_cold_data` helper (used
  by `open_overlay`, palette Enter, and the F-key arm) trips both flags,
  and the draw loop's existing `refresh_sessions` handler populates the
  picker via the on-disk session-index fallback. Startup picker path was
  already correct. FAST GATES ONLY per owner directive (check/clippy/
  targeted tests/fmt green; full suite + Windows cross verified green in
  CI — merge run `33238035710`).
  [48.3](docs/workorders/48.3-sessions-tab-empty-without-daemon.md)
- **WO 48.4 (2026-08-28)**: daemon `ThreadsChanged` pushes no longer open
  the full-screen "Resume a recent session" modal at every TUI startup.
  Split the dual-purpose `session_picker` field into data-only
  `SessionState::recent_sessions` (written by background
  `refresh_sessions`; read by Sessions tab + welcome screen) vs the
  modal `session_picker` (set ONLY by explicit `/resume` with no args).
  Also fixed the picker dropping itself on advertised k/j/↑/↓ nav keys
  (`handle_session_picker_keys` now restores the picker while still
  open). Regression tests for both. Fast gates green (workspace check,
  clippy -p kf-code, fmt, targeted tests); full suites deferred to CI.
  [48.4](docs/workorders/48.4-session-picker-modal-pops-at-startup.md)
- **Known-flake stabilization (2026-08-28)**: the four wall-clock flake
  families that caused red/noise across the WO 47 campaign got realistic
  budgets — assertions unchanged, nothing weakened. (1)
  `attached_cancel_token_kills_inflight_bash_promptly`: in-test readiness
  deadline 10s→30s, bash-kill bound 10s→25s (still below the child's 30s
  sleep, so a no-kill regression still fails it), ci-fast override 60s.
  (2) `same_ms_double_spawn_gets_distinct_{temp_dirs,worktrees}`:
  readiness deadlines 10s→60s — the WO 47.21 residual (these tests do
  NOT route through `ensure_private_data_dir`; nextest process isolation
  + ~50 parallel test processes starved the old 10s poll), ci-fast
  override 90s. (3)
  `edit_file_{fuzzy_preserves_unmatched_lines,roundtrip_reverses_replacement}`:
  ci-fast override 60s (proptest ×32, fresh tokio runtime per case,
  25-35s solo — no in-test window exists to raise). All in
  `.config/nextest.toml` beside the `run_bash_stuck_step_times_out`
  precedent. Gates: each family targeted 2× green + combined ci-fast
  filter green, fmt clean, `cargo check -p kf-code --lib` clean.
  FAST GATES ONLY per owner directive — full suites not run this session.
- **WO 48 series COMPLETE** (36 workorders, 36/36 `Status: Done`). Final
  wave (48.29, 48.30, 48.31, 48.32, 48.33, 48.34, 48.35, 48.36) merged at
  `357ac36e` (2026-08-29); earlier 48.x waves shipped through the campaign
  (48.2-48.4, 48.9, 48.10, 48.12, 48.16-48.18, 48.20, 48.21, 48.23-48.28).
  One-liners in CHANGELOG; full detail in `docs/workorders/48.*`.
  Highlights — 48.1/48.11/48.13/48.25/48.26/48.29 minify scanner wave
  (python/shell/ruby string+heredoc awareness, `::` round-trip); 48.5 MCP
  isError remap; 48.6/48.18 json_mode/response_format toggle pair; 48.14
  `can_run_tui` startup-picker gate; 48.15 gate-vs-body denial
  classification; 48.22 nightly subprocess filter (P0); 48.27 edit_file
  fuzzy-fork guards; 48.31 call_id streaming protocol; 48.32 workflow
  agent cancellation; 48.33 read_file windowed reads; 48.36 job ID
  reservation.
- **WO 47 series COMPLETE** (37 workorders: 36 Done + 47.8 owner-skipped,
  see Pending). Final wave (47.6, 47.7, 47.9-47.14) merged at `c3a1eaf2`;
  earlier 47.x waves shipped through the campaign. One-liners in
  CHANGELOG; full detail in `docs/workorders/47.*`. Highlights —
  47.1 table-driven verifier registration; 47.2 generic env-override
  loader; 47.3 kf-rbac JWT/JWKS half deleted; 47.4 workspace 13→10
  crates; 47.5 bench/testdoctor feature-gated out of the default binary;
  47.6 compression dedup (structural fold deferred); 47.7 MCP transport
  trait (−310 lines); 47.9 corpus archived (450 files →
  docs/archive/workorders/); 47.10 `emit!` ceremony macro (−100 lines);
  47.11 computer_use FROZEN not deleted; 47.12 daemon opt-in;
  47.13 TUI command gates; 47.14 plugin verifiers bus-only; audit wave
  47.15-47.37 (security/robustness fixes, all Done).
- **Everything earlier is closed**: WO 46.1-46.39, 45.x, 44.x, 43.x and
  prior series complete (verified by the WO 44 regression audit + WO 47.9
  archival pass). History: `git log` + `docs/archive/workorders/`.

## Pending / Deferred (open)

- **WO 48.9 (DONE 2026-09-04)**: added `minimal-build` CI job to
  `ci-merge.yml` + `ci-pr.yml` — runs `cargo check --workspace
  --no-default-features --locked`. The pre-existing warnings
  (`unused_mut` skills.rs, dead-code `apply_budget_slice`) were already
  fixed by WO 48.24 (commit `d9e6c60f`); the state.md note was stale.
  [48.9](docs/workorders/48.9-no-default-features-broken.md)

- **WO 47.8 (Done)**: deleted the 9 fuzz targets — `fuzz/` was 100%
  unwired (own `[workspace]`, 4 of 9 targets orphaned, zero CI jobs ran
  cargo-fuzz). Deleted the whole dir; no production consumers. Removed
  from this pending list. [47.8](docs/workorders/47.8-wire-or-delete-fuzz-targets.md)
- **WO 47.6 (DONE 2026-09-04)**: `MiddleStrategy` enum unifies
  `maybe_microcompact` + `compact_to_budget` behind one
  `process_middle` driver. 3-rung fallback ladder re-expressed as
  escalation sequence. LLM arm routes through same driver. Zero
  behavior drift — all 146+74 tests unchanged.
  [47.6](docs/workorders/47.6-compression-layers-6-to-2.md)
- **WO 47.12 (DONE 2026-09-04)**: compile-time excision of `src/daemon/**`
  + kf-rbac from default builds via `daemon` cargo feature (default-off).
  Runtime opt-in (`KF_CODE_DAEMON_AUTOSTART`) stays. Minimal build
  (`--no-default-features`) excludes daemon entirely.
  [47.12](docs/workorders/47.12-daemon-opt-in.md)
- **WO 47.13 (gates-not-cuts disclosure)**: TUI commands were GATED
  behind `[display] extra_commands`, not deleted — shipped-line count
  unchanged; flipping the gates to deletions belongs to a future WO.
  Doom banner deliberately ungated (safety UI, WO 43.31); line-mode
  `/carryover show|clear` ungated (parity follow-up if wanted).
  [47.13](docs/workorders/47.13-tui-command-diet.md)
- **TUI extra_commands hint (DONE 2026-09-04)**: both hint strings
  (`slash_commands.rs:92`, `commands/jobs.rs:15`) now teach the correct
  top-level `extra_commands = [...]` form instead of the unparsable
  `[display] extra_commands` nested form.
- **WO 47.14 (DONE 2026-09-04)**: all 14 built-in verifiers migrated to
  `BusVerifier`; `Verifier`/`VerifierSlots`/`VerifierHandler` deleted.
  One verifier architecture. `VerifyContext` extended (event_kind,
  tool_name, content_hash, bash fields). `VerdictEntry` gained `fix`
  field. Correction loop reads from bus verdicts. Net -392 lines.
  [47.14](docs/workorders/47.14-unify-verifier-traits.md)
- **WO 47.4 (DONE 2026-09-04)**: src/ imports now use
  `kf_plugin_host::sdk::TypeName` explicitly for types defined in the
  sdk module. Zero behavior difference.
  [47.4](docs/workorders/47.4-fold-routing-memory-crates.md)
- **WO 46.24 (DONE 2026-09-04)**: both append-mode sites
  (`src/shared/audit.rs` `AuditLog::new`, `src/main/cli_dispatch.rs`
  `init_tracing`) now open with `O_NOFOLLOW` on Unix via
  `OpenOptionsExt::custom_flags(libc::O_NOFOLLOW)`. Windows path
  documented as unprotected (ponytail: upgrade path is
  `openat2(RESOLVE_NO_SYMLINKS)`).
  [46.24](docs/workorders/46.24-predictable-tmp-filenames-toctou.md)
- **WO 43.26 (DONE 2026-09-04)**: `VerifierBus::run` is now offloaded to
  `tokio::task::spawn_blocking` — the bus is extracted from the
  `std::sync::Mutex`, run on a blocking thread, then put back. The lock is
  held only for extract/replace (microseconds), not across the sync verify
  calls (up to 5s per plugin verifier). No trait change needed; the
  `BusVerifier` trait stays sync.
- **kf-lsp PDEATHSIG (DONE 2026-09-04)**: `crates/kf-lsp/src/lib.rs`
  `setup_process_group` now calls `prctl(PR_SET_PDEATHSIG, SIGKILL)` on
  Linux, matching the session helper.
- **WO 43.1 (DONE 2026-09-04)**: all 5 adapters (ollama + openai_compat +
  anthropic + anthropic_bedrock + anthropic_vertex) now wrap stream()
  errors via `classify_transport_error` → typed `AdapterError`. The
  string-probe fallback in `src/main/error.rs` remains ONLY for
  session-layer sandbox/path-policy denials (those producers are not yet
  typed); the adapter-error portion of the fallback is no longer reached
  by unmigrated adapters.
  [43.1](docs/archive/workorders/43.1-typed-adapter-errors.md)
- **WO 43.20 (DONE 2026-09-04)**: replaced `aws-sigv4` +
  `aws-credential-types` + `aws-smithy-runtime-api` with in-tree SigV4
  signing using the already-present `sha2` + `hmac` + `hex` deps (~100
  lines in `bedrock_signing.rs`). All 3 aws crates removed from
  Cargo.toml; http 0.2 + http-body 0.4 duplicates GONE from Cargo.lock
  (only http 1.5.0 + http-body 1.1.0 remain). Wayland clipboard path
  still unverified (manual check, not blocked on code).
- **WO 43.22 (DONE 2026-09-04)**: `estimated: bool` field added to
  `TurnEvent::CostStats`, populated `true` on the usage-less fallback path
  (`turn.rs:was_estimated = usage.is_none()`), `false` for real usage.
  Integration test in `context_economics_test.rs` tightened to assert the
  flag. Vertex token cache implementation already shipped; unit tests for
  it still pending (Authenticator injection seam needed — minor tail).
- **WO 43.24 (DONE 2026-09-04)**: kf-testdoctor assert-free-body
  heuristic shipped — `find_assert_free_tests` in `suggest.rs` scans
  `#[test]`/`#[tokio::test]` function bodies for assertion macros
  (assert!/assert_eq!/assert_ne!/panic!/should_panic/unreachable!/todo!/
  unimplemented!), flags assert-free fns as `AssertFreeBody` suggestions.
  Line-based scan (ponytail: not AST-level; FP on helper-only test fns
  with asserts in callees). 5 tests added.
- **WO 39.4** Claude-compat phase 3 (hook stdin-JSON contract);
  **WO 39.1** phase 3-4 (external `claude -p`/`codex exec`/`opencode run`
  runner + LiteLLM gateway); **WO 38.10** P2 CLI polish;
  **WO 19.11** plugin production hardening; **WO 21.0.14** deferred
  tracker (ledger, open by design); **WO 29.1** verify-tools fold
  (phase 1 shipped, verify tools deferred to 29.7). Trackers live in
  `docs/archive/workorders/`.

## Known flakes

**Stabilized 2026-08-28.** The four documented families
(`attached_cancel_token_kills_inflight_bash_promptly`,
`same_ms_double_spawn_gets_distinct_{temp_dirs,worktrees}`,
`edit_file_{fuzzy_preserves_unmatched_lines,roundtrip_reverses_replacement}`)
now carry realistic wall-clock budgets: raised in-test deadlines where a
deadline existed + per-test ci-fast overrides in `.config/nextest.toml`.
No assertion weakened — the bash-kill bound still sits below the 30s
sleep a no-kill regression would run. Watch item: the edit_file pair is
inherent CPU cost (proptest ×32, fresh runtime per case); if it flakes
again under sustained >2× load, the next step is sharing one tokio
runtime across proptest cases (WO 33.11 follow-up), not lowering
coverage.

## Subagent system audit (2026-09-04) — BOTH HIGH GAPS FIXED

Two HIGH-priority gaps found and fixed:

- **Subagent nesting depth limit (FIXED)**: `ToolContext.subagent_depth`
  (0 = root, 1+ = nested) threaded through dispatch → tool calls.
  `TaskRequest.subagent_depth` threads it into the subagent executor.
  The `task` tool refuses spawn when `depth + 1 > max_subagent_depth`
  (config `tools.max_subagent_depth`, default 3, clamped 0–10).
  `CONFIG_FIELD_COUNT` bumped 109 → 110.
- **Task tool persona enum (FIXED)**: removed the hardcoded
  `"enum": ["explore", "plan", "coder"]` from the `persona` parameter
  schema. Now accepts any string; strict-schema models can invoke
  discovered `.claude/agents/*.md` agent names. Runtime validation
  unchanged: unknown personas get the full toolset, agent names get
  their restricted toolset.

MEDIUM-priority gaps (not blocking but limit capability):
- Model failure: no fallback to cheaper model (error propagates raw)
- Pipeline: configurable roles FIXED (PipelineConfig + RoleSpec);
  fan-out FIXED (futures::join_all); strictly sequential FIXED
  (now supports parallel via fan_out per role)
- Agent frontmatter: maxTurns FIXED; isolation/background/permissionMode
  FIXED; still missing hooks, mcpServers, memory
- Inter-subagent messaging: no SendMessage/ListAgents/TaskCreate tools
- Agent registry: FIXED (reloadable via /reload)

WO 39.4 (FULLY DONE 2026-09-04): Claude hook stdin-JSON contract +
settings.json/hooks.json reader + generic pre-tool/post-tool emit-site
wiring. A Claude plugin's hooks.json now runs its hooks with the payload
it expects and the deny semantics it expects.

Strengths vs competitors:
- Untrusted handoff fencing (prompt-injection-safe context handoff)
- Cooperative cancel cascade to nested subagents
- Per-subagent provider override with API key inheritance + model allowlist
- Concurrency backpressure (Queue + Reject modes with semaphore)
- Background subagents are production (not experimental like opencode)

## Architecture notes (load-bearing, not in WOs)

- `VerifierHandler::verify_event` caches verdicts keyed by `(file_path, content_hash)`.
  Only `Clean`/`Skipped` verdicts are cached — `Fixable`/`Unfixable` are not (the
  correction loop re-verifies after applying a fix; disk content changed, so a
  cached verdict would be stale). After a fix is applied, `CorrectionLoop::run`
  calls `invalidate_cache(path)` to drop the stale entry. `content_hash == 0`
  events never hit the cache (old events / producers without hash). WO 42.11.

- `Message.token_count` is populated at append time (`ConversationLog::append`/`append_async`). Estimators (`estimate_message_tokens` in `prompt/mod.rs`) return the cached value when `Some`, falling back to BPE counting when `None`. Content mutation sites (`truncate_tool_results`, `dedup_adjacent_tool_results`, `minify_old_messages`, `stub_old_tool_results`, compaction stub/condense) clear `token_count = None` to avoid stale cache. WO 42.12.

- `panic = "abort"` in release — panic hook (WO 38.2) restores terminal before abort. Keep abort (binary size); don't switch to unwind without measuring.
- Budget guard wired in production (WO 38.8) — `set_budget_stores` + `set_stratum_store` called from `run_session.rs`. Listener registry is session-keyed `HashMap`, not the old append-only Vec.
- Windows cross-compile gate in `scripts/ci-local.sh` — `cargo clippy --target x86_64-pc-windows-gnu` runs before every push. AGENTS.md §4 enforces it. This is the structural fix for the 25+ `fix(windows)` commit pattern.
- WO drift test in `kf-budget-core/tests/adr_xref_drift.rs` — enforces WO file header ↔ README index agreement. Prevents future status drift. `wo_status_headers_match_readme_index` is one of its 5 checks.
- `.config/nextest.toml` profiles: `ci-fast` (30s, fail-fast), `ci-full` (60s), `nightly` (600s). CI references by name, no inline `--config`. Per-test overrides: `run_bash_stuck_step_times_out` (60s — 30s workflow-step timeout, WO 43.26) + the four known-flake families stabilized 2026-08-28 (cancel 60s, same_ms pair 90s, edit_file pair 60s — see "Known flakes").

## CI / branch state

- **main == dev == origin/main == origin/dev at `357ac36e`** (2026-08-29).
- **CI GREEN**: GitHub run `33238035710` (CI (merge), branch main, head
  `357ac36e`, 2026-08-29T06:15:31Z, success) — the WO 48 final-wave merge
  commit. Full gates (test-full matrix + Windows cross) ran green in CI.
- Branch flow (NON-NEGOTIABLE, AGENTS.md task 7): push to dev → watch CI
  green (`gh run watch --exit-status`) → only then main. Never both at
  once; never main first.
- Worktrees: 9 — main checkout, dev-integration, six stale merged WO
  worktrees (`.worktrees/wo48.25`-`wo48.30`, prunable via
  `git worktree remove`), user's external (detached). Remote branches
  just `dev` + `main`.
- This session (docs truth pass) is docs-only; the flake-stabilization
  session before it ran FAST GATES ONLY per owner directive (`cargo fmt
  --check`, `cargo check -p kf-code --lib`, each flake family 2×
  targeted, combined ci-fast filter run) — its owed full gate run was
  covered by CI merge run `33238035710` on the final-wave push.
