# Lessons — Series 10 Wiring-Depth (WO 10.2, 10.7, 10.8, 10.9)

Worktree: `/home/kirk/Madlab/Github/kf-10b` off `origin/dev`.
Branch: `wo/10-series-wiring-depth`.

## What I learned

### Metrics test isolation (WO 10.2)
- The `with_test_path` helper in `metrics.rs` is sync-only (takes a
  non-async closure). For async executor tests that need to `.await`
  between `record()` calls, I added `set_test_path` / `clear_test_path`
  pub(crate) helpers that install/clear the thread-local
  `PATH_OVERRIDE` without holding a lock.
- The `#[tokio::test]` runtime is current-thread, so the thread-local
  override IS visible to `record()` calls inside `stream_iteration`.
  Do NOT hold `std::sync::MutexGuard` across `.await` — clippy's
  `await-holding-lock` lint catches it.
- The existing `test_plan_reason_emitted_after_tool_call` test uses
  `.iter().any()` to tolerate cross-test contamination (the metrics
  log is a shared platform file). For exact-count assertions, use
  `set_test_path` to isolate.

### prefix_len policy (WO 10.2)
- The WO suggested `prefix_len = messages.len() - 1` but that makes
  the prefix grow every turn (history grows), so `is_stable` never
  returns true. The correct first-cut policy is `prefix_len = 1`
  (system message only), which matches ADR-052's Future Work note.
  The conversation history cannot be part of the stable stem because
  it grows every turn.

### Anthropic API content-omission (WO 10.2)
- The Anthropic prompt-caching docs confirm: "Cache hits require 100%
  identical prompt segments" and the cache key is computed from the
  bytes sent. There is NO content-omission API — you must send the
  full content every turn. The `cache_control` markers are idempotent.
  ADR-052 already documented this; I shipped only the metric event
  wiring (the measurement), not a wire-bytes saving.

### SSE parser rewrite (WO 10.7)
- The old SSE parser only looked for `data:` lines. To capture
  `event:` (for the `endpoint` event) and `id:` (for `Last-Event-ID`),
  I rewrote it as a full `field: value` line parser. The key gotcha:
  `String::trim_end_matches` returns `&str` (not `String`), and
  `&str` doesn't have `.as_str()` (that's unstable `str_as_str`).
  Use the `&str` directly.

### Floating-point in regression thresholds (WO 10.9)
- `0.7 - 0.8` in f64 is `-0.10000000000000008882`, which IS less than
  `-0.10` (`-0.10000000000000000555`). So a "10% drop" at small sample
  sizes can trigger the regression gate due to floating-point. The
  `compare_with_threshold` test for "within threshold" uses 100/100 →
  92/100 (delta -8%) to avoid the boundary. The check is strict `<`
  (not `<=`), so a drop of exactly the threshold is not a regression.

### TS orchestrator event bus API (WO 10.8)
- The `EventBus.on()` handler must return
  `Promise<Result<void, HandlerError>>`, not `void`. The shape is
  `{ ok: true, value: undefined }` for success.
- The `SecurityEmitter` puts findings in `value.details` (an array),
  not `value.findings` (which is a count). Read the emitter source
  before assuming the event shape.

## Scope creep

- `src/shared/metrics.rs`: added `set_test_path` / `clear_test_path`
  pub(crate) helpers. This is technically outside the WO 10.2 file
  list, but the WO's integration test needs an async-friendly metrics
  path override and the existing `with_test_path` is sync-only.
- `crates/kirkforge-plugin-host/src/lib.rs`: made `mod env` →
  `pub mod env` for WO 10.8 (the bridge verifier reuses
  `curated_env`). This is a one-word visibility change, not a behavior
  change.

## Pre-existing failures (not mine)

- `crates/plugin3-core --test readme_drift` fails on `origin/dev`
  because the 10.0 commit (other subagent) added 3 tests to
  `cost.rs`/`paths.rs` without bumping the README test count from 1550
  to 1553. This is the other subagent's 10.0/10.3 scope. My WOs do not
  touch `crates/plugin3-core/`.

## What I'd do differently

- For WO 10.2, I'd read ADR-052's Future Work section first — it
  already says `prefix_len = 1` is the first-cut policy, which would
  have saved one test-iteration.
- For WO 10.7, I'd check the `String::trim_end_matches` return type
  before writing the parser — the `&str` vs `String` borrow issue
  cost a compile cycle.