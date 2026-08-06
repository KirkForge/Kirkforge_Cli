# Verifier Slot System & Correction Loop

**Source:** KirkForge-Plugin (`packages/orchestrator/src/reducer.ts`, `truth-model.ts`, `correction-loop.ts`, `task-profile.ts`)
**Goal:** Formalize the 4 verifier slots (lint/types/security/graph) from the event bus into a canonical reduction pipeline with a truth-model precedence table. The correction loop runs verify → correct → re-verify until all required slots pass.

## Why

Today, the LLM decides when to lint, when to type-check, and when to stop. This is:
- **Expensive** — every tool call round-trip costs tokens
- **Unreliable** — the LLM may skip verification or misinterpret results
- **Inconsistent** — no structured verdict, just "looks good to me"

A deterministic correction loop fixes all three: required slots are verified every turn, the verdict is computed from a precedence table, and the LLM only gets one job (correct the code).

## The 4 Verifier Slots

```
┌─────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐
│  lint       │  │  types       │  │  security    │  │  graph     │
│  .rs / .py  │  │  tsc --noEmit│  │  safety rules│  │  import    │
│  .js / .ts  │  │  ruff / mypy │  │              │  │  analysis  │
│  .go / .md  │  │  cargo check │  │              │  │            │
└──────┬──────┘  └──────┬───────┘  └──────┬───────┘  └──────┬─────┘
       │                 │                  │                 │
       └─────────────────┴──────────────────┴─────────────────┘
                                    │
                           ┌────────▼────────┐
                           │   StateReducer  │
                           │  (materialized  │
                           │     view)       │
                           └────────┬────────┘
                                    │
                           ┌────────▼────────┐
                           │  Truth Model    │
                           │  (precedence)   │
                           └────────┬────────┘
                                    │
                           ┌────────▼────────┐
                           │   Verdict       │
                           │ pass / fail /   │
                           │ unknown         │
                           └─────────────────┘
```

## Truth Model Precedence

From `truth-model.ts`, the final verdict is computed in strict order:

1. **Protocol integrity break** → `fail` (unterminated artifact markers, truncated output)
2. **Validator pass/fail** → overrides all verifiers (e.g., `cargo test` fails → `fail`)
3. **Validator error/timeout** → `unknown`
4. **Validator not configured** → `unknown` (advisory only)
5. **Required verifier fail** → `fail`
6. **Advisory verifier fail** → `unknown`
7. **All verifiers pass** → `pass`

## Correction Loop Flow

```
1. Snapshot workspace (cp to temp dir)
2. Delegate to LLM (generate code, fix issues)
3. Write artifacts (with path safety)
4. Fire all 4 verifiers (deterministic, no LLM)
5. Reduce to workspace state packet
6. Compute verdict via truth model
7. If fail → build correction prompt from verifier output → go to step 2
8. If pass → commit, clean up temp dir
9. If max corrections hit → fail open
```

## Integration Points

| File | Change |
|------|--------|
| `src/session/` | New `verifier/` module — `reducer.rs`, `truth_model.rs`, `slot.rs` |
| `src/session/executor.rs` | Wire `preflight_check()` before each turn, `postflight_check()` after |
| `src/tools/` | Existing tools stay; verifiers are separate (not tools) |
| `src/shared/mod.rs` | New `Verdict` enum, `WorkspaceState` struct |

## Token Savings

Each correction loop turn consumes tool call tokens. The loop eliminates the *model asking* for verification results — they arrive as structured context pre- and post-turn. Estimated savings: 800-1200 tokens per turn on verification-related round-trips.