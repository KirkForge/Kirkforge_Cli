# ADR-0012: Speculative priming — prediction pipeline (DEFERRED)

- **Status:** Deferred
- **Date:** 2026-06-24

## Context

The current architecture is *reactive*: the user prompts,
the model responds, Plugin3 cleans up. A speculative design
would be *proactive*: the user is about to prompt, Plugin3
pre-computes the context for the likely next prompt.

Three speculative ideas:

1. **Tool-output pre-format** — when a tool result arrives,
   pre-format it for the most likely next slice. The savings
   are small (slice is already O(N)); the cost is the
   pre-format itself.
2. **Context pre-warm** — predict the next prompt and
   pre-compute the prefix that would be injected (system
   prompt, recent findings, related code). Inject
   lazily — only if the prompt actually arrives.
3. **Compaction pre-stage** — when the budget is
   Approaching, pre-compute the `LocalSummaryCompactor`
   output for the oldest N turns so the compaction hint
   is instant.

All three are speculative: they cost CPU and code complexity
for an unproven win.

Three reasons to defer:

1. The MVP scope is slicing + budget. Speculative work is a
   third axis; the budget guard must ship first.
2. The prediction quality of a deterministic predictor (most
   recent tool, longest line, etc.) is low. A future
   contributor who wants real prediction needs an LLM call —
   which costs tokens, the very thing we are trying to save.
3. The reactive pipeline already runs in <50 ms per hook.
   Speculative work competes for the same wall-clock budget.

## Decision

This ADR documents the *deferred* design so a future
contributor has a starting point.

### Prediction source

The MVP's reactive pipeline is the *truth*: what actually
happened. Speculative work is *hypothesis*: what might
happen. The MVP does not maintain a hypothesis; a future ADR
adds one.

A future predictor could use:

- **Markov chain on user prompts** — n-gram on the user's
  recent prompts, predict the next. Cheap; medium quality.
- **Embedding similarity** — embed recent turns, find the
  most-similar historical turn, predict the user's next
  prompt is structurally similar. Medium cost; high quality.
- **LLM prediction** — call a small model to predict the
  user's next prompt. High cost; highest quality.

### Pre-warm cache

A speculative design maintains a *pre-warm cache*:

```rust
pub struct PreWarmCache {
    /// Pre-computed slice for each candidate tool result.
    slices: HashMap<String, SlicedOutput>,
    /// Pre-computed compaction hint for each candidate turn.
    compactions: HashMap<String, CompactHint>,
    /// Validity timestamp — pre-warm entries expire after
    /// `ttl_seconds`.
    ttl_seconds: u64,
}
```

When a tool result arrives, the cache pre-computes the slice
*before* the budget guard runs. When the budget guard decides
to slice, the cached slice is available in O(1).

### Cancellation

Speculative work must be cancellable. If the user does
something different, the pre-warm is wasted. A future ADR
specifies the cancellation policy:

- **Time-based** — entries expire after `ttl_seconds`.
- **Event-based** — a new tool result invalidates the
  pre-warm for the previous tool's slice.
- **Counter-based** — limit pre-warm to N concurrent
  hypotheses; evict the oldest.

The MVP does none of this; the cache is not implemented.

### Failure mode

A speculative design that predicts *wrong* must not corrupt
the reactive pipeline. The cache is *advisory*: a cache miss
falls through to the reactive path. A cache hit skips the
reactive work for that step but does not skip the validation.

## Consequences

Negative first:

- The deferred status means speculative priming does not
  exist in Plugin3 today. A user who wants proactive context
  shaping cannot get it from Plugin3.
- A future contributor who picks up this ADR must design the
  prediction source (Markov / embedding / LLM) and the
  cancellation policy.

Positive:

- The MVP ships smaller. Reactive is enough for the MVP.
- The cache shape is documented. A future contributor does
  not start from scratch.
- The cancellation question is surfaced early. A speculative
  design without cancellation is a memory leak.

## Implementation notes

This ADR is a placeholder. No code lands in the MVP for
speculative priming. The `PreWarmCache` struct is not
implemented.

A future contributor who picks up this ADR should:

1. Pick a prediction source. The Markov chain is the cheapest
   starting point; the LLM predictor is the most expensive
   but highest quality.
2. Specify the cancellation policy. Time-based is the
   simplest.
3. Add a `speculative` feature gate so the MVP build does
   not pull in the prediction dependency.
4. Benchmark: does the pre-warm save more wall-clock than
   it costs?

The ADR will be promoted from `Deferred` to `Accepted` once
the design questions above are answered and a measurement
shows a positive trade.

### Open questions for the future contributor

1. What is the cost of the prediction itself? A Markov chain
   is O(N) on the prompt history; an LLM call is O(N) on
   the prompt plus the model's latency. The savings must
   exceed the cost.
2. What is the hit rate? If the predictor is wrong 80% of
   the time, the cache is mostly waste. The MVP does not
   measure this; a future ADR adds a hit-rate counter.
3. How does the user opt out? A user who does not want
   speculative work should be able to disable it in
   `config.toml`. The MVP has no such flag because there is
   no speculative work to disable.