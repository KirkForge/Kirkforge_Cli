# ADR-075: Emission flattening for the executor-backed ModelClient

- **Status:** Accepted
- **Date:** 2026-08-19
- **Related:** [ADR-073](./073-tools-session-layering-ports.md) (tools↔session
  layering ports — the `TaskSpawner` seam this adapter builds on)

## Context

`kf-orchestrator`'s `ModelClient` trait (WO 29.7) models one model turn:
`execute(&TaskBrief) -> Emission`, where `Emission` carries `content`,
usage fields, `format` (echoing the delegation mode), and
`finish_reason`. The trait had no production implementation — tests used
`RecordingClient`, and the binary's plugin-tools verify commands stubbed
out citing that gap.

WO 35.6 wires the kf-code `Executor` into this trait via
`src/session/executor_adapter.rs`. The semantic mismatch to resolve
first: `Emission` models **one model turn**, while the Executor produces
a **multi-turn tool-using session** — N model calls, tool dispatch
between them, continuation rounds on truncation.

## Decision

The adapter **flattens** the session into the existing `Emission` shape:

- `content` = the **final assistant message** of the session (same
  extraction the `task` tool's summary uses).
- `prompt_tokens` / `completion_tokens` = the **sum** of every turn's
  `CostStats` event; `total_tokens` = their sum.
- `format` echoes `TaskBrief.template` verbatim (the trait contract).
- `finish_reason` is derived from the turn outcome: continuation
  exhaustion → `"length"`; a session that ended with pending tool calls
  (hit the turn budget mid-dialog) → `"tool_calls"`; otherwise `"stop"`.
- `retried` stays `false` (retry policy is the caller's concern).

The alternative — extending `Emission` with a session variant carrying
the full conversation — was **rejected** because every consumer of
`Emission.content` in the orchestrator's mode executors (`persist_code_blocks`,
`parse_jsonl_artifacts`, `parse_decomposition`, schema-contract
extraction) parses `content` as text. A session variant would fork every
one of those paths for no current consumer, and the TS orchestrator this
crate ports had the same flattening (its `Agent.run` returns the final
message with accumulated usage).

## Consequences

- Mode executors work unchanged against a real model session.
- Mid-session information (intermediate tool results, per-turn costs) is
  not visible to the orchestrator. That is acceptable today: nothing in
  kf-orchestrator consumes it. If a future mode needs the transcript,
  add a `session_log` field to `Emission` — additive, no fork.
- `finish_reason: "tool_calls"` signals an unfinished session (turn
  budget hit); correction-loop consumers can treat it like the TS
  orchestrator treated incomplete responses.
- The token sum double-counts re-sent context across turns (each turn's
  prompt includes the whole prefix). This matches how the TS
  orchestrator reported session usage and what `OrchestratorStats`
  expects; it is a session-cost figure, not a marginal-cost figure.
