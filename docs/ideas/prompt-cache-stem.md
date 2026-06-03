# Prompt Cache & Stem Agent Pattern

**Source:** vix README (stem agent pattern), vix `internal/daemon/workflow.go` (fork_from)
**Goal:** Maximize prompt cache hit rate by keeping the system prompt identical across workflow phases. Cache is keyed on `(model, system_prompt)`, so every phase that shares the same system prompt hits cache on the user messages.

## The Problem

Today's naive approach:
```
Phase 1: { system: "Explore: ...", user: "..." }
Phase 2: { system: "Execute: ...", user: "..." }
```

Different system prompts → cache miss on phase 2. Cache is keyed on (model, system prompt hash). Even one character difference = cold cache.

## The Stem Agent Pattern

```
Shared stem (same every phase):
  { system: "You are a coding agent. Tools: [bash, read_file, ...]" }

Phase 1 (explore):
  { system: "...", user: "Explore the codebase: $(task)" }

Phase 2 (execute):
  { system: "...", user: "Based on exploration, implement: $(task)" }
```

The system prompt is **identical** across all phases. Phase-specific instructions go in user messages — which are *output* tokens for the previous turn, and cache is keyed on *input* tokens. So changing user messages between phases does NOT invalidate the cache. Only the shared system prompt matters.

## Cache Math

| Strategy | Phase 1 input | Phase 2 input | Phase 3 input | Total input | Cache hits |
|----------|---------------|---------------|---------------|-------------|------------|
| Naive (per-phase system prompt) | 8K sys + 1K user = 9K | 8K sys + 1K user = 9K | 8K sys + 1K user = 9K | 27K | 0 (all miss) |
| Stem agent (shared sys prompt) | 8K sys + 1K user = 9K | 8K cached + 1K new = 1K | 8K cached + 1K new = 1K | 11K | 2 (2nd/3rd phase hit) |
| **Savings** | | | | **59% fewer input tokens** | |

## Implementation

The system prompt builder (`src/session/prompt.rs::build()`) already produces a model-type-and-tools-aware system prompt. The stem agent pattern requires:

1. **Don't change system prompt between turns** — the system prompt is set once per session, not regenerated per phase
2. **Phase instructions go in user messages** — the first user message of each phase contains the phase-specific instruction
3. **fork_from shares the system prompt** — when forking a session, the system prompt (message[0]) is preserved

## Integration Points

| File | Change |
|------|--------|
| `src/session/prompt.rs` | Cache system prompt per model+tools combo (already has a cache field, verify it works) |
| `src/session/executor.rs` | Don't rebuild system prompt every turn — use cached version |
| `docs/ideas/workflow-engine.md` | Workflow steps default to `fork_from=prior_step` |