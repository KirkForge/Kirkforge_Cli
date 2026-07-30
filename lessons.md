# lessons.md — Series 11 (Plugin System Hardening)

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

## WO 14.4 (status bar graceful degradation)

### What I learned
1. **`⚠️` emoji width is 2 display cells but `.content.chars().count()`
   counts it as 1** (the codepoint `U+26A0` + `U+FE0F` variation selector
   = 2 chars, but the variation selector is width-0, so `chars().count()`
   returns 2 — wait, actually `⚠️` is `U+26A0` (1 char) with a variation
   selector appended making it 2 chars, but the rendered width is 2
   cells). The sandbox span `"⚠️ UNSANDBOXED "` is 14 chars by
   `chars().count()` but 15 display cells. The existing pre-WO-14.4 code
   used `chars().count()` for ALL spans, so the "fits" check was off by
   1 whenever the UNSANDBOXED span was visible. WO 14.4 fixes this with
   a manual `+1` when the span contains `⚠` (cheaper than adding
   `unicode-width` as a direct dep, which is not in the root Cargo.toml
   — only transitive via ratatui). The workorder flagged this as a real
   but separate bug; I fixed it inline because it made the drop-loop
   math wrong by one cell.

2. **`unicode-width` is NOT a direct dependency in the root `Cargo.toml`.**
   It's transitive via `ratatui` (and several crates use it directly:
   `kirkforge-draw-core`, `kirkforge-draw`). Adding it as a direct dep
   to the main binary would show up in the size-optimized release
   binary. The manual `+1` for the one known wide emoji is the cheaper
   fix; if more wide glyphs land in the status bar later, promote
   `unicode-width` to a direct dep then.

3. **`collapse_span` (Ctrl+T indicator) was on the LEFT side of the
   spacer in the original layout.** WO 14.4's priority list names it as
   a droppable right-side span, so I moved it to the right side of the
   spacer (into the drop-loop's span set). This is a minor visual shift
   at wide widths (the Ctrl+T hint now sits after the spacer, flush
   against the right cluster, instead of right after the model name).
   The workorder's priority list is the contract; the done-condition
   names it as droppable, so moving it is correct.

4. **`session::hooks::tests::test_run_decision_merges_builtin_and_plugin_hooks_deterministically`
   is flaky under heavy system load.** It spawns two external bash hook
   processes and reads a marker file; under load avg 25+ (a competing
   worktree build), the plugin hook's write hadn't flushed before the
   assertion. It passes in isolation. This is NOT a WO 14.4 regression
   (my change only touches `src/tui/widgets/status.rs`). The hooks.rs
   file already documents race concerns at lines 873/923. When running
   the full workspace gate on a loaded machine, expect this test to
   flake; re-run in isolation to confirm green.

5. **Build times on this repo are ~5-7 min per profile** (the
   `auto_generate_cdp` / `headless_chrome` crate is the slow one).
   Competing worktree builds (`wo-14.7`) pushed load avg to 25 and
   made the gate take ~25 min total. Budget for this; a clean machine
   does the full gate in ~10 min.

### What I tried that didn't work
- **`let mut space = 0usize;` initial assignment.** Clippy flagged it
  as `unused_assignments` (the `0` is never read; every branch
  reassigns). Restructured to compute `space` as a single `let` after
  the drop loop runs — the drop loop is now always evaluated (no-op if
  it already fits), and `space` derives from the post-drop `floor`.
  Cleaner and warning-free.

### What I'd do differently
- Nothing material. The drop loop is 8 iterations max (4 droppable
  spans), allocates one `Vec` of 10 spans per frame. That's fine for a
  status bar (1 row, rendered on state change). A bitmask over fixed
  spans (the workorder's suggestion) would avoid the `Vec` but adds
  complexity for no measurable gain at 1 row/frame.