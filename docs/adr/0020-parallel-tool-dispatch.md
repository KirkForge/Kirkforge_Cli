# ADR-020: Parallel Tool Dispatch

- **Status:** Accepted
- **Date:** 2026-07-19

## Context

The executor previously dispatched tool calls sequentially in `dispatch_tool_calls_sequential`. Each call awaited the previous one, so a multi-tool turn spent the sum of all tool latencies even when the tools were independent. Model batches frequently contain independent reads, greps, or bash probes that could run concurrently.

## Decision

Introduce `dispatch_tool_call_batch` with a three-phase design:

1. **Prepare / pre-gate** — run all read-only safety checks that can block a call *before* its body runs: unknown-tool check, plan-mode enforcement, schema validation, permission rules, deny list, URL deny list, bash command check, search-path check, file path guard, and non-file pre-tool hooks. Denied calls are buffered and replayed in input order during Phase 3.
2. **Run** — spawn non-file tool bodies with `tokio::spawn`. A cancellation check between spawns preserves sequential cancellation semantics. File tools are run sequentially in a separate phase so the read-before-edit gate can observe reads completed earlier in the same batch.
3. **Record** — sequentially apply mutable side effects (events, conversation append, read gate, audit log, metrics, carryover, correction loop) in input order. Already-completed results are recorded even if cancellation fires; only missing results short-circuit the remainder.

## Consequences

- Independent non-file tool calls now overlap, reducing turn latency.
- File tools remain ordered so `[read_file(X), edit_file(X)]` in the same batch passes the read-before-edit gate.
- Cancellation semantics are preserved: the first call in a cancelled batch still records its result, later unspawned/uncompleted calls become placeholders.
