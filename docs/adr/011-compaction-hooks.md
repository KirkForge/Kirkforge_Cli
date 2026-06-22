# ADR 011: Tail-Preserving Compaction with pre-compact / post-compact Hooks

## Status

Accepted

## Date

2026-06-22

## Context

Long sessions exceed the model context window.  We already keep a system anchor and the most recent messages verbatim, stub or condense the middle, and optionally ask a local LLM to summarize.  This protects the conversation tail but the process is opaque: the user and external integrations cannot tell when compaction starts, which strategy is chosen, or how much history is lost.

Lifecycle hooks (see `src/session/hooks.rs`) are the project's extension point for external integrations.  They currently fire around tool calls and turns.  Compaction should also be observable so that:

- operators can log or audit when context is dropped;
- downstream tools can react to lossy history changes (e.g., back up the session);
- tests can assert that compaction events carry useful metadata.

## Decision

Emit `pre-compact` and `post-compact` hook events around every compaction pass.

Both events are **fire-and-forget** and run with the same timeout/crash handling as other observational hooks.  They are intentionally not gating hooks: a broken compaction hook must not prevent the agent from continuing to operate.

### Hook contract

Hooks receive the event name in `KF_EVENT` and a JSON payload in `KF_TOOL_ARGS_JSON`:

```json
{
  "message_count": 20,
  "preserve_recent": 2,
  "original_count": 20,
  "result_count": 8,
  "dropped_tool_results": 5,
  "condensed_assistant_turns": 3,
  "summarised_messages": 0,
  "strategy": "naive"
}
```

Field meanings:

| Field | Meaning |
|-------|---------|
| `message_count` | Messages in the conversation before compaction |
| `preserve_recent` | Configured number of recent messages to keep verbatim |
| `original_count` | Messages before compaction (same as `message_count` in current impl) |
| `result_count` | Messages after compaction |
| `dropped_tool_results` | Number of tool results replaced with stubs (naive path) |
| `condensed_assistant_turns` | Number of assistant turns condensed to summaries (naive path) |
| `summarised_messages` | Number of messages compressed into an LLM summary (summarize path) |
| `strategy` | `"pending"` for `pre-compact`, `"naive"` or `"summarize"` for `post-compact` |

### Timing

- `pre-compact` fires at the start of the compaction arm, before either strategy runs, with `strategy: "pending"`.
- `post-compact` fires after the chosen strategy finishes and the conversation is replaced, with the actual strategy and counts.
- If compaction produces no change, `post-compact` still fires with `result_count == original_count`.

## Consequences

**Positive:**
- External observers can now see exactly when and how history is reduced.
- The same hook runner and timeout semantics apply, so no new infrastructure is needed.
- Tests can verify compaction behavior by inspecting hook payloads rather than the conversation log.

**Negative / limitations:**
- The payload is a plain JSON object; future hooks may want the full before/after conversation paths.
- Hook scripts that block or crash are silently skipped after a timeout, so audit guarantees are best-effort only.
- `pre-compact` cannot veto compaction.  If a gating use case appears we can add a decision variant later.

## Implementation

- `src/session/executor.rs`:
  - `CompactHookStats` struct bundles the metadata.
  - `Executor::run_compact_hook` serializes the struct and calls `run_hook`.
  - `compact_rx` arm calls `run_compact_hook("pre-compact", ...)` before work and `run_compact_hook("post-compact", ...)` after work.
- `src/session/hooks.rs`: module docs updated to list the new events and the JSON payload.
- `src/session/executor.rs` tests: `test_compact_hooks_fire_pre_and_post` verifies both events write expected JSON.
