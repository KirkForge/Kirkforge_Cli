# ADR-0003: SlicingTransform + CompactionTransform

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

Plugin3 has two transform concerns that Stratum does not have:

1. **Semantic slicing** — keep the first N bytes and the last M
   bytes of a tool output; offload the middle. The middle is
   *discardable* but the head and tail are not.
2. **Semantic compaction** — summarise the input. The summary
   is *lossy*. The summary is *not* recoverable from the
   original.

Stratum's transforms are byte-level (Reformat) or pointer-level
(Offload). Plugin3 introduces two new categories:

- **SlicingTransform** — lossy in the middle, exact at the
  edges. Reversible via the OffloadStore for the middle section.
- **CompactionTransform** — lossy overall, not reversible.

The two are distinct because:

- A `SlicingTransform` can be undone by retrieving the offload
  marker. A user who needs the middle asks, and gets it back.
- A `CompactionTransform` is irreversible. A user who needs
  detail that the summary elided is out of luck.

Conflating them would mean "summarise, with a marker for the
original" — which is fine in theory but in practice the user
asks "show me the original" and gets a marker, then asks again
"show me the original *content*" and is told it is gone. Two
distinct transforms make the contract honest.

## Decision

### SlicingTransform trait

```rust
// crates/plugin3-core/src/slicing.rs

use crate::store::OffloadStore;

pub trait SlicingTransform: Send + Sync {
    /// Human-readable name.
    fn name(&self) -> &'static str;

    /// Apply the transform. The input is the full content.
    /// The output is the head + tail concatenated with an
    /// offload marker for the middle.
    fn apply(
        &self,
        input: &str,
        store: &dyn OffloadStore,
    ) -> Result<SlicedOutput, TransformError>;
}

pub struct SlicedOutput {
    /// The kept head (first N bytes), exact.
    pub head: String,
    /// The kept tail (last M bytes), exact.
    pub tail: String,
    /// Marker referring to the offloaded middle. None if the
    /// input was small enough that no slicing occurred.
    pub offload_marker: Option<String>,
    /// Bytes saved vs the input. Always >= 0.
    pub bytes_saved: usize,
}
```

### HeadTailSlicer — the canonical slicing impl

```rust
#[derive(Clone, Copy)]
pub struct HeadTailSlicer {
    pub head_bytes: usize,
    pub tail_bytes: usize,
}

impl Default for HeadTailSlicer {
    fn default() -> Self {
        // ponytail: 4096/4096 mirrors the detector's
        // `SLICE_HEAD_BYTES` / `SLICE_TAIL_BYTES`. A
        // contributor who tunes this default without
        // updating the detector's defaults surfaces
        // here via the in-file `head_tail_slicer_default_matches_adr`
        // drift test.
        Self { head_bytes: 4096, tail_bytes: 4096 }
    }
}

impl SlicingTransform for HeadTailSlicer {
    fn name(&self) -> &'static str { "head_tail" }

    fn apply(
        &self,
        input: &str,
        store: &dyn OffloadStore,
    ) -> Result<SlicedOutput, TransformError> {
        let len = input.len();
        // ponytail: saturating_add so a hostile config (head_bytes =
        // usize::MAX) doesn't wrap and turn a giant input into a
        // "fits" case (ADR-0017 § Reproducible builds).
        if len <= self.head_bytes.saturating_add(self.tail_bytes) {
            return Ok(SlicedOutput {
                head: input.to_string(),
                tail: String::new(),
                offload_marker: None,
                bytes_saved: 0,
            });
        }
        // ponytail: byte slicing panics on multi-byte UTF-8; align to
        // char boundaries so non-ASCII tool output (CJK logs, emoji
        // markers) doesn't crash the post-tool-use hook.
        let head_end = floor_char_boundary(input, self.head_bytes);
        let tail_start = ceil_char_boundary(input, len.saturating_sub(self.tail_bytes));
        let (head, tail) = if tail_start > head_end {
            (&input[..head_end], &input[tail_start..])
        } else {
            (input, "")
        };
        let middle = &input[head_end..tail_start];
        let key = store.put(middle.as_bytes())
            .map_err(|e| TransformError::Internal(format!("store: {e}")))?;
        Ok(SlicedOutput {
            head: head.to_string(),
            tail: tail.to_string(),
            // ponytail: format_slice_marker wraps the key with the
            // SLICE_MARKER_PREFIX / SLICE_MARKER_SUFFIX pair (defined
            // in ADR-0004 § OffloadStore). The earlier draft used an
            // inline format string here; that form drifts the wire
            // shape away from `parse_slice_marker`.
            offload_marker: Some(format_slice_marker(&key)),
            bytes_saved: middle.len(),
        })
    }
}
```

The `HeadTailSlicer` is the only slicing transform in the MVP.
A future contributor can add `RegionSlicer` (regex-delimited
sections), `BinarySlicer` (offsets from a binary header), etc.

### CompactionTransform trait

```rust
// crates/plugin3-core/src/compaction.rs

pub trait CompactionTransform: Send + Sync {
    // removed

    fn apply(&self, input: &str) -> Result<CompactedOutput, TransformError>;
}

pub struct CompactedOutput {
    /// The summary.
    pub summary: String,
    /// Bytes saved vs the input.
    pub bytes_saved: usize,
    /// True if this compaction is lossless (None) or lossy.
    pub lossy: bool,
}
```

### LocalSummaryCompactor — MVP impl

The MVP CompactionTransform is the *local* summary — a
heuristic, regex-driven extractor that pulls the first line of
each paragraph, the headings, the JSON keys, the log levels.

```rust
pub struct LocalSummaryCompactor {
    pub max_output_bytes: usize,    // default: 8192
}

impl CompactionTransform for LocalSummaryCompactor {
    fn name(&self) -> &'static str { "local_summary" }

    fn apply(&self, input: &str) -> Result<CompactedOutput, TransformError> {
        let summary = local_summarise(input, self.max_output_bytes);
        let lossy = summary.len() < input.len();
        Ok(CompactedOutput {
            bytes_saved: input.len().saturating_sub(summary.len()),
            summary,
            lossy,
        })
    }
}
```

`local_summarise` is intentionally cheap — it runs in the
PostToolUse hook on the agent's host, so latency matters more
than quality. The MVP does not call an LLM to summarise; that
is a future ADR (compaction-via-LLM is a separate design).

### When to use which

| Transform | Reversible | Use when |
|-----------|-----------|----------|
| `SlicingTransform` (head/tail) | Yes (offload marker) | Tool output with structure — keep the start (the command) and the end (the result), discard the noisy middle (the progress lines) |
| `CompactionTransform` (local) | No | Conversation history — summarise old turns to fit a budget |

### Error contract

Both transforms return `Result<_, TransformError>` (Stratum
ADR-0011 — three variants: `InvalidInput`, `Skipped`,
`Internal`). A transform that *cannot* run on a given input
returns `Ok` with `bytes_saved == 0` (no-op) or
`TransformError::Skipped` (orchestrator may try another).

A transform that panics is a bug. The no-panic property test
(ADR-0016) covers both trait families.

## Consequences

Negative first:

- Two traits instead of one is more API surface. A contributor
  must pick the right trait. The convention: slicing keeps the
  edges, compaction summarises.
- `LocalSummaryCompactor` is heuristic. A future LLM-based
  compactor will be better; the MVP is good-enough-for-now.
- The middle section is offloaded but the user cannot
  `cat`-it — they need the slice marker and the OffloadStore
  retrieval command. ADR-0010 documents the retrieval flow.

Positive:

- Slicing is honest about what is kept and what is discarded.
- Compaction is honest about being lossy.
- Both transforms compose with the OffloadStore trait (ADR-0004).

## Implementation notes

The slicing and compaction modules are siblings, not parent /
child. The orchestrator (ADR-0007) knows about both, the
detector (ADR-0006) does not.

A `SlicedOutput` with no marker (because the input was small)
is the same shape as a no-op — the orchestrator treats both
identically. This keeps the call site uniform.

```rust
pub fn slice_or_skip(
    input: &str,
    slicer: &dyn SlicingTransform,
    store: &dyn OffloadStore,
) -> SlicedOutput {
    match slicer.apply(input, store) {
        Ok(out) => out,
        Err(e) => {
            // ponytail: Skipped is the explicit no-op; everything
            // else is a slicer failure the user can amplify via
            // budget::Over (ADR-0005). Log the non-Skipped case so
            // a host's stderr captures the regression. The MVP does
            // NOT depend on a phantom tracing dep (ADR-0017 §
            // Workspace Cargo.toml); the helper emits one
            // `eprintln!` line tagged `plugin3:` and falls back
            // to the no-op shape. The drift test
            // `adr_0003_slice_or_skip_omits_tracing_warn` pins
            // the absence of the phantom dep.
            if !matches!(e, TransformError::Skipped(_)) {
                eprintln!("plugin3: slicer failed; passing input through: {e}");
            }
            SlicedOutput {
                head: input.to_string(),
                tail: String::new(),
                offload_marker: None,
                bytes_saved: 0,
            }
        }
    }
}
```

The `slice_or_skip` helper is the canonical call site. Tests
cover the Ok branch (passes through verbatim) and the
non-Skipped Err branch (logs + falls back to no-op); the
Skipped branch is folded into the same match arm because
the no-op shape is identical.