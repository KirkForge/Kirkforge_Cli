# ADR-0001: Purpose — output-side sibling of Stratum

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

The KirkForge plugin ecosystem has three families of plugins:

1. **Plugin1 (KirkForge-Plugin)** — input-side routing and
   verification. Splits a task into Brain/Brawn tiers; runs a
   Verifier on the Brawn output; emits ReducedStatePacket as
   the persisted task state. Cheap-model + deterministic-
   verification pattern saves tokens by avoiding the cost of
   asking a frontier model to do work a smaller model can do
   correctly under verification.

2. **Plugin2 (Stratum)** — input-side context compression.
   Reformat transforms (byte-level: JSON compact, log dedupe,
   source collapse, diff summary) reduce the size of tool
   outputs before they enter the model context. Offload
   transforms move full content to an `OffloadStore` and emit a
   marker. The model sees a marker; the user can `cat` the
   marker to retrieve content.

3. **Plugin3 (this plugin)** — output-side and budget control.
   Three concerns:
   - Tool outputs that Stratum does not handle (host-side
     PostToolUse callbacks return content that escaped the
     Reformat pipeline).
   - Token budget — a per-conversation ceiling enforced before
     a request is sent. If the budget is exceeded, auto-
     intervene (slice the new tool output, or refuse and emit
     a compaction prompt).
   - Cost reporting — track usage per turn so the user can see
     which prompts blew the budget.

The structural gap is that both Plugin1 and Plugin2 are
**input-side**: they shape what the model *reads*. Plugin3 is the
**output-side**: it shapes what the model *emits* (via the tool
loop) and what reaches the user's *wallet* (via the budget).

## Decision

### Mission statement

Plugin3 exists to:

1. Slice tool outputs that arrive via host-side hooks
   (PostToolUse) — content that did not pass through Stratum's
   Reformat pipeline because Stratum only sees what the user
   puts in front of the model, not what the host surfaces
   separately.
2. Enforce a token budget on every turn, with auto-intervention
   when the budget is exceeded.
3. Report per-turn and per-session cost so the user can audit
   the savings.

### MVP scope

The MVP ships:

- **Tool output slicing** (ADR-0003, ADR-0006, ADR-0007) —
  the `SlicingTransform` trait, the tool-output detector, the
  parallel slicing orchestrator. Slicing keeps the head and
  tail of a long tool output and offloads the middle to the
  `OffloadStore` (ADR-0004).
- **Token budget** (ADR-0005) — three-state guard (Under /
  Approaching / Over). On `Over`, auto-slices the largest
  recent tool output to bring the session back into budget.
- **Cost reporting** (ADR-0010) — `usage.jsonl` emitted by the
  PostToolUse hook, queryable via `plugin3 report` subcommand.

### Out of scope for MVP

- Persistent knowledge (ADR-0011) — saved findings that
  survive compaction. Deferred; the budget auto-intervention
  is the load-bearing feature.
- Speculative priming (ADR-0012) — predicting the next
  user prompt and pre-computing context. Deferred; speculative
  work is its own ADR series.

### Plugin3 vs Stratum — when does which run?

The two plugins are complementary, not competitive:

| Trigger | Plugin2 (Stratum) | Plugin3 |
|---------|-------------------|---------|
| UserPromptSubmit | (no) | Yes — budget check before sending |
| PreToolUse | (no) | Yes — emit slicing hint for known-bloated tools |
| PostToolUse | (no) | Yes — slice the new tool output |
| PreCompact | (no) | Yes — emit a Compact hint to the host |
| User-pasted content | Yes — Reformat pipeline | (no) |
| File read by agent | Yes — Reformat pipeline | (no) |

Plugin2 runs when content flows **into** the model's
unstructured-input buffer (paste, file read, diff). Plugin3
runs when content flows **out** of a host-side tool loop or
**into** the model via a host-issued prompt.

### Plugin3 vs Plugin1 — distinct by hook surface

Plugin1's hook surface is SessionStart + Subagent (delegation
setup). Plugin3's hook surface is PostToolUse + UserPromptSubmit
+ PreCompact. The two plugins can run side-by-side in the same
session without conflict.

## Consequences

Negative first:

- Three plugins is more surface than one. A contributor must
  pick the right plugin for the right hook. The hook matrix
  above is the canonical reference; the README of each plugin
  cross-links to it.
- Plugin3 depends on Plugin2's `OffloadStore` trait shape
  (ADR-0004). A breaking change to Stratum's `OffloadStore`
  forces a Plugin3 release.
- The MVP scope (slicing + budget) is small. A user who wants
  persistent knowledge must wait for ADR-0011.

Positive:

- Plugin3 fills the structural gap. With it, the plugin
  ecosystem covers input-routing, input-compression, and
  output-control — three independent optimisation dimensions.
- The budget guard is the load-bearing feature. A user who
  installs only Plugin3 still gets a meaningful token-saving
  win, even without Plugin1 or Plugin2.
- The MVP is small enough to ship. Two features, both
  testable, both with clear ADR coverage.

## Implementation notes

The crate layout follows ADR-0002. The MVP test surface is
slicing (golden tests on a known-bloated tool output) plus the
budget (property tests for the three-state transition).

A user who installs only Plugin3 should see a measurable
reduction in token-per-turn on long sessions within one turn
of the budget firing. The first session is the load-bearing
proof that the plugin works.