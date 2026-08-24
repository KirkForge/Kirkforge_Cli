# ADR-051: Stratum and Budget Guard Coordination

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-07-25

## Context

Stratum (input-side compression) and the Plugin3 budget guard
(output-side slicing) are folded into the core binary behind the
`stratum` and `budget` feature flags (ADR-046, ADR-047). Each ships
in-process Rust tools and hooks (WO 6.6, WO 6.7). The slicing action
shipped in WO 7.1. The two systems are independent at the code level:

- When the budget guard slices a tool result, the head+tail+marker
  display enters the conversation raw. Stratum never runs over the
  sliced display, so the model sees an unsliced-and-uncompressed
  blob in the slot the slicing path just freed up.
- When Stratum compresses a tool output, the budget guard has no
  view of the compressed size — `budget.used` reflects whatever the
  post-tool hook recorded from the raw result.
- When the budget is `Approaching` and Stratum is in `Lite` mode,
  both subsystems are reducing pressure but neither can leverage
  the other. Stratum stays in `Lite` (less compression) when full
  compression would help.

WO 8.6 closes this gap.

## Decision

Add a small, sync coordination layer between the budget guard
(`src/session/budget.rs`) and Stratum (`src/session/stratum.rs`).
The mechanism is a **registered-listener dispatch** on a
process-global `OnceLock<Mutex<Vec<...>>>` — not the async `EventBus`
in `src/session/event_bus.rs`. Reasons:

1. The slice path is itself sync (`apply_budget_slice` returns
   `ToolOutcome` synchronously, called from `record_tool_result`).
   An async roundtrip would force a `tokio::task::block_in_place`
   and panic in single-threaded test runtimes (per AGENTS.md §7).
2. The existing in-process hook model (`InProcessHook`,
   `HookRunner::add_in_process_hook`) is sync and registered at
   executor build time. A listener registry on the budget module
   matches the same shape.
3. The event payload is small (four fields, no serialisation
   roundtrip), so the cost of a custom dispatch surface is
   negligible.

The listener type is `Arc<dyn Fn(BudgetSlicedEvent) -> Option<String>
+ Send + Sync>`. The first listener that returns `Some(replacement)`
wins; the rest are skipped. The budget calls all registered
listeners in registration order on every successful slice.

`BudgetSlicedEvent` lives in `src/session/budget.rs` (carries
`{original_size, sliced_size, key, sliced_display}`) and is
imported by `src/session/stratum.rs`. This avoids a circular
dependency and keeps the event shape co-located with its
producer.

The Stratum side exposes:

- `current_session_mode()` / `set_session_mode(Mode)` — a
  process-global `OnceLock<Mutex<Mode>>` separate from the
  config-derived `active_mode()`. The session mode is what the
  listener consults at slice time.
- `compress_for_budget(content, mode) -> String` — a small helper
  that runs the Stratum pipeline (empty by default; identity for
  plain text) and returns the compressed string.
- `default_budget_sliced_listener()` — the default listener
  implementation.
- `register_default_budget_listener()` — calls into
  `budget::register_sliced_listener`. Wired up at executor build
  time in `executor/mod.rs` under `#[cfg(all(feature = "budget",
  feature = "stratum"))]`.

`apply_budget_slice` dispatches to listeners in both the `Success`
and `FileContent` arms; the post-tool hook (`record_tool_usage`)
then records `result.len() / 4` of whatever content the returned
`ToolOutcome` carries, so the budget's `used` counter reflects the
post-compression size automatically — no extra `used` bookkeeping
is needed in the slice path itself.

### Auto-escalation

`apply_budget_slice` checks `state == Approaching` before slicing
and, when the `stratum` feature is enabled, calls
`stratum::set_session_mode(Mode::Full)` if the current session
mode is `Lite`. The escalation is one-way (`Lite → Full`) and
idempotent (Full/Ultra/Off are no-ops). The `PreCompactHook` in
`budget.rs` (in-process handler for the `pre-compact` event) also
calls the escalation path when the budget is `Over` or
`Approaching` — the post-compaction tool output will then be
compressed more aggressively.

### Failure modes

- If no listener is registered, `apply_budget_slice` falls back to
  the pre-coordination behaviour (sliced display enters the
  conversation raw). The pre-WO tests still pass.
- If the listener panics, the budget mutex is held but the panic
  propagates. Listeners must be `Send + Sync` and not panic;
  tests use plain closures.
- If the `stratum` feature is off but `budget` is on, the
  escalation path is a no-op (gated by `#[cfg(feature =
  "stratum")]`).
- If both features are off, no coordination is wired up and the
  helper is a pass-through (matches the pre-WO default).

## Consequences

- `budget.used` reflects the post-compression size: when Stratum
  compresses the sliced display, the post-tool hook records the
  compressed length and `used` advances by the post-compression
  tokens. Pre-WO, `used` reflected the raw sliced length.
- Stratum `Lite` mode auto-escalates to `Full` when the budget
  is `Approaching`, so the model sees more aggressive compression
  precisely when it matters.
- `PreCompactHook` (in `budget.rs`, not `hooks.rs` as the WO
  workorder's literal wording suggested — the actual hook lives
  in `budget.rs` per WO 6.7) escalates Stratum when budget
  pressure triggers compaction. This means a session that compacts
  with budget pressure will use Full Stratum for the next
  tool-result cycle.
- A small change. No new dependencies. The Stratum pipeline
  stays a no-op identity (the empty pipeline returns input
  unchanged), so the compression listener currently produces no
  measurable savings — that is fine for the WO's coordination
  contract. Future work can register content transforms in the
  pipeline.
- The listener API is extensible: future plugins (Draw? Video?)
  can register their own `BudgetSlicedListener` without touching
  the budget module.

## Implementation notes

- `src/session/budget.rs`:
  - New: `BudgetSlicedEvent`, `BudgetSlicedListener`,
    `register_sliced_listener`, `sliced_listener_count`,
    `clear_sliced_listeners` (test-only).
  - Modified: `apply_budget_slice` dispatches to listeners and
    swaps the sliced display for the listener's replacement.
  - New: `maybe_escalate_stratum()` (gated by `feature = "stratum"`).
  - Modified: `PreCompactHook::handle` calls
    `maybe_escalate_stratum()` after the `used = 0` reset.
- `src/session/stratum.rs`:
  - New: `SESSION_MODE` static, `current_session_mode`,
    `set_session_mode`, `compress_for_budget`,
    `default_budget_sliced_listener`,
    `register_default_budget_listener`.
- `src/session/executor/mod.rs`:
  - `Executor::build` and the rebuild path call
    `register_default_budget_listener` under
    `#[cfg(all(feature = "budget", feature = "stratum"))]`.
- 3 new tests in `budget.rs` (1 always-on, 2 gated by `feature =
  "stratum"`); 4 new tests in `stratum.rs`. All budget-mutating
  tests use the `shared_budget_test_lock` (WO 7.2 lesson) so
  parallel tests do not race.
- No changes to `src/session/hooks.rs` (the WO's literal
  "PreCompactHook lives in hooks.rs" assumption is wrong — the
  hook lives in `budget.rs`; see WO 6.7). The work is on the
  budget side.
- No changes to `src/session/executor/turn.rs` — the slice
  happens inside `apply_budget_slice` and the listener swap
  returns through the existing `ToolOutcome` return path; the
  post-tool hook records the new size.

## Future work

- Register Stratum content transforms so the
  `default_budget_sliced_listener` produces a measurably shorter
  output. Currently the empty pipeline is identity, so the
  listener is observably a no-op in plain-text tool results.
- A `BudgetSliced` variant on the existing `BusEvent` enum
  (separate from this listener-based mechanism) for verifier
  observability. The async `EventBus` is the right channel for
  observability dashboards; the sync listener is the right
  channel for in-process pipeline coordination.
- A config field to control escalation (e.g.
  `budget.auto_escalate_stratum: bool`, default `true`) so users
  who run Stratum in `Lite` deliberately can opt out.
