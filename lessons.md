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