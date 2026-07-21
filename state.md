# KirkForge-Cli Production-Readiness State

## Current focus

**UNCOMMITTED — Session 5 (real graph + security verifier emitters) is in the working tree, not on main.** Last commit on `main` is `6d378e5` (Session 4). The Session 5 work — `graph-emitter.ts`, `security-emitter.ts` + tests, `emitter-factory.ts` wiring, `reducer.ts` security-event merge, ADR-007/013/0011/0012/README rewrites, `tool-gitnexus`/`tool-graphify` package deletions, `test-batched.sh` cleanup, README/kirkforge.toml stale-ref removals, Rust `Box::leak`→interner + pollable approval-reader fixes, CI `node-version: "24"` — is ~32 dirty files on `main` (see `git status --porcelain`). It needs committing.

Remaining before commit: the rename is **not complete**. The orchestrator-side `tool-gitnexus`/`tool-graphify` packages are gone and the `graph`/`security` verifier slots are real, but the old `gitnexus`/`graphify` names still live in the plugin capability system (`packages/plugin/src/index.ts`, `core-types`, `core-schemas`, `core-config`, `apps/cli` doctor/tools/bootstrap + tests), `src/session/mcp_client/mod.rs` docs, `src/session/prompt/mod.rs:541-542` (graphify-out forbid), `scripts/impact-fallback.sh`, `CHANGELOG.md`, and `plugins/kirkforge-plugin/README.md`. Count: **35 `gitnexus` + 27 `graphify` refs repo-wide (excl. docs/adr)**. That cleanup is the next P0.

ADR-vs-code (touched ADRs): ADR-007 now matches `SLOT_TO_SIGNAL` (5 slots); ADR-013 now says vendored in-repo (matches reality); ADR-0011/0012 honestly Rejected (matches zero code). CI: both `.github/workflows/ci.yml` and `release.yml` now use `actions/setup-node@v4` with `node-version: "24"` (was 22 / 20).

Session 4 is complete and green. Next up after the commit: Session 6 (rich provider config + web UI architecture). Plan references: `.claude/plans/session-4-bugfix-perf-campaign.md`, `.claude/plans/roadmap-2026-q3.md`.

Sessions:
- Session 1 — cloud-routed frontier defaults + native Kimi adapter ✅
- Session 2 — write-side minification / VFS ✅
- Session 3 — cron / scheduled jobs ✅
- Session 4 — correctness bugs C11–C27 + performance P4–P9 ✅
- Session 5 — rich provider config + web UI architecture

Background: the project is being repositioned as a daily-driver coding agent that routes through Ollama to cloud frontier models (Kimi, GLM, DeepSeek, etc.), not a local-only "potato hardware" client. Session 1 corrected the defaults, routing, and docs and added a native Kimi adapter.

---

## Plugin / satellite integration status

The five satellite plugin manifests now live in-repo under `plugins/<name>/` and are registered as workspace plugin sources by default. They are disabled until toggled on with `/plugins toggle <name>`.

| Plugin | Plugin wrapper | Source location in this repo | Binary/runtime |
|--------|----------------|------------------------------|----------------|
| kirkforge-draw | `plugins/kirkforge-draw/` | `crates/kirkforge-draw`, `crates/kirkforge-draw-core` | `kfd` |
| kirkforge-video | `plugins/kirkforge-video/` | `crates/kirkforge-video` | `kirkforge-video` + FFmpeg |
| stratum | `plugins/stratum/` | `crates/kirkstratum-core`, `crates/kirkstratum-hosts`, `crates/kirkstratum-cli` | `stratum` |
| kirkforge-plugin3 | `plugins/kirkforge-plugin3/` | `crates/plugin3-core`, `crates/plugin3-hosts`, `crates/plugin3-cli` | `plugin3` |
| kirkforge-plugin (SDK) | `plugins/kirkforge-plugin/` | `npm/kirkforge-plugin` | Node.js >= 20; `apps/cli/dist/index.js` |

All satellite source is now vendored in this repo; there is no separate repository required to build or run them.

---

## Open work items — runtime plugin mount/unmount

### Phase 1 — CLI host primitives (no UI change)

1. [x] Add `PluginRegistry::load_one`, `remove`, `rebuild_indexes` in `crates/kirkforge-plugin-host/src/lib.rs`.
2. [x] Add `SkillRegistry::add_plugin` / `remove_plugin` in `src/session/skills.rs`.
3. [x] `Executor::reload_plugins` already rebuilds toolset/hooks/verifiers from a fresh `PluginRegistry`; per-plugin toggles send a full snapshot over `plugin_reload_tx` for correctness.
4. [x] Unit tests for the new registry mutations.

### Phase 2 — `/plugins` slash command family

5. [x] Create `src/tui/commands/plugins.rs` implementing:
   - `/plugins list`
   - `/plugins enable <name>`
   - `/plugins disable <name>`
   - `/plugins reload`
   - `/plugins trust <name> <tier>`
6. [x] Wire `/plugins` dispatch in `src/tui/keys.rs`.
7. [x] Register module in `src/tui/commands/mod.rs`.
8. [x] Reuse existing `plugin_reload_tx` channel to forward updated `PluginRegistry` snapshots.
9. [x] Update `AppState::plugin_status` after each toggle.
10. [x] Unit tests for parsing, list/enable/disable/trust, and plugin status helpers.

### Phase 3 — Satellite packaging (distinct folders preserved)

1. [x] Verify `KirkForge-Draw/plugin/` works when copied into `~/.local/share/kirkforge/plugins/kirkforge-draw`.
2. [x] Verify `KirkForge-Video/plugin/` works when copied into `~/.local/share/kirkforge/plugins/kirkforge-video`.
3. [x] Create a KirkForge manifest + tool scripts for `KirkForge-Plugin2` (Stratum) under a new `plugin/` subfolder.
4. [x] Create a KirkForge manifest + tool scripts for `KirkForge-Plugin3` under a new `plugin/` subfolder.
5. [x] Create a KirkForge manifest + tool scripts for the `KirkForge-Plugin` verification CLI under a new `plugin/` subfolder.
6. [x] Document install path for each: copy `plugin/` folder to `~/.local/share/kirkforge/plugins/<name>` (do not merge repos).

### Phase 4 — Integration tests + TUI verification

1. [x] Automated tests: enable/disable a plugin via `handle_plugins_command`, confirm tools and skills appear/disappear.
2. [x] End-to-end TUI wiring test: `handle_input_key` with `/plugins list` pushes the expected system message.
3. [x] Run `scripts/ci-local.sh quick` on `feature/runtime-plugin-toggle`, `merge`, `dev`, and `main` — all green.
4. [x] Run `scripts/impact-fallback.sh` (GitNexus is currently unavailable; passed).

### Phase 5 — Docs + follow-ups

1. [x] Update `README.md` plugin section with `/plugins` commands.
2. [x] Update `CHANGELOG.md`.
3. [x] Update plugin/satellite status table.
4. [x] Merge `feature/runtime-plugin-toggle` → `merge` → `dev` → `main`; all branches pass `scripts/ci-local.sh quick`.
5. [x] Push `main`, `dev`, and `merge` to origin.
6. [ ] Consider persisting enable/disable state in `~/.local/share/kirkforge/plugin-state.toml` (v2, out of scope for v1).

### Phase 6 — Source-level unification into one codebase

1. [x] Move `KirkForge-Draw` Rust crates into `crates/kirkforge-draw*` and make them workspace members.
2. [x] Move `KirkForge-Video` Rust crate into `crates/kirkforge-video` and fix clippy/fmt issues.
3. [x] Move `KirkForge-Plugin2` (Stratum) Rust crates into `crates/kirkstratum*` and make them workspace members.
4. [x] Move `KirkForge-Plugin3` Rust crates into `crates/plugin3*`; reconcile ADR numbering and remove standalone-workspace drift tests.
5. [x] Vendor `KirkForge-Plugin` Node SDK source into `npm/kirkforge-plugin/`; build and verify plugin tool wrappers locate `apps/cli/dist/index.js`.
6. [x] Update plugin tool `common.sh` helpers to prefer workspace `target/release/` / `target/debug/` binaries, falling back to `PATH`.
7. [x] Run `cargo check --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all -- --check` — all green.
8. [x] Update `README.md`, `state.md`, and `CHANGELOG.md` to reflect that all five satellites are now source-level parts of this repo.

---

## Recently completed

- [x] **Session 5 — Real graph + security verifier emitters; ADR honesty; Box::leak + detached-thread fix (2026-07-17, Task 8).**
  - **Graph emitter** (was a `status:"skipped"` stub at `emitter-factory.ts:39-60`): real regex import-edge extraction in `npm/kirkforge-plugin/packages/orchestrator/src/graph-emitter.ts` → `state.graph` with `edgeCount`/`newEdges`/`brokenEdges`/`cycles`. DFS back-edge cycle detection; broken edge = missing target OR named symbol not exported. Status maps to valid `VerifierStatus` (`skipped`/`fail`/`pass`). Gate tests in `tests/graph-emitter.test.ts`: import cycle → `cycles>=1`, `status!="skipped"`; removed/unexported symbol → `brokenEdges>=1`.
  - **Security emitter** (was `security = resolvedLint` alias at `emitter-factory.ts:89`): real obfuscated dangerous-call scanner in `src/security-emitter.ts` → `verify.security`. Catches bracket-keyed `window["eval"](`/`child_process["exec"](`, string-concat `child_process.exec('ls ' + x)`, `vm.runIn*`, Python `eval`/`os.system`/`subprocess shell=True`/`pickle.loads` — forms the lint safety regex (`no-eval` `\beval\s*\(`, `no-shell-exec` `${}`-only) misses. Gate tests in `tests/security-emitter.test.ts`: obfuscated eval/exec flagged; clean code passes; comments not flagged.
  - **Reducer merge fix** (`reducer.ts:113-135`): the lint engine and SecurityEmitter both emit `verify.security` concurrently; last-wins `get` would drop one. Now sums all `verify.security` events so both literal + obfuscated findings count. Single-event tests unaffected.
  - **Surgical `emitter-factory.ts`** wiring (qwen's ChangesEmitter + stale-ref comment untouched): imports the two emitters, deletes the inline `GraphEmitter` stub, swaps `resolvedSecurity` for `new SecurityEmitter(...)`, passes `writtenFiles` to graph. `verification-emitter-routing.test.ts` updated to assert `SecurityEmitter`/`GraphEmitter` (not the old lint alias).
  - **ADR-007** (`docs/adr/007-...`): "Four fixed slots" → "Five slots (lint, types, security, graph, imports) matching `SLOT_TO_SIGNAL`"; `git` dropped, `graph`+`imports` added; priority list rewritten (Security 1, Lint 2, Types 3, Graph 4 structural, Imports 5 advisory); "default 4" → "default 5"; security described as a dedicated emitter, not a lint alias.
  - **ADR-013** (`docs/adr/013-...`): "separate repository KirkForge-Plugin consumed as path dependencies" → "vendored in-repo since Phase-6 under `crates/plugin3-*`"; standalone CLI `apps/plugin-cli` → `crates/plugin3-cli`; path-deps consequence → vendored single-repo consequence. The prior separate-repo framing marked superseded.
  - **ADR-0011 / ADR-0012** (B10): Deferred → **Rejected** (2026-07-17) with one-line rejection blockquote; `docs/adr/README.md` index updated.
  - **Box::leak** + **detached approval-reader thread**: see "Correctness (low) — DONE 2026-07-17" below for file:line.
  - Gate green: `npx vitest run` 996 passed (≥984); `cargo test` 1196 lib + integration green (incl. `approval_reader_thread_joins_on_shutdown`); `cargo clippy --all-targets -- -D warnings` clean; orchestrator `tsc --noEmit` clean. Outstanding: live `gitnexus`/`graphify` refs in config schemas / plugin capabilities / CLI / `mcp_client` docs / `prompt/mod.rs` remain — that cleanup is the qwen delegate's KirkForge-Cli P0 (stale-ref cleanup), not edited here to avoid concurrent-line collision.

- [x] **Session 1 — Cloud-routed frontier model defaults + native Kimi adapter (2026-07-15).**
  - `Config::default()` now leaves `default_model`, `ollama_host`, and `summarize_model` empty; `default_request_timeout_secs` reduced to 120.
  - Smart routing no longer hard-codes model names; tier names are resolved via `routing_model_map` then `default_model`. The `contains("pro")` substring bug (C26) is removed.
  - Added native `src/adapters/kimi.rs` for Kimi/Moonshot with 256K context, native tool calls, and `reasoning_content` thinking support.
  - Removed the old "potato hardware" / low-resource laptop narrative from ADRs, README, CHANGELOG, `src/cli.rs`, `src/tui/syntax/mod.rs`, `src/tui/commands/route.rs`, `src/session/prompt/summarizer.rs`, and `config.toml.example`.
  - Commit gate green: `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings`, `scripts/impact-fallback.sh`, `scripts/ci-local.sh quick`.

- [x] **Session 2 — Write-side minification / VFS envelope (2026-07-16).**
  - Added `minify_write_side` config flag (`Config` + env `KIRKFORGE_MINIFY_WRITE_SIDE` + TOML `minify_write_side`), default `false`.
  - Added `src/shared/minify/expand.rs` for envelope parsing/wrapping/expansion and language mapping.
  - `read_file` wraps whole-file and partial output in `<minified lang="...">...</minified>` when both read-side and write-side minification are enabled.
  - `write_file` detects the envelope, expands it via external formatters or a language-aware fallback, and writes formatted source to disk.
  - `edit_file` performs a minified round-trip: expands the original file, expands minified `old_string`/`new_string` if they are envelopes, applies the edit, and writes the expanded result.
  - Updated system prompt (`prompts/system.hbs`), ADR 005, CHANGELOG, and `config.toml.example`.
  - Commit gate: run as part of this update.

- [x] **Session 4 — Correctness bugs C11–C27 + performance P4–P9 (2026-07-16).**
  - Correctness: deterministic `event_bus` idempotency set, summarizer divide-by-zero guard, `strip_test_blocks` brace-depth tracking, background bash `workdir` tilde expansion, `read_file` single-minify + raw cache, DSML escaped/single-quoted attributes, ffmpeg `flite` filter escaping, daemon socket-before-PID ordering, word-boundary `git_sanitation`, state-machine `memory` frontmatter parser.
  - Performance: TUI message/event buffers switched to `VecDeque`, per-language keyword cache via `OnceLock`, width-aware markdown rule, char-slice compare in safety helper, `Arc<BusEvent>` in event-bus history, line-by-line NDJSON conversation loading from `BufReader`.
  - Added `edit_file` fuzzy-CRLF whitespace-normalization regression test.
  - Commit gate: `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all`, `scripts/impact-fallback.sh`, `scripts/ci-local.sh quick`.

- [x] Phase 1 — Plugin host primitives (`load_one`, `remove`, `find_active_by_name`) and `SkillRegistry` plugin-aware mutations; committed as `feat(plugins): add per-plugin load/remove primitives`.
- [x] Phase 2 — `/plugins` slash-command family wired into the TUI with list/enable/disable/reload/trust; unit tests green.
- [x] Phase 3 — Filesystem plugin wrappers created for `KirkForge-Plugin2`, `KirkForge-Plugin3`, and `KirkForge-Plugin`.
- [x] Phase 4/5 — README/CHANGELOG updated, TUI wiring test added, `feature/runtime-plugin-toggle` merged through `merge` and `dev` to `main`; `scripts/ci-local.sh quick` green on every branch.
- [x] Phase 6 — All five satellite repos merged into this single codebase (`crates/*` for Rust, `npm/kirkforge-plugin` for Node), workspace builds green, merged through `merge` → `dev` → `main` (2026-07-10).
- [x] Phase 6 follow-up — Node SDK test batches green locally, video integration tests guarded for missing `ffmpeg`/`flite`, CI installs those binaries, coverage gate aligned with current core-module coverage; `main`, `dev`, and `merge` CI all green on GitHub (2026-07-10).
- [x] Review fix-list (P0/P1/P2 from `review.md`) implemented and committed on `refactor/module-splits` (2026-07-14): S1/S2/S3, C1–C10, C15, C18–C22, P1/P2/P3/P10, plus the `env.rs`/`cli.rs` new files. P3 cleanups and the medium-deferred C11–C14/C16/C17/C23–C27 items remain open. Separate `fix(metrics)` commit made the test `PATH_OVERRIDE` thread-local to stop a cross-test `test_rotation_replaces_old_log` flake exposed by the gate run. Gate green: `cargo test --workspace` ×3, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`. Not yet merged to `dev`/`main`.

## Next work — review fix list (from `review.md`, 2026-07-13)

Full codebase review done (six parallel subsystem passes, ~59K LOC / 130 files / 11 crates; npm test count 996 as of Session 5, was mis-stated as 866 here). Pick up here tomorrow. GitNexus/graphify MCP tools are uninstalled; `scripts/impact-fallback.sh` is the only regression smoke-net (do not reintroduce gitnexus/graphify). Commit gate: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `scripts/ci-local.sh quick`.

### P0 (real bugs / security — do these first)
- [x] **S1 — `$IFS` bash safety-gate bypass.** Fixed in `src/session/bash_runner/safety.rs`; added `contains_shell_expansion_evasion` block plus regression tests.
- [x] **C1 — audit `truncate_string` UTF-8 panic.** Fixed in `src/shared/audit.rs` with `char_indices()`-safe truncation and a multibyte regression test.
- [x] **S2 — plugin verifier env leak.** Fixed by adding `env_clear()` + curated allowlist in `crates/kirkforge-plugin-host/src/{env.rs,tool.rs,verifier.rs,hook.rs}` and a `plugin_verifier_does_not_leak_session_env` test.
- [x] **C2 — `minify_old_messages` is a no-op.** Fixed extension detection in `src/session/prompt/mod.rs` (`synthetic_extension_for`) and added a token-reduction test.
- [x] **P1/P2 — TUI idle CPU.** Fixed in `src/tui/{app.rs,events.rs,mod.rs,widgets/chat/mod.rs}` with per-message version counter and gated slow-tick dirty mark.

### P1 (correctness — do after P0)
- [x] **C4/C5 — SSE parser.** Rewrote `parse_openai_compat_stream` in `src/adapters/openai_compat/mod.rs` to use a raw byte buffer and decode only complete frames as UTF-8; invalid JSON frames are now reported and discarded instead of buffered forever. Added multibyte-split and invalid-frame tests.
- [x] **C3 — `undo.push()` eviction-before-write.** Reordered `push` in `src/session/undo.rs` to write the new snapshot before trimming old entries; added a read-only-dir failure test that proves old snapshots survive.
- [x] **C6 — `line_mode` cancellation loses the editor.** Replaced the `Option<Editor>` with an `Arc<Mutex<Option<Editor>>>` in `src/line_mode.rs` so the blocking task always restores the editor even if the outer future is cancelled.
- [x] **P3 — tracing re-opens log file per record.** Open the log file once in `src/main/mod.rs` and share it behind a mutex; the per-record `OpenOptions::open` is gone.
- [x] **S3 — `read_image` has no self-guard.** Added `PathGuard` to `src/tools/read_image.rs`; updated `src/tools/mod.rs` wiring and tests.

### P2 (worth fixing, less urgent)
- [x] **S5/S7 — security verifier.** `src/session/verifier/security.rs:233` now returns `Skipped` for files >1 MB; `:269-284` shell-danger scan no longer requires `.sh/.bash/.zsh` extension. Added regression tests.
- [x] **S4 — `check_traversal` deny-list gap.** `src/session/access/mod.rs:110-164` now re-checks the canonical path against the deny list (moved the existing re-check from `check_read` into `check_traversal`); `executor/helpers/mod.rs:241` does the same for resolved search paths. Added regression tests.
- [x] **C9/C10 — durability + OOM.** `src/session/carryover.rs:245-270` now writes `carryover.json` atomically via a same-directory temp file + rename; `src/session/session_index.rs:313-320` now counts non-empty lines by streaming with `BufReader` instead of `read_to_string`. Added regression tests.
- [x] **C22 — status-bar spacer.** `src/tui/widgets/status.rs:111-147` now includes the `[Ctrl+T: tool collapse ON]` span in `right_visible_len`; added an 80-column regression test.
- [x] **C15/P10 — grep `total` undercount + glob unbounded.** `src/tools/grep.rs:87-89,124` now continues scanning after `max_matches` results are collected so `total` reflects every match; `src/tools/glob.rs:73-101` now accepts `max_matches` (default 1000) and reports the true total while truncating output. Added regression tests.
- [x] **C18/C19 — exit codes + man page drift.** `src/main/mod.rs:110-130` now uses a typed `KirkForgeError` enum for exit-code classification, including the missing "outside the allowed area" / "not permitted" cases; `build.rs:31-86` no longer mirrors `Cli` — it includes the real `src/cli.rs` so the man page stays in sync with `Metrics`, `Sessions::search`, and the `OutputFormat` enum.
- [x] **C7/C8 — stream/log loss.** `src/session/conversation.rs:417-430` now skips corrupt NDJSON lines while preserving later valid lines; `src/adapters/ollama_ndjson.rs:204-255` now flushes buffered tool calls when the stream ends, even if `done: true` is absent. Added regression tests.
- [x] **C20/C21 — turn-event JSON.** `src/main/turn_events.rs:154` now accumulates `turn_cost` into the running `cumulative_cost` instead of blindly overwriting it with the event field, so a cached/zero-cost turn cannot wipe the session cost; `:105-122` now synthesises a `ToolCallRecord` for `ToolResult` events with no matching `ToolStart` instead of dropping them. Added regression tests.
- [x] **S6 — git verifier misses prefixed/chained git commands.** `src/session/verifier/git.rs` gate replaced with `command_invokes_git` (split on `&&`/`||`/`;`/`|`/newlines, skip leading env assignments + `sudo`/`env`/… prefixes); `is_git_modifying_command` made chain-aware via the same extractor. `cd /repo && git merge`, `sudo git …`, `GIT_DIR=… git …` now detected. Added unit tests. Ceiling: `$(git …)`, git in quoted strings, `sudo -E git` are not parsed (post-condition verifier, not a shell parser).
- [x] **S8/S9/S10 — medium-security follow-ups (2026-07-15).** `is_gitignored` is now async via `tokio::process::Command` so `check_write` no longer blocks the async runtime. The daemon Unix-socket line reader is capped at 1 MiB (`read_line_limited`). Plugin signature verification now resolves `minisign` from `PATH` before spawning and canonicalizes the manifest path before signing; regression tests cover path resolution, canonicalization, and the missing-binary error.

All P0/P1/P2 review items above are landed on `refactor/module-splits` (commits `ebe5040`, `cf7b5ce`, `c3ddae7`, 2026-07-14). **Not yet merged to `dev`/`main`.** Remaining gaps consolidated below — pick up here tomorrow.

## Open work for tomorrow

Run `scripts/impact-fallback.sh` before editing each symbol (GitNexus is out of scope this session). Commit gate: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `scripts/ci-local.sh quick`.

### First: merge today's work
- [x] Merge `refactor/module-splits` → `dev` → `main` (through `merge`) and push; today's three commits are now on `dev`/`main`/`merge` (2026-07-15).

### Security (medium)
- [x] **S8** — `is_gitignored` spawns a thread + blocks up to 2s per call from the async `check_write` path. `src/session/access/mod.rs:74-100`.
- [x] **S9** — `read_line` with no length cap on both ends of the daemon unix socket (corrupted stream → unbounded memory). `src/daemon/client.rs:51`, `src/daemon/server.rs:244`.
- [x] **S10** — minisign verification runs `minisign` from PATH with no existence pre-check; manifest path not canonicalized before signing. `crates/kirkforge-plugin-host/src/lib.rs:396-432`.

### Correctness (medium-deferred — defer unless hit)
- [x] **C11** — `event_bus` idempotency trim is effectively random (HashSet has no order). `src/session/event_bus.rs:451-456`. Fixed by keeping a deterministic ordered set and trimming from the front.
- [x] **C12** — `summarizer` divides by zero when `tokens_before == 0`. `src/session/prompt/summarizer.rs:231`. Guarded and reports fallback.
- [x] **C13** — `strip_test_blocks` leaks the test mod's closing `}` into minified output. `src/shared/minify/lang.rs:99-116`. Tracks brace depth and swallows the matching `}`.
- [x] **C14** — background bash workdir not tilde-expanded (foreground is). `src/session/bash_jobs.rs` now expands `~` the same way foreground bash does.
- [x] **C16** — `read_file` double-minifies + cache-poisons on whole-file read. `src/tools/read_file.rs:102-119`. Caches raw content; minify happens at prompt layer.
- [x] **C17** — `parse_name_attr` doesn't handle escaped quotes in DSML. `src/adapters/tool_call_markup.rs:137-147`. Now handles `\"` and single-quoted attributes.
- [x] **C23** — ffmpeg `flite` filter string under-escaped (`:`, `\`, `]`, `,` are metachars). `crates/kirkforge-video/src/pipelines/animated_explainer.rs:827-833`. Escaped via `ffmpeg_escape`.
- [x] **C24** — PID file written before `UnixListener::bind` (stale PID on bind fail). `src/daemon/server.rs:50-56`. Socket bind now happens before PID write.
- [x] **C25** — `git_sanitation` false positives: `.env` substring matches `source .env.local`; `=======` matches `========`. `src/session/git_sanitation.rs:35, 220-225`. Word-boundary/line-anchored matching.
- [x] **C26** — `resolve_model` substring routing: `contains("pro")` matches "professor"/"prophet". `src/session/adapter_swap.rs:144-146`. Fixed by removing the heuristic entirely; tier names are now resolved via `routing_model_map` then `default_model`.
- [x] **C27** — `memory::parse_frontmatter` naive `find("---")`/`split_once(':')` truncates URLs. `src/session/memory/mod.rs:283-286, 339`. Small state-machine parser that only treats `---` at line start as a delimiter.

### Correctness (low — opportunistic)
- `daemon/mod.rs:130` unreachable `unwrap_or_else` (idx from `position` + `remove` always `Some`).
- `adapters/mod.rs:38-40` redundant `status == 503` (subsumed by `500..600`); `:66` unused `_client`.
- `openai_compat/mod.rs:388-390` redundant Claude `starts_with` branches; `:473` "server-side" → "client-side".
- `ollama_ndjson.rs:321` orphaned doc-comment fragment.
- `line_mode.rs:113-114` rewrites whole history file per input line (bounded by 100-entry cap).
- `mcp_tools.rs` `Box::leak` leaks tool-name strings per MCP reload (~100 B/tool).
- `mcp_client/mod.rs:674-676` detached `Drop` reaper task may not complete before runtime shutdown → zombie child.
- `prompt/mod.rs:13,30` `PromptBuilder.cache: HashMap` is dead (declared, never used).
- `prompt/mod.rs:357-368` `estimate_message_tokens` ignores `content_parts` (base64 images) → under-estimates.
- `bash_minify.rs:374-444` `extract_file_path` uses `split_whitespace` (spaces in paths skip minify — safe, no corruption).
- `read_file.rs:67-76` double allocation (full `String` + `Vec<&str>`), bounded by 1 MiB cap.
- `git_sanitation.rs:268-272` rename split on ` -> ` mis-parses filenames containing ` -> `.
- `main/mod.rs:1045-1099` detached approval-reader thread never joined; `/help`/carryover slash commands print to stdout in non-interactive pipe mode.

### Correctness (low — opportunistic) — DONE 2026-07-17 (Task 8)
- [x] **Box::leak unbounded on `/reload plugins`** — `Box::leak` per `McpToolWrapper`/`PluginToolWrapper` construction accumulated on every reload (`executor/mod.rs:434` rebuilds all plugin wrappers). Replaced with a deduplicating interner `shared::intern_static_str` (`src/shared/mod.rs:697-725`) wired into `src/session/mcp_tools.rs:45-46` and `src/session/plugin_tools.rs:93-94`. Leaks at most once per unique tool name; repeated reloads reuse. (Full `Arc<str>`-in-`ToolDef` fix is the upgrade path — ~90-site change since `Arc<str> == &str` is not in std; not justified by the reload-growth defect this interner already bounds.)
- [x] **Detached approval-reader thread never joined** (`main/mod.rs:1107`) — Unix path now polls `/dev/tty` via `libc::poll` with a 200 ms interval + `shutdown` flag (`read_approval_answer_pollable`/`poll_read_line`, `main/mod.rs:945-1030`); the `JoinHandle` is joined on both answer and timeout paths (`main/mod.rs:1106-1130`). Gate test `approval_reader_thread_joins_on_shutdown` (`main/mod.rs:1351`) confirms the thread joins within ~one poll interval on shutdown. Windows path stays blocking (stdin not interruptible) and remains detached.

### Performance (still open)
- [x] **P4** — `events.rs` `drain(0..n)` O(n) memmove → `VecDeque`. `src/tui/events.rs:313` and related TUI modules. (== P3.2 first half)
- [x] **P5** — `syntax` keyword `HashSet` rebuilt per code block → `OnceLock` per language. `src/tui/syntax/language.rs` and `src/tui/syntax/mod.rs`. (== P3.2 second half)
- [x] **P6** — `rendering.rs` horizontal rule hardcoded 40 chars regardless of terminal width. `src/tui/rendering/mod.rs:463`. Now width-aware.
- [x] **P7** — `events.rs` allocates `"assistant".to_string()` per token event → `&str` compare. `src/session/bash_runner/safety.rs` `word_boundary_match` char-slice compare (the described site was consolidated into the safety helper).
- [x] **P8** — `caching.rs` clones every `StreamEvent` (`ev.clone()` then send). `src/session/event_bus.rs` stores `Arc<BusEvent>` and hands out cheap clones.
- [x] **P9** — `ollama_ndjson.rs` allocates a `Vec<u8>` per NDJSON line → slice in-place. `src/session/conversation.rs` `load_messages` now parses line-by-line from a `BufReader`; the original ollama_ndjson buffer site is covered by the same streaming pattern.
- **P11** — `deepseek.rs`/`gemini.rs`/`glm.rs` have byte-identical `stream()` bodies (~60 lines × 3); documented duplication, not asking for an abstraction.

### Cleanup / maintainability (P3)
- [ ] **P3.1** — `src/tui/keys/mod.rs:71-990` `handle_input_key` ~900-line match → table-driven command dispatch.
- [ ] **P3.2** — `src/tui/events.rs:313` `drain(0..n)` → `VecDeque`; `src/tui/syntax/mod.rs:195-203` keyword `HashSet` → `OnceLock` per language. (== P4 + P5)
- [ ] **P3.3** — Consolidate `[workspace.dependencies]` for `serde`/`tokio`/`clap`/`tracing` (redeclared across 9 non-root crates with drift risk).
- [ ] **P3.4** — Remove dead `PromptBuilder.cache` (`src/session/prompt/mod.rs:13,30`); mark/remove unused host-crate tool/hook executors or fix their env handling.

### Test gap (only one left from the review)
- [ ] `edit_file` fuzzy-fallback incl. the CRLF byte-offset fix has zero coverage — the "fuzzy" tests use an `old_string` that is an exact substring so exact-match fires first. `src/tools/edit_file.rs:168-288`, tests 396-473.

### Plugin (v2, deferred)
- [ ] **Phase 5.6** — persist enable/disable state in `~/.local/share/kirkforge/plugin-state.toml`.

## Notes

- GitNexus and graphify tools were uninstalled (2026-07-17): the MCP server, skills, hooks, and binaries are gone. Any remaining `scripts/impact-fallback.sh` is now the only regression smoke-net; do not reintroduce gitnexus/graphify.
- Each satellite's KirkForge runtime plugin wrapper is under `plugins/<name>/` and registered as a workspace plugin source by default.
- The satellite source code now lives in this repo; the plugin tool scripts prefer workspace-built binaries (`target/release/` / `target/debug/`) and fall back to `PATH` only when needed.
- `KirkForge-Plugin` (the SDK) also contains the plugin-host crate used by the CLI; changes there affect both packaging and runtime.
- Default branch `main` now contains the unified single codebase.

## Carryover from KirkForge-Plugin3 (retired to archive 2026-07-17)

Source `KirkForge-Plugin3/state.md` (gap audit B1–B15) was reviewed on fold-in. B1–B9, B11–B15 are fixed in code now at `crates/plugin3-*`. One item remains open:

- [x] **B10 — ADR-0011 / ADR-0012 resolved 2026-07-17.** Both were Deferred since 2026-06-24 with zero stub code in `crates/plugin3-*`. Decision: **Rejected** (not implemented, not live architecture). `docs/adr/0011-persistent-knowledge.md:3,6` and `docs/adr/0012-speculative-priming.md:3,6` now carry `Status: Rejected (2026-07-17)` with a one-line rejection blockquote; `docs/adr/README.md:27-28` index updated.

Cross-project pattern noted there and still relevant to this repo: release automation, CI gating, and ops docs (CHANGELOG/deployment/runbook) tend to lag the code.
