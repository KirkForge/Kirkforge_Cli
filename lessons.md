# Lessons — WO 15.7 session (cancel leak + double-record + enabled_plugins gate)

## What I learned about this codebase
- `dispatch_tool_call_batch` Phase 2 has TWO loops that touch `running`:
  the **spawn loop** (pushes `JoinHandle`s) and the **collect loop**
  (awaits them). The cancel leak (bucketlist 2.3) is in the COLLECT loop,
  not the spawn loop: when the collect loop `break`s on `cancelled`, the
  remaining `(idx, handle)` pairs are dropped by the `for` iterator, and
  dropping a `JoinHandle` DETACHES the task (it keeps running). The fix
  is to `handle.abort()` the remaining handles on break. The spawn loop's
  own `break` is fine — its handles go into the collect loop.
- `running.drain(..)` yields a consuming iterator that still owns the
  vec; on `break`, the remaining un-yielded elements are still in the
  vec and can be aborted via the iterator's remaining items
  (`for (_, h) in iter { h.abort(); }`). This preserves input-order
  awaiting (front-to-back) AND aborts the tail.
- Adding a `tokio::task::yield_now()` inside `run_prepared_call`
  (before the cancel check) CHANGES scheduling: it lets the spawn loop
  advance and spawn task N+1 before task N's `tool.run()` body starts,
  which broke `test_cancelled_tool_batch_appends_placeholders` (both
  tools' `call_count` hit 2 instead of 1). The existing test relies on
  the spawn loop's single `yield_now()` being the only yield point so
  task 2 is NOT spawned before `cancelled` is checked. Lesson: do NOT
  add yield points inside spawned-task entry functions; the spawn
  loop's yield is the contract. The `cancel_token.is_cancelled()` check
  without a yield is sufficient for the "already cancelled at spawn"
  case; the abort handles the "spawned before flag flip" case.
- The folded-plugin config key for Budget is `"kirkforge-plugin3"`, NOT
  `"budget"`. The bucketlist item 5.1 text said
  `enabled_plugins.contains("budget")` but the actual config key (per
  `default_plugin_sources()` in `shared/config/tools.rs:63` and
  `FOLDED_PLUGINS` in `plugin_tools/loader.rs:33`) is
  `"kirkforge-plugin3"`. The feature flag is `"budget"`; the plugin
  name / `enabled_plugins` key is `"kirkforge-plugin3"`. Always check
  the actual config key, not the WO text.
- `ToolDef.name` and `.description` are `&'static str`, not `String`.
  Clippy (`useless_conversion`) flags `.into()` on `&str` literals for
  these fields. Use bare string literals: `name: "sleep_a"` not
  `name: "sleep_a".into()`.
- `ToolError::AccessDenied { message }`'s `message` field is already
  the full user-facing string ("🔒 Access denied: {msg}").
  `ToolError::to_user_message()` prepends "Access denied: " again, and
  `handle_tool_outcome` for `Failure` wraps in "Error: ". So to record
  a pre-built denial ONCE without re-prefixing, read the `message`
  field directly and emit the `Role::Tool` message + `TurnEvent`
  manually — do NOT route through `handle_tool_outcome` (it would
  double-prefix).
- `cargo check -p kirkforge --lib` takes ~6-7 min on a cold build when
  other worktrees are building in parallel (file lock contention + CPU
  saturation). The full workspace test suite took ~6 min (363s for the
  biggest binary). Budget ~15-20 min for the full gate when parallel
  worktrees are active.

## What I tried that didn't work
- First attempt added `yield_now()` + `cancel_token.is_cancelled()` in
  `run_prepared_call`. This broke
  `test_cancelled_tool_batch_appends_placeholders` (call_count 2 vs 1)
  because the extra yield let task 2 spawn before cancellation was
  checked. Fixed by removing the yield and keeping only the token
  check (catches already-cancelled-at-spawn; the collect-loop abort
  catches the rest).
- First Phase 3 fix for 2.8 routed the `AccessDenied` outcome through
  `handle_tool_outcome`, which double-prefixed the message ("Error:
  Access denied: 🔒 Access denied: ..."). Fixed by reading the
  `message` field directly and emitting the event + message manually,
  mirroring the `GuardVerdict::Denied` branch in `record_tool_result`.

## What I'd do differently
- Before adding a yield point inside a spawned task, trace the
  existing cancellation test's timing assumptions. The spawn loop's
  single `yield_now()` is a load-bearing scheduling contract; adding a
  second one inside the task body changes which tasks get spawned
  before the flag check.