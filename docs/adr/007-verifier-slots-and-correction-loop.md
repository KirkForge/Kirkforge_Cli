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

Four fixed slots (lint, type-check, git, security). Each slot holds one verifier implementation. Slots are registered in priority order; lower priority number = runs first.

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

- **Security (priority 1)** — a hardcoded API key blocks everything; no point linting a file that shouldn't exist
- **Lint (priority 2)** — fixable warnings are caught before git operations
- **Git (priority 3)** — dirty worktree or merge conflicts are flagged after modifications
- **Type-check (priority 4)** — reserved for future use (e.g., `cargo check`)

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

| Verifier | Priority | Events | Checks |
|----------|----------|--------|--------|
| Security | 1 | FileWrite | API keys, private keys, tokens, dangerous shell commands, path traversal |
| Lint | 2 | Edit, FileWrite | `cargo clippy` for Rust files; extensible to Python, JavaScript |
| Git | 3 | GitOperation, BashExec | Dirty worktree, merge conflicts, failed operations |

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

- Files: `src/session/verifier/mod.rs` (~430 lines), `lint.rs` (~120 lines), `security.rs` (~185 lines), `git.rs` (~140 lines)
- 24 unit tests across all verifiers and the correction loop
- `VerifierSlots` with configurable max (default 4)