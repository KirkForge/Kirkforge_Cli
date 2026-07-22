# ADR-039: Execution replay + time-travel

## Status

Accepted

## Context

Debugging agent failures requires seeing what the model saw at each turn. The conversation log (`.conv.ndjson`) records tool results but not the full prompt or the model's raw response. Without structured turn traces, debugging agent behavior is guesswork. The benchmark harness (ADR-038) also benefits: you can replay a failed bench task to see where the model went wrong.

## Decision

Persist `TurnRecord`s as NDJSON in `<session-id>.trace.ndjson`. Wire `TraceRecorder` into the executor — after each `run_turn_collecting` call, serialize the turn's events into a `TurnRecord` and append it. Add `kirkforge replay <session-id>` subcommand with `--turn`, `--from`, `--to` range flags. Default: tracing on. `--no-trace` to disable.

### Data types

- `TurnRecord`: turn number, timestamp, prompt messages, model response, tool calls (name + args + result + duration), outcome (Success/Error/Cancelled/Timeout), tokens in/out, duration_ms.
- `RecordedMessage`: role + content.
- `RecordedToolCall`: tool + args + result + duration_ms.
- `TurnOutcome`: Success | Error(String) | Cancelled | Timeout.

### TraceRecorder

Append-only NDJSON writer. One line per turn. `load()` reads all records, skipping corrupt lines. Parallels `ConversationLog`'s NDJSON pattern but is simpler (no checkpoint recovery needed — traces are diagnostic, not crash-critical).

### Replay subcommand

`kirkforge replay <session-id> [--data-dir] [--turn N] [--from N] [--to N]`. Resolves the session id to a trace file under the data directory. Prints each matching turn's summary in plain stdout format. No model calls, no tool execution — read-only.

### Wiring

`TraceRecorder` is an `Option<TraceRecorder>` field on `Executor`. Set via `set_trace()` after construction. After `run_turn_collecting` returns the event vec, the executor aggregates token counts, tool calls, and model response from the events, builds a `TurnRecord`, and appends it to the trace file. The trace file path is `<log_path>.trace.ndjson` (e.g., `<session-id>.conv.ndjson` → `<session-id>.trace.ndjson`).

### CLI

`--no-trace` flag on the `Run` subcommand disables tracing. `TraceRecorder::open()` is called in `run_session()` before the TUI/line-mode branch; the recorder is passed into both code paths.

## Consequences

- Positive: time-travel debugging, bench failure replay, no more "what did the model see?" guessing.
- Positive: trace files are NDJSON — one line per turn, crash-safe (sync_all after each write).
- Negative: disk usage — one trace file per session, ~10-50KB per turn for large contexts.
- Negative: ~1ms latency per turn for the sync write. Acceptable for a coding agent.
- The trace does not include the full system prompt on every turn (only the conversation messages visible via `conversation.all()`). A future enhancement could capture the full prompt including system prompt and tool definitions.