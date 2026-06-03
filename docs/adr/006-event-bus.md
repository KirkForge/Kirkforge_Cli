# ADR 006: Event Bus — Pub/Sub Dispatcher for Tool Execution Events

## Status

Accepted

## Date

2026-06-03

## Context

Tool execution generates side effects that downstream consumers need to react to: verifiers check results, hooks trigger notifications, loggers record history. Without a structured dispatch mechanism, every new consumer requires threading calls through the executor's tool-handling code. This creates coupling: the executor must know about all consumers, their ordering, and their failure modes.

Additionally, tool events can fire multiple times for the same logical action (retries, repeated edits). Consumers need idempotency guarantees so they don't double-process.

## Decision

A typed event bus with handler registration, content-addressed idempotency, and sequential-per-handler processing.

### Event kinds

Nine event kinds, one per tool operation type:

| Kind | Trigger | Payload |
|------|---------|---------|
| `FileRead` | read_file tool executed | path, size_bytes, truncated |
| `FileWrite` | write_file tool executed | path, content_length |
| `Edit` | edit_file executed | path, diff |
| `BashExec` | bash tool executed | command, exit_code, stdout_len, stderr_len |
| `GitOperation` | git command executed | args, output, success |
| `LintRun` | linter executed | tool, target, findings |
| `TypeCheck` | type checker executed | target, errors, success |
| `SecurityScan` | security scan | target, issues |
| `ToolError` | any tool returns error | tool, error |

### Architecture

```
Tool Execution → BusEvent → EventBus.dispatch()
                               ├── Handler 1 (idempotency check)
                               ├── Handler 2 (idempotency check)
                               └── Handler N (idempotency check)
```

Each handler implements the `EventHandler` trait:

```rust
#[async_trait]
pub trait EventHandler: Send + Sync {
    fn id(&self) -> &str;
    fn subscribed_kinds(&self) -> Vec<EventKind>;
    async fn handle(&self, event: &BusEvent) -> HandlerResult;
}
```

### Idempotency

Events carry a content-derived key computed by hashing the payload fields. The bus maintains a per-handler cache of `(handler_id, EventKind, idem_key)` tuples. If the same handler has already processed an event with the same key, dispatch skips it.

```rust
pub fn idem_key(&self) -> u64 {
    let mut hasher = DefaultHasher::new();
    match self {
        BusEvent::FileRead(e) => e.path.hash(&mut hasher),
        BusEvent::Edit(e) => { e.path.hash(&mut hasher); e.diff.hash(&mut hasher); }
        // ... per-variant hashing of semantically meaningful fields
    }
    hasher.finish()
}
```

The idempotency cache can be cleared via `clear_idem_cache()` to allow re-processing (e.g., after a handler is updated).

### Thread safety

The bus wraps its state in `Arc<Mutex<BusInner>>`. Handlers are called outside the mutex lock to prevent deadlocks if a handler itself dispatches events. The lock is re-acquired only to update the idempotency cache and event history after all handlers complete.

### Event history

The bus retains the last N events (default 100) as `StoredEvent` with kind, payload, timestamp, idem_key, and list of handlers that processed it. This is used for debugging and optional UI display.

## Consequences

**Positive:**
- New consumers register without modifying executor code
- Idempotency prevents double-processing without requiring consumers to track state
- Sequential-per-handler ordering guarantees no concurrent state mutations within a single handler
- Event history aids debugging
- The bus is testable in isolation without tools or a model

**Negative:**
- Async dispatch adds latency to the tool execution pipeline (negligible for deterministic handlers, measurable if handlers make network calls)
- Idempotency cache grows with unique events — bounded by reset/clear operations
- No handler priority ordering within the same event kind (handled at the verifier layer, not the bus)

## Implementation

- File: `src/session/event_bus.rs` (~580 lines)
- 11 unit tests covering dispatch, filtering, idempotency, multi-handler, registration lifecycle, history
- `NoopHandler` provided for testing