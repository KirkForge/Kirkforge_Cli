# @kirkforge/orchestrator

Verification pipeline engine. Coordinates lint, type-check, security, change-tracking, and graph analysis emitters, then reduces results via `StateReducer` into a `ReducedStatePacket`.

## Design

- **Emitters**: `lint`, `types`, `security`, `changes`, `graph` — each runs independently and emits structured events.
- **StateReducer**: Aggregates events by `taskId`, applies `VerifierPolicy`, and computes a `batteryScore`.
- **Path safety**: `safeRelativePath()` prevents directory traversal on all file inputs.
- **Circuit breaker**: Worker model failures trigger cooldown, then escalate.

## Key exports

- `StateReducer` — event aggregation and policy application
- `createVerificationEmitters()` — factory for all five emitters
- `detectTaskProfile()` / `profileForLanguage()` — language → policy mapping
- `safeRelativePath()` — path sanitization
