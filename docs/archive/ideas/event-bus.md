# Event Bus — Deterministic Actions

**Source:** KirkForge-Plugin (`packages/core-events`, `packages/orchestrator/src/emitter-factory.ts`)
**Goal:** Replace LLM tool calls for deterministic tasks (lint, type-check, git state, import graph) with local event-driven listeners. Saves tokens, eliminates latency, and produces structured results the LLM consumes as context.

## Why

Every lint/type-check/git-status call today requires:
1. LLM emits a tool call → 100-300 tokens
2. Tool runs locally → fast, but burns context window on the round-trip
3. Tool result fed back to LLM → another 200-1000 tokens
4. LLM interprets result → could error

With an event bus, the agent loop pre-fires all deterministic checks before the LLM turn and injects results as structured system context. The LLM never "asks" for lint results — they're already there.

## Event Types

```
verify.lint       — lint results per file (error count, rule violations, severity)
verify.types      — type checker output (tsc --noEmit, ruff, etc.)
verify.security   — security scan findings (safety-category lint rules)
state.changes     — git diff: files changed, insertions/deletions, untracked files
state.graph       — import/use graph edges, broken edges, cycles
```

## Architecture Sketch

```rust
// Core trait — any source of deterministic context
#[async_trait]
pub trait Verifier: Send + Sync {
    fn kind(&self) -> VerifierKind;
    async fn verify(&self, workspace: &Path) -> Vec<VerifierEvent>;
}

// Event bus — typed pub/sub
pub struct EventBus {
    handlers: Vec<Box<dyn Fn(VerifierEvent) -> Pin<Box<dyn Future<Output=()>> + Send>>>,
}

// Fired before each LLM turn, results injected into system prompt
pub async fn preflight_check(bus: &EventBus, workspace: &Path) -> WorkspaceState {
    let events = futures::future::join_all(
        bus.handlers.iter().map(|h| h.verify(workspace))
    ).await;
    WorkspaceState::from_events(events)
}
```

## Integration Points

| File | Change |
|------|--------|
| `src/session/executor.rs` | Before `run_turn`, fire `EventBus::preflight()` and append results as system context |
| `src/tools/` | Keep bash/grep/read as LLM-driven; lint/type-check/git become event listeners |
| `src/session/` | New `event_bus.rs` module — bus + handlers |
| `src/shared/mod.rs` | New `VerifierKind` enum, `VerifierEvent` struct |

## What This Replaces

Today's LLM-in-the-loop → tomorrow's event-driven:

- `grep` for error patterns → `lint` verifier
- `bash` for `git diff` → `changes` verifier
- `bash` for `tsc --noEmit` → `types` verifier
- `bash` for file listing → `graph` verifier (import/use analysis)

## Token Math

| Action | Today (tool call round-trip) | With event bus (inline) | Savings |
|--------|-----|------|---------|
| Lint check | ~400 tokens | ~50 token result summary | 87% |
| Git status | ~300 tokens | ~30 token summary | 90% |
| Type check | ~500 tokens | ~80 token error list | 84% |
| **Per turn** | **~1200 tokens** | **~160 tokens** | **87%** |