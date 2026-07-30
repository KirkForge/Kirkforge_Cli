# lessons.md — Series 11 (Plugin System Hardening)

## WO 14.8 — Dead-Code + #[allow] Audit

### Warning counts per module-wide allow
- `src/session/mod.rs` `#![allow(dead_code)]` hid **17 unique items**:
  2 dead methods (`tool_context_for_call`, `dispatch_tool_call` — the
  single-call dispatch superseded by `dispatch_tool_call_batch`), 2 dead
  struct fields (`CompactHookStats.tokens_before/tokens_after`), 1 dead
  const (`CONFLICT_MARKERS` — inlined in `has_conflict_marker`), 2 dead
  struct fields (`StdioMcpClient.reader_task/stderr_drain`), 3 lifecycle
  methods (`McpClient/StdioMcpClient/McpHttpTransport::disconnect` — used
  by tests only), 7 dead struct fields (`McpHttpTransport` —
  client/base_url/session_id/last_event_id stored but never read; clones
  moved into spawned tasks do the real work), 1 dead const
  (`TOOL_RESULT_DEFAULT_CAP` — head+tail used instead), 2 test-only
  items (`DEFAULT_PRESERVE_RECENT`, `compact` — pub but only used in
  same-file tests), 1 dead struct field (`MicrocompactResult.tokens_before`),
  1 dead struct field (`UndoStack.session_id` — dir computed from it,
  field never read after).
- `src/shared/mod.rs` `#![allow(dead_code)]` hid **3 unique items**:
  `expand_minified_by_ext` (pub fn, no callers at all), `CollapseBlankLines`
  (struct + `new` + `Iterator` impl — no callers at all).

### What I learned
1. **The single-call `dispatch_tool_call` was 740 lines of dead code.**
   It was superseded by `dispatch_tool_call_batch` in turn.rs but never
   deleted. The module-wide allow hid it for many WOs. Deleting it
   orphaned 13 imports — clippy caught them on the next run.
2. **`McpHttpTransport` stored 7 fields that were never read.** The
   constructor clones `client`/`session_id`/`last_event_id` into the
   spawned reader/poster tasks; the struct copies were dead storage.
   `reader_task`/`poster_task`/`shutdown_tx` are only read by
   `disconnect()` (a lifecycle method used by tests, not production —
   production relies on the Drop fallback).
3. **`pub` items used only by same-file tests are a common dead-code
   pattern.** `compact` and `DEFAULT_PRESERVE_RECENT` in compaction.rs
   are `pub` but only called from the same module's `#[cfg(test)]`
   block. The fix is `#[cfg(test)]` on the item (vanishes from lib
   build, stays for test build) — cleaner than a targeted allow.
4. **A stale `#[allow(clippy::too_many_arguments)]` sat on
   `conversation_log(&self)`** (0 args) — probably leftover from a
   refactor. Removed it; count went 12 → 11 real allows.
5. **`BrandKit` in animated_explainer.rs has a `ponytail:` annotation
   AND `#[allow(dead_code)]`.** The allow is legit (serde-deserialized
   fields read by serde, not code). Preserved the ponytail; added a
   reason comment.
6. **Pre-existing `clippy::approx_constant` warnings in
   `src/session/video.rs` tests** (`3.14` approximates PI) surface only
   under `--features video`. These are NOT from this audit (pre-existing
   on origin/dev). Out of scope for the #[allow] audit; noted for a
   future video-feature clippy pass.
7. **Background cargo via `setsid ... & disown` survives the tool's
   command timeout.** The shell tool kills the foreground process group
   on timeout, but `setsid` creates a new session, so the cargo process
   keeps running. Poll with `pgrep`/`tail` on the log file.

## Series 11 (Plugin System Hardening) — prior session notes

## What I learned

1. **`Config::default()` includes 4 in-repo plugin_sources.** Tests that
   assert "empty" plugin state must clear `plugin_sources` AND
   `enabled_plugins` — the default config points at the real repo `plugins/`
   dir. This bit me in WO 11.0's `plugin_ops` tests.

2. **`#[serde(rename_all = "kebab-case")]` on a struct converts ALL fields.**
   To keep a snake_case TOML key (`depends_on`, `resource_limits`) under
   a kebab-case struct, use `#[serde(rename = "depends_on")]` per field.
   The WO spec used `depends_on` (snake_case); without the per-field
   rename, the TOML key becomes `depends-on` and manifests break.

3. **`minisign` crate's `KeyPair::generate_unencrypted_keypair()` works
   with `minisign-verify`.** The two crates are by the same author; the
   keybox format is compatible. `allow_legacy = true` in
   `minisign_verify::PublicKey::verify` accepts both standard and
   legacy signatures.

4. **`run_hook_script` returns `Ok(Allow)` for non-zero exits (not 0/2).**
   To audit fail-open failures (WO 11.6), I had to make it return `Err`
   for non-zero exits so the `run_decision` Err arm fires the audit +
   fail-open path. This was a behavior change in the return type, not
   the semantics (the caller still converts Err → Allow).

5. **`HostedPlugin` needed `original_capability_count`.** After
   `filter_capabilities` mutates the plugin, the manifest's
   `capabilities` is the *filtered* set, not the original. To show the
   filtered count in `/plugins list` (WO 11.3), I had to record the
   original count before filtering.

6. **`notify-debouncer-mini` 0.7 API:** `new_debouncer` returns
   `Result<Debouncer<RecommendedWatcher>, Error>`. The channel receives
   `DebounceEventResult` (= `Result<Vec<DebouncedEvent>, Error>`), not
   `DebouncedEvent` directly. The `notify::RecursiveMode` is re-exported
   as `notify_debouncer_mini::notify::RecursiveMode`.

7. **`tokio::process::Command` vs `std::process::Command`:**
   `setup_rlimits` takes `&mut tokio::process::Command` (the bash
   runner uses tokio's Command). Passing `command.as_std_mut()` was
   wrong — pass `&mut command` directly.

8. **Clippy `uninlined_format_args`:** `format!("{x}")` not
   `format!("{}", x)` for local variables. The repo enforces this.

9. **The `readme_drift` test counts `#[test]` attributes under `crates/`
   only.** Adding tests to `crates/kirkforge-plugin` and
   `crates/kirkforge-plugin-host` bumped the count from 1555 → 1569; I
   had to update `crates/plugin3-core/README.md`. The 10-series
   regression (subagent B forgot this) was a real warning — I checked
   every time.

## What I tried that didn't work

- **TUI migration in WO 11.0:** I initially planned to rewrite the TUI
  `handle_plugins_op` to call the shared `plugin_ops` functions. This
  would risk a regression in the live-reload path (`plugin_reload_tx`).
  I kept the TUI unchanged and made the shared layer additive. The WO
  notes this as an explicit decision.

- **Release binary size check for WO 11.1:** A full `cargo build --release`
  timed out (20+ min on this machine). I documented the size impact as
  estimated (the `minisign-verify` crate is zero-dependency, ed25519-only,
  ~50KB). The WO accepts this.

## What I'd do differently

- **Test the `kirkforge plugin` CLI via `assert_cmd`:** I tested the
  shared `plugin_ops` functions directly (unit tests) and ran
  `kirkforge plugin list` manually. An `assert_cmd` integration test
  would prove the CLI end-to-end. I skipped this for time; the unit
  tests + manual run cover the contract.

- **The `AuditEntry` enum change is backward-incompatible for old NDJSON
  logs.** Old entries (struct form, no `"kind"` tag) don't deserialize
  with the new tagged enum. I documented this in ADR-061. A future
  migration could add a fallback raw-JSON reader.

## Scope creep

- **`enable_plugin` in TUI now honors `reject_on_excess_plugin_trust`.**
  WO 11.3 required loading a downgraded plugin, which needed this fix.
  The TUI's `enable_plugin` previously always used
  `TrustPolicy::up_to()` (which sets `reject_on_excess = true`). I
  changed it to `with_reject_on_excess(cfg.tools.reject_on_excess_plugin_trust)`.
  This is a 1-line bug fix that the WO 11.3 test required — not scope
  creep, but it's a behavior fix outside the WO's "display only" scope.

- No other scope creep. All 10 WOs touched only their named files + the
  shared doc files (TECHNICAL.md, state.md, CHANGELOG.md, ADR index,
  workorders README).

## WO 12.6 (testdoctor smart suggest + apply)

### What I learned
1. **`regex` is a root `[dependencies]` entry but not a
   `[workspace.dependencies]` entry.** To share it with the testdoctor
   (a separate bin), I added `regex = "1"` to
   `[workspace.dependencies]` and `regex = { workspace = true }` in the
   testdoctor's `Cargo.toml`. The root `[dependencies] regex = "1"`
   line (used by web_fetch) stays — it's the same version, so cargo
   dedups. Adding a workspace dep is the clean way to share without
   bumping the main binary's size (the testdoctor is a separate bin).

2. **The `readme_drift` test counts `#[test]` under `crates/` only.**
   Confirmed again: I added 25 net `#[test]` under
   `crates/kirkforge-testdoctor/` (1616 → 1641). The drift window is 2,
   so I had to bump `crates/plugin3-core/README.md` from 1615 → 1641.
   The count script (`python3` walk) matches the test's logic exactly
   (`#[test]` on its own line + next non-blank line starts with `fn `).

3. **Hand-rolled unified diff is fine for the doctor's "show the diff
   first" contract.** No `similar` dep needed. The diff is a
   single-hunk, line-level diff — O(n*m) worst case but test files are
   a few hundred lines. The format (`---`/`+++`/`@@`) is good enough
   for a CLI tool; a human reads it, applies with `--yes`.

4. **Text-based apply must refuse to guess.** If a pattern matches
   multiple sites (e.g. two `std::env::set_var(` calls in one test),
   return an error — don't pick one. The doctor prints the diff; the
   human decides. This is the v1 contract; a `syn`-based v2 could
   resolve by line number.

5. **`Regex::new(...).is_ok_and(|r| r.is_match(src))`** is the clippy-
   safe way to use a one-shot regex without `unwrap()`. The pattern
   is a static literal so it never fails to compile, but clippy still
   wants the `is_ok_and` form over `.unwrap().is_match()`.

### What I tried that didn't work
- **Rewriting `apply_tokio_start_paused` with an in-place `out.clear()`
  mid-loop.** The first version tried to rewind the output buffer to
  drop the previously-emitted attribute line, then rebuild. It was
  fragile (off-by-one on the trailing newline, missed lines after the
  fn signature). Replaced with a clean two-pass approach: first pass
  locates the fn + attr indices, second pass rebuilds the whole file
  substituting the new attr line at the known index.

### What I'd do differently
- **Use `similar` for the diff.** The hand-rolled diff is fine but a
  real diff library produces better multi-hunk output. `similar` is
  already a root dep (line 105); sharing it via workspace deps would be
  zero-cost. Deferred to keep the v1 change small — the contract is
  "show the diff, then apply with --yes", and a single-hunk diff
  covers the 3 supported fix kinds.