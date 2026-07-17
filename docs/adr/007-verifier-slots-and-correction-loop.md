# ADR 007: Verifier Slots and Correction Loop

## Status

Accepted

## Date

2026-06-03

## Context

Tool execution is model-driven: the LLM decides what files to edit, what commands to run, and what code to write. But the LLM makes mistakes — it leaves unused variables, introduces security vulnerabilities, creates merge conflicts, and writes code that doesn't compile. These errors compound across turns because the model doesn't independently verify its own output.

Previous approaches:
- Relying on the LLM to self-correct in the next turn — expensive (full round trip), unreliable (model rationalizes its error), and slow
- Running verifiers as models calls — wastes tokens on deterministic checks
- No verification at all — leads to broken state that the user must clean up

The key insight: most post-execution checks are deterministic. They can run in-process, be instantiated as event bus handlers, and produce structured results the system can act on without another LLM call.

## Decision

A verifier-slot system with priority-based truth model and an auto-correction loop, sitting on the event bus.

### Verifier slots

Five slots (`lint`, `types`, `security`, `graph`, `imports`), matching
`SLOT_TO_SIGNAL` in `npm/kirkforge-plugin/packages/orchestrator/src/reducer.ts`
and `VerifierSlot` in `packages/correction-core/src/types.ts`. The original
design had four slots (`lint`, `type-check`, `git`, `security`); `git` was
dropped (dirty-worktree checks moved out of the verifier loop) and `graph` +
`imports` were added. Each slot holds one verifier implementation. Slots are
registered in priority order; lower priority number = runs first.

```rust
pub trait Verifier: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> u8;
    async fn verify(&self, event: &BusEvent) -> Verdict;
}

pub enum Verdict {
    Clean,                              // No issues
    Fixable(FixSuggestion),             // Can auto-correct (with original/replacement)
    Unfixable(VerificationError),       // Needs human or model intervention
    Skipped(String),                    // Verifier skipped (tool unavailable, wrong file type)
}
```

### Truth model

Verifiers run in priority order. The first non-`Clean`, non-`Skipped` verdict wins immediately. This means:

- **Security (priority 1)** — a hardcoded API key or dangerous call (`eval`,
  shell injection) blocks everything; no point linting a file that shouldn't
  exist. Backed by a dedicated security emitter (token-based dangerous-call
  scan), not an alias of the lint rules.
- **Lint (priority 2)** — fixable warnings are caught before type/structural checks.
- **Types (priority 3)** — `tsc` (JS/TS) or `pyright` (Python).
- **Graph (priority 4)** — structural: import-edge extraction reports
  `newEdges`/`brokenEdges`/`cycles` in `state.graph`. A referenced symbol that
  no longer exists surfaces as a broken edge; an import cycle surfaces as
  `cycles ≥ 1`.
- **Imports (priority 5)** — advisory import-hygiene warnings (unused/banned
  imports); a warning source, not fail-closed.

The dropped `git` slot (dirty worktree / merge-conflict checks) was removed from
the verifier loop; those checks, where still relevant, live outside the slot
system.

### Event bus integration

A `VerifierHandler` wraps the slot registry and implements `EventHandler`:

```rust
impl EventHandler for VerifierHandler {
    fn subscribed_kinds(&self) -> Vec<EventKind> {
        vec![Edit, FileWrite, BashExec, GitOperation, ToolError]
    }
}
```

When a tool event fires, `VerifierHandler.verify_event()` extracts the verifier list from the RwLock (to avoid holding the lock across await), runs each verifier against the event data, and collects any `FixSuggestion` results.

### Correction loop

When a verifier returns `Fixable`, the correction loop applies the fix:

```rust
async fn apply_fix(fix: &FixSuggestion) -> bool {
    let content = fs::read_to_string(&fix.file)?;
    let new_content = content.replace(&fix.original, &fix.replacement);
    fs::write(&fix.file, &new_content)
}
```

The loop returns `CorrectionResult` entries that are appended to the conversation as tool results, so the model sees what was corrected. Maximum 3 correction iterations to prevent infinite loops.

### Verifier implementations

| Verifier | Priority | Event | Emitter | Checks |
|----------|----------|-------|---------|--------|
| Security | 1 | `verify.security` | `SecurityEmitter` | API keys, private keys, tokens, dangerous calls (`eval`, shell injection, path traversal) |
| Lint | 2 | `verify.lint` | language-specific lint engine (TS/Py/Sh/C/Rs/Go/SQL) | Fixable warnings, style/safety rules |
| Types | 3 | `verify.types` | `TscEmitter` (JS/TS) / `PyrightEmitter` (Python) | Type errors |
| Graph | 4 | `state.graph` | `GraphEmitter` | Import-edge extraction: `newEdges`/`brokenEdges`/`cycles` |
| Imports | 5 | `verify.imports` | `ImportLintEngine` | Unused/banned imports (advisory, not fail-closed) |

The `git` slot from the original four-slot design was dropped (dirty-worktree checks moved out of the verifier loop). `state.changes` is emitted separately by `ChangesEmitter` from `writtenFiles`; it is not a verifier slot.

## Consequences

**Positive:**
- Deterministic checks — no token cost, no model latency
- Security issues caught before they reach the conversation
- Auto-fix reduces model error propagation across turns
- Priority system prevents wasted verification on already-blocked operations
- Verifiers are testable in isolation (no model needed)

**Negative:**
- Verifiers add latency to the tool execution pipeline (typically <100ms for local checks)
- Security verifier uses substring matching — no regex negation or context-aware analysis
- Lint verifier only supports Rust; Python/JS support is planned but not implemented
- Auto-fix is naive string replacement — may fix the wrong occurrence or mangle formatting
- Correction loop at 3 iterations could still loop if the fix introduces a new issue

## Implementation

- Files: `src/session/verifier/mod.rs` (~430 lines), `lint.rs` (~120 lines), `security.rs` (~185 lines); the `graph` and `imports` slots are implemented in the npm Orchestrator (`npm/kirkforge-plugin/packages/orchestrator/src/emitter-factory.ts`), not the Rust verifier module.
- 24 unit tests across all verifiers and the correction loop
- `VerifierSlots` with configurable max (default 5)