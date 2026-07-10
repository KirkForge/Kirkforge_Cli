# ADR-0007: Parallel slicing orchestrator

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

A session may have several recent tool outputs. The budget
guard (ADR-0005) decides whether to slice; the orchestrator
runs the slice. Slicing is embarrassingly parallel — each
tool output is independent — so a future ADR can introduce
`rayon` to fan out across cores.

Today the orchestrator runs serially: `run_post_tool_use`
slices a single recent output per turn, and the orchestrator
call is on the PostToolUse critical path. The MVP pays a
~0 ms fan-out cost (no thread spawn, no par_iter pool) at
the cost of leaving multi-core throughput on the table for
multi-output batches. The serial loop matches Stratum
ADR-0001 (synchronous, no async runtime).

Two reasons not to slice in serial **were** plausible at
draft time:

1. A session with five recent tool outputs at ~50 ms each
   blocks the PostToolUse hook for 250 ms. The user feels
   the lag.
2. The slice is a no-op for outputs below the threshold; the
   detector (ADR-0006) already classified them, but the
   orchestrator must still iterate them.

ponytail: in practice the PostToolUse hook only slices the
*current* tool output (one entry), not the whole recent
list — the budget guard hands the orchestrator a single
output per invocation. The parallel-fan-out motivation was
solving a problem that the call site doesn't have. The ADR
keeps the `run(&SlicingOrchestrator, &[(String, String,
Option<String>)])` signature so a future batch-mode caller
can swap in `par_iter` without changing the API.

## Decision

### Orchestrator API

```rust
// crates/plugin3-core/src/orchestrator.rs

pub struct SlicingOrchestrator<'a> {
    pub store: &'a dyn OffloadStore,
    pub slicer: &'a dyn SlicingTransform,
    pub detector: DetectorCache,
}

pub struct OrchestratorResult {
    /// Per-output decision: either Keep or Slice with the
    /// resulting marker.
    pub decisions: Vec<(String, SliceDecision)>,
    /// Total bytes saved across all sliced outputs.
    pub total_bytes_saved: usize,
}

/// ponytail: the `Sliced` variant carries `head` and `tail` in
/// addition to `marker` / `bytes_kept` / `bytes_offloaded` —
/// the orchestrator returns the kept bytes alongside the
/// marker so the caller can pass the kept bytes through to
/// the host without re-fetching them. The earlier draft
/// omitted `head` and `tail`, which would have forced the
/// CLI to look up the slice from the store to render the
/// pass-through view (ADR-0003 § HeadTailSlicer already
/// computes these; carrying them avoids the redundant read).
/// ponytail: both variants carry `kind: ToolOutputKind` —
/// the orchestrator already classified the output via
/// `DetectorCache::get_or_detect` to make the Slice/Keep
/// decision. Surfacing it on the decision removes the
/// redundant `detector::detect(...)` call the CLI previously
/// issued on the PostToolUse hot path (per output); the
/// orchestrator's cache already helps the second call
/// within a session — this field eliminates even the first.
pub enum SliceDecision {
    Keep { kind: ToolOutputKind, bytes: usize },
    Sliced {
        kind: ToolOutputKind,
        marker: String,
        head: String,
        tail: String,
        bytes_kept: usize,
        bytes_offloaded: usize,
    },
}

pub fn run(
    orch: &SlicingOrchestrator<'_>,
    outputs: &[(String, String, Option<String>)], // (key, content, tool_name)
) -> OrchestratorResult {
    let mut decisions: Vec<(String, SliceDecision)> =
        Vec::with_capacity(outputs.len());
    for (key, content, tool_name) in outputs {
        let kind = orch.detector.get_or_detect(tool_name.as_deref(), content);
        let bytes = content.len();
        let decision = match should_slice(kind, bytes) {
            Decision::Keep => SliceDecision::Keep { kind, bytes },
            Decision::Slice { keep_head, keep_tail } => {
                // ponytail: static slicer is enough for MVP —
                // the per-kind sizing lives in ADR-0006's
                // threshold table, so the orchestrator hands
                // the detector's keep_head / keep_tail
                // straight to HeadTailSlicer. A future
                // dynamic-slicer ADR refactors this site;
                // SlicedOutput's shape is unchanged.
                let slicer = HeadTailSlicer {
                    head_bytes: keep_head,
                    tail_bytes: keep_tail,
                };
                // ponytail: route through `slice_or_skip`
                // (ADR-0003 § Implementation notes) — the
                // canonical call site that handles Ok and
                // Skipped/non-Skipped error paths. The
                // orchestrator owns detect→decide; the
                // slicer handles its own failure fallback
                // so this site doesn't repeat the log-and-
                // pass-through boilerplate. The
                // non-Skipped-error path emits one
                // `eprintln!` to the host's stderr (no
                // tracing dep — see § Logging below).
                let out = slice_or_skip(content, &slicer, orch.store);
                let bytes_kept = out.head.len() + out.tail.len();
                let bytes_offloaded = bytes.saturating_sub(bytes_kept);
                match out.offload_marker {
                    Some(marker) => SliceDecision::Sliced {
                        kind, marker,
                        head: out.head,
                        tail: out.tail,
                        bytes_kept,
                        bytes_offloaded,
                    },
                    None => SliceDecision::Keep { kind, bytes },
                }
            }
        };
        decisions.push((key.clone(), decision));
    }
    let total_bytes_saved: usize = decisions.iter()
        .map(|(_, d)| match d {
            SliceDecision::Keep { .. } => 0,
            SliceDecision::Sliced { bytes_offloaded, .. } => *bytes_offloaded,
        })
        .sum();
    OrchestratorResult {
        decisions,
        total_bytes_saved,
    }
}
```

### Why serial, not rayon (deferred — Ponytail)

ponytail: the MVP serial loop is deliberate. The PostToolUse
hook only feeds a single `(key, content, tool_name)` tuple
to the orchestrator today — multi-output batches are a
future call site. Adding rayon for a one-entry loop would
add ~50 KB to the binary (per the `par_iter` bench on a
similar workload) for zero measured gain. The
`DetectorCache` already short-circuits the second-call cost
(ADR-0006 § Detection memoisation), so even a hypothetical
batch caller wouldn't see the linear-without-parallelism
slowdown until the batch exceeds a few entries.

If a future ADR introduces a batch-mode caller that pushes
>1 output per turn, the migration is mechanical: replace
the `for (key, content, tool_name) in outputs` loop with
`outputs.par_iter().map(...).collect()` and add
`rayon = "1"` to `plugin3-core`'s `[dependencies]` table.
The `OrchestratorResult` and `SliceDecision` shapes are
unchanged.

The MVP is synchronous (mirrors Stratum ADR-0001) — no
`tokio`, no async runtime, no `Send + 'static` tax. A
future ADR introduces `tokio` if a transform becomes
fundamentally async (LLM call, network fetch).

### Decision: static slicer only

The orchestrator constructs `HeadTailSlicer` with fixed
`head_bytes` / `tail_bytes`. A future contributor who needs
per-kind slicers builds a `SlicingOrchestrator` per kind and
dispatches. The MVP is intentionally simple — the threshold
table in ADR-0006 already encodes the per-kind sizing.

### Logging

ponytail: the earlier draft specified a `tracing::info!`
event per orchestrator run, consumed by the cost-reporting
pipeline (ADR-0010). The MVP does **not** depend on
`tracing` (ADR-0017 § Workspace Cargo.toml). The
orchestrator emits zero events today — the
`OrchestratorResult.decisions` + `total_bytes_saved`
are returned to the caller, and the cost
reporter (ADR-0010) reads `bytes_saved` from there.

The one stderr line the orchestrator can produce today is
`slice_or_skip`'s non-Skipped error fallback (ADR-0003 §
Implementation notes) — that path emits one `eprintln!`
with `plugin3: slicer failed; passing through: <err>`. A
contributor who re-introduces the `tracing::info!` event
must add `tracing = "0.1"` to `plugin3-core`'s dependencies
and update this drift test:

```rust
// crates/plugin3-core/tests/slicing_orchestrator_spec_drift.rs
// (ponytail: pinned by `adr_0007_logging_section_omits_tracing_event`)
```

## Consequences

Negative first:

- ponytail: no parallel slicing today. A future batch-mode
  caller that pushes >1 output per turn pays the serial
  loop's latency. The upgrade path is mechanical (§ Why
  serial, not rayon above); the migration cost is bounded
  by the binary-size budget of `rayon` (~50 KB per the
  `par_iter` bench on a similar workload).
- The orchestrator allocates a `SlicingOrchestrator` per
  invocation. A future ADR pools the struct if profiling
  shows allocation overhead.
- A failing slicer logs and passes through. A user who
  *wants* the slice to fail loud must set the budget to
  `Over` and let the budget guard refuse the turn.

Positive:

- The orchestrator returns a structured `OrchestratorResult`
  the cost reporter can serialise.
- The orchestrator is a pure function of the inputs and the
  store; no hidden state, easy to test.
- The `DetectorCache` is shared across orchestrator calls,
  so the second call on the same content is O(1).

## Implementation notes

The orchestrator lives at
`crates/plugin3-core/src/orchestrator.rs`. It depends only on
`store`, `slicing`, and `detector` (no `rayon`, no `tracing`).
No dependency on `plugin3-cli` or `plugin3-hosts`.

The orchestrator's tests live at
`crates/plugin3-core/tests/orchestrator.rs` and cover:

- Empty input list (no decisions).
- Single small output (Keep).
- Single large output (Sliced, marker present).
- Mixed list (some Keep, some Sliced).
- Failing slicer (passes through, total_bytes_saved == 0).
- Cache hit (second call is faster).

The property test (ADR-0016) asserts the no-panic property on
random `(kind, content)` pairs.

The `// ponytail: static slicer is enough for MVP` comment is
deliberate — a future contributor who needs dynamic sizing
removes it and refactors.

The drift test for this ADR lives at
`crates/plugin3-core/tests/slicing_orchestrator_spec_drift.rs`
and pins the absence of `rayon`/`tracing` claims, the
`Sliced` variant's `head`/`tail` fields, and the free
`run` function shape.