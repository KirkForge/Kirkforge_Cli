# ADR-031: PlanReason trace events

- **Status:** Accepted
- **Date:** 2026-07-20

## Context

Operational metrics already record *what* happened (tool calls, verifier verdicts, turns, approvals), but they don't explain *why* a planner made a decision. The sixth-pass review identified this as the second-biggest observability gap: operators can see that a tool was selected or that context was truncated, but not the model-declared or heuristic reason behind it.

## Decision

Add a `PlanReason` variant to `MetricEvent` in `src/shared/metrics.rs` that carries a structured decision trace.

### Decision-kind enum

```rust
pub enum PlanDecisionKind {
    ToolSelect,
    ContextTruncate,
    MemoryRetrieve,
    PromptFailure,
    CompactionTrigger,
    ModelSelect,
}
```

The `PlanReason` event has these fields:

- `decision_kind: PlanDecisionKind` — what kind of planning decision was made.
- `reason: String` — model-declared or heuristic explanation.
- `related_id: Option<String>` — tool-call id, memory slug, context-window id, etc.
- `confidence: f64` — 0.0..1.0 confidence when a heuristic produced the decision.

### Emit points

| Source file | Decision kind | Reason content | `related_id` |
|---|---|---|---|
| `src/session/executor/turn.rs` | `ToolSelect` | Model thinking block if present, else `"model-emitted tool call"` | Tool-call id |
| `src/session/executor/loop_.rs` | `CompactionTrigger` | `"budget exceeded at N tokens (threshold T)"` | `None` |
| `src/session/executor/dispatch.rs` | `ContextTruncate` | `"max_tool_result_chars=N hit"` | Tool-call id |
| `src/session/prompt/mod.rs` | `MemoryRetrieve` | `"query='...' matched memory 'slug'"` | Memory slug |
| `src/adapters/mod.rs` | `PromptFailure` | `"connect error on attempt N"`, `"HTTP SSS transient error on attempt N"`, etc. | `None` |

### OTel mapping

`MetricEvent::to_otel_attrs` emits a span named `plan.reason` with attributes:

- `plan.decision_kind` — stringified variant name (e.g. `"ToolSelect"`).
- `plan.reason` — the human-readable reason.
- `plan.confidence` — the confidence value.
- `plan.related_id` — present only when `related_id` is `Some`.

When the `otel` feature is enabled and `OTEL_EXPORTER_OTLP_ENDPOINT` is set, the existing `emit_event_span` path exports these attributes automatically; no additional export code is required.

### Summary behaviour

`PlanReason` events are intentionally excluded from `MetricsSummary` counts. They are tracing/observability events, not success/failure counts.

## Consequences

Positive:

- Operators can answer "why" questions directly from the NDJSON log or OTel backend.
- No new dependencies; the existing `MetricEvent` + optional OTel pipeline carries the new variant.
- Confidence field gives future heuristics a place to report uncertainty.

Negative:

- `PlanReason::ToolSelect` in `turn.rs` only captures a reason when the adapter delivers a non-empty `Thinking` stream. Adapters that don't expose reasoning will record the fallback `"model-emitted tool call"`.
- Memory retrieval emits one event per selected fact; very large `memory_top_n` settings will generate proportionally more events.

## Tests

- `shared::metrics::tests::test_plan_reason_round_trip` — records a `PlanReason` and verifies it round-trips through the NDJSON log.
- `shared::metrics::tests::test_otel_attrs_from_plan_reason` (otel feature) — verifies the four OTel attributes are produced with correct values.
- `session::executor::tests::test_plan_reason_emitted_after_tool_call` — uses the executor test harness with a `MockAdapter` to assert a `ToolSelect` `PlanReason` is recorded after a model-issued tool call.
