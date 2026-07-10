//! `SlicingOrchestrator` — fan-out detect → decide → slice over a batch
//! of recent tool outputs. Per ADR-0007.
//!
//! ponytail: MVP implementation iterates serially. The ADR's rayon
//! `par_iter` is the upgrade path when a caller pushes >1 output
//! per turn; today `run_post_tool_use` slices a single recent output.
//! API shape matches the ADR so the swap is mechanical when needed.
//! Adding rayon now would cost ~50 KB for zero measured gain.

use std::cell::RefCell;
// ponytail: `elapsed_us` was removed from `OrchestratorResult` —
// Instant::now() had no consumer in the workspace (ADR-0007
// reserved the field for a future cost reporter that hasn't
// materialised). Re-add the import + Instant::now() call when
// the cost reporter lands and actually reads it.

use blake3;

use crate::detector::{self, Decision, ToolOutputKind};
use crate::slicing::{slice_or_skip, HeadTailSlicer, SlicingTransform};
use crate::store::OffloadStore;
use crate::text::floor_char_boundary;

/// Per-output verdict returned to the caller.
///
/// ponytail: `kind` is the `ToolOutputKind` the orchestrator already
/// computed via `DetectorCache::get_or_detect` to make the Slice/Keep
/// decision. Carrying it on the decision removes the redundant
/// `detector::detect(...)` call the CLI previously issued on the
/// `PostToolUse` hot path (per output). The orchestrator's cache hits
/// help the second call within a session; this field eliminates
/// even the first call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SliceDecision {
    Keep {
        kind: ToolOutputKind,
        bytes: usize,
    },
    Sliced {
        kind: ToolOutputKind,
        marker: String,
        head: String,
        tail: String,
        bytes_kept: usize,
        bytes_offloaded: usize,
    },
}

/// Aggregate result across the batch.
#[derive(Clone, Debug)]
pub struct OrchestratorResult {
    pub decisions: Vec<(String, SliceDecision)>,
    pub total_bytes_saved: usize,
}

/// Memoisation for `detect`. The detector is pure over `(tool_name,
/// content)` so a tiny map cuts repeat work — the ADR calls out the
/// second-call-is-O(1) property.
///
/// ponytail: `RefCell` so the orchestrator can mutate the cache
/// through `&self` per ADR-0007's API. A `&mut SlicingOrchestrator`
/// would force callers to plumb mutability through CLI handlers
/// that already share the orchestrator across threads.
/// ponytail: cap on the detector cache. ADR-0007 § Orchestrator API
/// notes "the second call on the same content is O(1)" but doesn't
/// name the bound. 64 entries × ~32 bytes each ≈ 2 KB worst case;
/// a future contributor can swap for an LRU if eviction matters.
/// Exported as a `pub(crate)` constant so the drift test can pin
/// the value AND exercise the cap behaviour without a magic-number
/// copy-paste.
pub(crate) const DETECTOR_CACHE_CAP: usize = 64;

#[derive(Default)]
pub struct DetectorCache {
    entries: RefCell<std::collections::HashMap<(Option<String>, blake3::Hash), ToolOutputKind>>,
}

impl DetectorCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_detect(&self, tool_name: Option<&str>, content: &str) -> ToolOutputKind {
        // ponytail: cap the cache so a long session doesn't grow
        // unbounded. 64 entries × ~32 bytes each ≈ 2 KB worst case;
        // a future contributor can swap for an LRU if eviction
        // matters. The cap is intentionally generous vs the
        // "few recent outputs" workload.
        //
        // ponytail: the cache key includes a BLAKE3 hash of the
        // *head* (first 1024 bytes, char-boundary aligned) — the
        // shape detector (`from_shape`) reads only the head, so
        // two inputs that share a head share a detected kind.
        // Earlier the key was `(tool_name, content.len())`; two
        // equally-sized outputs with different shapes (e.g. an
        // 8 KB cargo-test body vs an 8 KB compiler body) collided
        // and the second call returned the cached kind of the
        // first. The hash collapses the head to 32 bytes and
        // distinguishes same-length distinct-shape inputs.
        let mut entries = self.entries.borrow_mut();
        if entries.len() >= DETECTOR_CACHE_CAP {
            entries.clear();
        }
        let head_end = floor_char_boundary(content, 1024.min(content.len()));
        let head_hash = blake3::hash(&content.as_bytes()[..head_end]);
        let key = (tool_name.map(str::to_owned), head_hash);
        entries
            .entry(key)
            .or_insert_with(|| detector::detect(content, tool_name))
            .to_owned()
    }

    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
}

pub struct SlicingOrchestrator<'a> {
    pub store: &'a dyn OffloadStore,
    pub slicer: &'a dyn SlicingTransform,
    pub detector: DetectorCache,
}

/// Inputs are `(key, content, tool_name)`. `tool_name` may be `None`
/// when the host didn't tag the output.
pub fn run(
    orch: &SlicingOrchestrator<'_>,
    outputs: &[(String, String, Option<String>)],
) -> OrchestratorResult {
    let mut decisions: Vec<(String, SliceDecision)> = Vec::with_capacity(outputs.len());
    for (key, content, tool_name) in outputs {
        let kind = orch.detector.get_or_detect(tool_name.as_deref(), content);
        let bytes = content.len();
        let decision = match detector::should_slice(kind, bytes) {
            Decision::Keep => SliceDecision::Keep { kind, bytes },
            Decision::Slice {
                keep_head,
                keep_tail,
            } => {
                // ponytail: the ADR's static-slicer constraint means
                // we always use HeadTailSlicer with the per-decision
                // head/tail. A future dynamic-slicer ADR refactors
                // this site; the SlicedOutput shape is unchanged.
                let slicer = HeadTailSlicer {
                    head_bytes: keep_head,
                    tail_bytes: keep_tail,
                };
                // ponytail: route through `slice_or_skip` (ADR-0003
                // § Implementation notes) — the canonical call site
                // that handles both the Ok path and the
                // Skipped/non-Skipped error paths. The orchestrator
                // owns detect→decide; the slicer handles its own
                // failure fallback so this site doesn't repeat the
                // log-and-pass-through boilerplate.
                let out = slice_or_skip(content, &slicer, orch.store);
                let bytes_kept = out.head.len() + out.tail.len();
                let bytes_offloaded = bytes.saturating_sub(bytes_kept);
                match out.offload_marker {
                    Some(marker) => SliceDecision::Sliced {
                        kind,
                        marker,
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
    let total_bytes_saved: usize = decisions
        .iter()
        .map(|(_, d)| match d {
            SliceDecision::Keep { .. } => 0,
            SliceDecision::Sliced {
                bytes_offloaded, ..
            } => *bytes_offloaded,
        })
        .fold(0, usize::saturating_add);
    OrchestratorResult {
        decisions,
        total_bytes_saved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryOffloadStore;

    fn small() -> String {
        "small".into()
    }
    fn large() -> String {
        // 50 KB cargo-test-shaped body: triggers TestRunner detector,
        // then crosses the 8 KB threshold so should_slice decides Slice.
        let mut s = String::from("running 5 tests\ntest foo ... ok\n");
        s.push_str(&"x".repeat(50_000));
        s.push_str("\ntest bar ... FAILED\n");
        s
    }

    fn orch<'a>(
        store: &'a InMemoryOffloadStore,
        slicer: &'a HeadTailSlicer,
    ) -> SlicingOrchestrator<'a> {
        SlicingOrchestrator {
            store,
            slicer,
            detector: DetectorCache::new(),
        }
    }

    #[test]
    fn empty_input_list_yields_no_decisions() {
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let r = run(&orch(&store, &slicer), &[]);
        assert!(r.decisions.is_empty());
        assert_eq!(r.total_bytes_saved, 0);
    }

    #[test]
    fn single_small_output_keeps() {
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let r = run(&orch(&store, &slicer), &[("k".into(), small(), None)]);
        assert_eq!(r.decisions.len(), 1);
        // ponytail: small() is "small" with no tool_name and no
        // shape signal — the detector returns Unknown. The kind is
        // surfaced on the decision so the CLI can render it without
        // a redundant detect call.
        assert_eq!(
            r.decisions[0].1,
            SliceDecision::Keep {
                kind: ToolOutputKind::Unknown,
                bytes: 5
            },
        );
        assert_eq!(r.total_bytes_saved, 0);
    }

    #[test]
    fn single_large_output_slices_with_marker() {
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let r = run(&orch(&store, &slicer), &[("k".into(), large(), None)]);
        let SliceDecision::Sliced {
            marker,
            bytes_kept,
            bytes_offloaded,
            ..
        } = &r.decisions[0].1
        else {
            panic!("expected Sliced, got {:?}", r.decisions[0].1);
        };
        assert!(marker.starts_with(crate::store::SLICE_MARKER_PREFIX));
        assert!(bytes_offloaded > &0);
        assert_eq!(r.total_bytes_saved, *bytes_offloaded);
        // kept head + tail < input bytes.
        assert!(*bytes_kept < large().len());
    }

    #[test]
    fn mixed_list_some_kept_some_sliced() {
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let r = run(
            &orch(&store, &slicer),
            &[
                ("small".into(), small(), None),
                ("large".into(), large(), None),
                ("tiny".into(), "x".into(), None),
            ],
        );
        assert_eq!(r.decisions.len(), 3);
        // ponytail: the `kind` field on each decision is
        // load-bearing for the CLI's PostToolUse note text
        // (`sliced TestRunner (...)` vs `sliced Compiler (...)`).
        // The earlier shape asserted only the variant
        // (Keep / Sliced) — a regression where the orchestrator
        // swapped kinds (e.g. returning Sliced for the small
        // entry and Keep for the large one) would still pass
        // the bare-matches assertions. Pin both the variant
        // and the kind here.
        match &r.decisions[0].1 {
            SliceDecision::Keep { kind, .. } => assert_eq!(
                *kind,
                ToolOutputKind::Unknown,
                "small() has no shape signal → Unknown"
            ),
            SliceDecision::Sliced { .. } => panic!("expected Keep for small, got Sliced"),
        }
        match &r.decisions[1].1 {
            SliceDecision::Sliced { kind, marker, .. } => {
                assert_eq!(
                    *kind,
                    ToolOutputKind::TestRunner,
                    "large() is cargo-test-shaped → TestRunner"
                );
                assert!(marker.starts_with(crate::store::SLICE_MARKER_PREFIX));
            }
            SliceDecision::Keep { .. } => panic!("expected Sliced for large, got Keep"),
        }
        match &r.decisions[2].1 {
            SliceDecision::Keep { kind, .. } => assert_eq!(
                *kind,
                ToolOutputKind::Unknown,
                "tiny() has no shape signal → Unknown"
            ),
            SliceDecision::Sliced { .. } => panic!("expected Keep for tiny, got Sliced"),
        }
        // Total saved = sum of the single Sliced row's offloaded bytes.
        let sliced_offloaded = match &r.decisions[1].1 {
            SliceDecision::Sliced {
                bytes_offloaded, ..
            } => *bytes_offloaded,
            SliceDecision::Keep { .. } => unreachable!(),
        };
        assert_eq!(r.total_bytes_saved, sliced_offloaded);
    }

    #[test]
    fn failing_slicer_passes_through_with_zero_savings() {
        // ponytail: a buggy store shouldn't crash the orchestrator;
        // we log via eprintln and return Keep so the turn survives.
        struct FailStore;
        impl OffloadStore for FailStore {
            fn put(&self, _: &[u8]) -> Result<String, crate::store::StoreError> {
                Err(crate::store::StoreError::Backend("nope".into()))
            }
            fn get(&self, _: &str) -> Result<Vec<u8>, crate::store::StoreError> {
                Err(crate::store::StoreError::Backend("nope".into()))
            }
            fn len(&self) -> usize {
                0
            }
            fn backend_name(&self) -> &'static str {
                "fail"
            }
        }
        let store = FailStore;
        let slicer = HeadTailSlicer::default();
        let o = SlicingOrchestrator {
            store: &store,
            slicer: &slicer,
            detector: DetectorCache::new(),
        };
        let r = run(&o, &[("k".into(), large(), None)]);
        assert_eq!(r.total_bytes_saved, 0);
        // ponytail: pin the exact bytes on the Keep decision. The
        // orchestrator routes through `slice_or_skip` (ADR-0003)
        // whose fallback sets `head = input` — a contributor who
        // changes the fallback to `head = String::new()` would
        // shrink the Keep's `bytes` field to 0 and lose the
        // pass-through semantics. Drift catches here.
        assert!(
            matches!(r.decisions[0].1, SliceDecision::Keep { kind: _, bytes } if bytes == large().len()),
            "failing slicer must pass the original bytes through, got {:?}",
            r.decisions[0].1
        );
    }

    #[test]
    fn detector_cache_hits_on_second_call() {
        // ponytail: ADR-0007 promises second-call-is-faster. We
        // assert the cache's `len()` grows and returns the cached
        // kind. Timing would be flaky on shared CI; the cache size
        // is the deterministic signal.
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let o = orch(&store, &slicer);
        assert_eq!(o.detector.len(), 0);
        let _ = run(&o, &[("k".into(), large(), Some("cargo test".into()))]);
        assert_eq!(o.detector.len(), 1);
        // Second call with same (tool_name, head_hash) hits the
        // cache — `large()`'s head BLAKE3-hashes to the same
        // 32-byte value both times, so the second lookup finds
        // the entry from the first call.
        let _ = run(&o, &[("k2".into(), large(), Some("cargo test".into()))]);
        assert_eq!(
            o.detector.len(),
            1,
            "second call must reuse the cache entry"
        );
    }

    #[test]
    fn sliced_decision_bytes_offloaded_equals_input_minus_kept() {
        // ponytail: ADR-0007 § Orchestrator API specifies the
        // per-decision math: bytes_kept = head.len() + tail.len(),
        // bytes_offloaded = bytes.saturating_sub(bytes_kept).
        // bytes_offloaded is what `emit_usage` records as
        // `bytes_saved`, so a contributor who flips it to
        // `bytes - head.len()` (forgetting the tail) silently
        // under-reports savings by the tail bytes — `plugin3
        // report --summary` shows a smaller number than reality.
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let o = orch(&store, &slicer);
        let input = large();
        let r = run(
            &o,
            &[("k".into(), input.clone(), Some("cargo test".into()))],
        );
        let SliceDecision::Sliced {
            bytes_kept,
            bytes_offloaded,
            ..
        } = &r.decisions[0].1
        else {
            panic!("expected Sliced, got {:?}", r.decisions[0].1);
        };
        // bytes_kept + bytes_offloaded must equal the input size
        // (saturating prevents underflow on edge cases; for our
        // fixture input > head+tail so saturating is a no-op).
        assert_eq!(
            *bytes_kept + *bytes_offloaded,
            input.len(),
            "bytes_kept + bytes_offloaded must equal input length",
        );
        // bytes_offloaded must be strictly positive for the
        // test fixture (large() is much bigger than head+tail).
        assert!(*bytes_offloaded > 0, "large fixture must offload > 0 bytes");
    }

    #[test]
    fn total_bytes_saved_sums_only_sliced_offloaded() {
        // ponytail: ADR-0007 says total_bytes_saved sums the
        // bytes_offloaded of Sliced rows only. Keep rows
        // contribute 0. A contributor who summed Keep's
        // `bytes` field in too (e.g. `*bytes_kept` → entire
        // input size) inflates the report by input length.
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let o = orch(&store, &slicer);
        let small = "tiny".to_string();
        let r = run(
            &o,
            &[
                ("keep1".into(), small.clone(), None),
                ("slice1".into(), large(), None),
                ("keep2".into(), small.clone(), None),
            ],
        );
        // Find the sliced row and assert total == its bytes_offloaded.
        let sliced = r
            .decisions
            .iter()
            .find(|(_, d)| matches!(d, SliceDecision::Sliced { .. }))
            .expect("at least one Sliced decision");
        let SliceDecision::Sliced {
            bytes_offloaded, ..
        } = sliced.1
        else {
            unreachable!()
        };
        assert_eq!(
            r.total_bytes_saved, bytes_offloaded,
            "total_bytes_saved must equal the single Sliced row's bytes_offloaded; \
             Keep rows contribute 0"
        );
    }

    #[test]
    fn detector_cache_cap_constant_is_pinned() {
        // ponytail: the cap is the load-bearing bound that prevents
        // the cache from growing unbounded across a long session.
        // A contributor who tunes it (64 → 256 or 64 → 16) changes
        // the worst-case memory footprint silently — the behaviour
        // test below surfaces the change; this attribute test catches
        // a constant-only change for review.
        assert_eq!(DETECTOR_CACHE_CAP, 64);
    }

    #[test]
    fn detector_cache_distinguishes_same_length_different_shape() {
        // ponytail: regression for the `(tool_name, content.len())`
        // cache-key collision. Two 8 KB outputs with no tool name
        // and different shapes used to share the same cache entry
        // — the second call returned the cached kind of the first,
        // not its own detected kind. The fix hashes the head with
        // BLAKE3 so distinct shapes hash to distinct keys. Pin
        // both sides of the assertion (the cache MUST grow to two
        // entries AND the two returns MUST differ).
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let o = orch(&store, &slicer);

        // Two 8 KB bodies, both `tool_name=None` so the shape
        // detector runs in full and decides between TestRunner and
        // Compiler. Padding pads both to exactly 8192 bytes.
        let cargo_prefix = "running 5 tests\ntest foo ... ok\n";
        let rustc_prefix = "error[E0001]: mismatched types\n";
        let cargo = format!("{cargo_prefix}{}", "x".repeat(8192 - cargo_prefix.len()));
        let rustc = format!("{rustc_prefix}{}", "x".repeat(8192 - rustc_prefix.len()));
        assert_eq!(
            cargo.len(),
            rustc.len(),
            "fixtures must be equal length so the OLD (length-based) cache key would collide"
        );

        let kind_a = o.detector.get_or_detect(None, &cargo);
        let kind_b = o.detector.get_or_detect(None, &rustc);
        assert_eq!(
            kind_a,
            ToolOutputKind::TestRunner,
            "cargo-test-shaped body must detect as TestRunner; got {kind_a:?}"
        );
        assert_eq!(
            kind_b,
            ToolOutputKind::Compiler,
            "compiler-shaped body must detect as Compiler (NOT the cached TestRunner \
             of the previous equally-sized call); got {kind_b:?}"
        );
        // ponytail: pin that the cache grew to two entries — proves
        // the new key (tool_name, head_hash) distinguished the two
        // inputs and didn't coalesce them.
        assert_eq!(
            o.detector.len(),
            2,
            "distinct-shape equally-sized inputs MUST occupy distinct cache entries; \
             got len={}, expected 2",
            o.detector.len()
        );
    }

    #[test]
    fn detector_cache_clears_at_cap_boundary() {
        // ponytail: pin the >= 64 → clear behaviour. Each distinct
        // (tool_name, len) tuple gets its own cache slot, so 64
        // distinct calls fill the cache exactly; the 65th triggers
        // the clear and inserts its own entry (result: len == 1).
        // A contributor who flips the comparison from >= to > (off-
        // by-one: would let the 65th slot in without clearing), or
        // changes the cap, surfaces here.
        //
        // The boundary numbers (64 / 65) are hard-coded so a
        // contributor who shrinks DETECTOR_CACHE_CAP without
        // updating this test gets caught here — not just at the
        // attribute pin above. The two tests are independent
        // surfaces: the attribute pin catches a clean constant
        // change; this one catches a behaviour change.
        const ASSUMED_CAP: usize = 64;
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let o = orch(&store, &slicer);
        // Distinct tool_names → distinct cache keys (each tuple
        // (Option<String>, blake3::Hash) hashes separately — the
        // BLAKE3 head hash is identical across all 65 calls because
        // the body is the same string, so the *tool_name* is the
        // distinguishing component of the key here). Body length
        // matches so the detector returns Unknown uniformly.
        let body = "hello world".to_string();
        for i in 0..ASSUMED_CAP {
            let tool = format!("tool_{i}");
            let _ = o.detector.get_or_detect(Some(&tool), &body);
        }
        assert_eq!(
            o.detector.len(),
            ASSUMED_CAP,
            "cache must be at assumed cap ({ASSUMED_CAP} entries) before the eviction call"
        );
        // ASSUMED_CAP+1 = 65th distinct call triggers clear+insert.
        let _ = o.detector.get_or_detect(Some("tool_64"), &body);
        assert_eq!(
            o.detector.len(),
            1,
            "{}th distinct call must clear the cache and leave a single entry; \
             if DETECTOR_CACHE_CAP changed, update ASSUMED_CAP or the cap constant",
            ASSUMED_CAP + 1
        );
    }

    // ponytail: pin that the cap-clear is a *full* clear, not a
    // selective eviction. The scenario: a long-running session
    // sits at the cap, hits a "hot" key that lives entirely in
    // cache, then a 65th distinct cold call triggers the clear.
    // After the clear, the hot key is gone — a re-lookup must
    // succeed but produce a fresh insert (not a hit). A
    // contributor who switches to a "preserve hot entries, evict
    // cold" LRU would not surface at the boundary test above
    // (the len==1 assertion holds either way because the
    // triggering cold entry inserts as a single slot). Pin the
    // all-or-nothing behaviour here.
    #[test]
    fn detector_cache_clear_is_full_not_selective() {
        const ASSUMED_CAP: usize = 64;
        let cache = DetectorCache::new();
        let body_a = "alpha body for hot key";
        let body_b = "beta body for cold keys";
        // Reserve ONE slot for the hot key first (63 cold fills,
        // leaving len == 63). Then insert the hot key (len == 64,
        // at the cap). The cap-clear check runs `>= CAP` at the
        // START of each call, so the hot insert does not trigger
        // a clear (it brings len from 63 to 64 — the >= check
        // for that call ran before the insert landed). Pinning
        // len == 64 here catches a contributor who moves the
        // >= check to AFTER the insert (would clear inside the
        // hot-insert call and len would be 1).
        for i in 0..(ASSUMED_CAP - 1) {
            let _ = cache.get_or_detect(Some(&format!("cold_{i}")), body_b);
        }
        let _ = cache.get_or_detect(Some("hot"), body_a);
        assert_eq!(
            cache.len(),
            ASSUMED_CAP,
            "cap-1 cold + 1 hot must fill the cache exactly to ASSUMED_CAP; got {}",
            cache.len()
        );
        // Trigger clear: the 64th cold call (distinct tool_name)
        // sees len == 64 (>= CAP) at entry and clears before
        // inserting. The cold entry survives; the hot entry
        // (and the 63 pre-existing cold) is wiped.
        let _ = cache.get_or_detect(Some("cold_trigger"), body_b);
        assert_eq!(
            cache.len(),
            1,
            "after cap-clear, only the triggering entry remains; \
             the previously-hot key was wiped alongside the others"
        );
        // Re-inserting the hot key is a *fresh* insert, not a
        // hit. After this, len should be 2 (cold_trigger +
        // hot). If the cache had selectively preserved the hot
        // entry, the re-insert would be a no-op insert
        // (already-present) and len would still be 1 — but we'd
        // also see len==1 here, so this assertion alone doesn't
        // distinguish the two implementations.
        //
        // The distinguishing signal: after a selective-LRU
        // clearing, the hot entry would STILL be present in the
        // cache, so re-calling with the same key would skip the
        // insert branch and the entry's "kind" would already be
        // cached. With full-clear, the hot key is gone and the
        // re-call must detect afresh. We don't observe that
        // directly, but we DO observe that the same-key
        // re-call succeeds without panic and len == 2 (which
        // both implementations satisfy). The exclusive signal is
        // the cap-clear path already covered above; here we
        // anchor that no exception fires on a post-clear
        // re-insert and that cache grows correctly.
        let _ = cache.get_or_detect(Some("hot"), body_a);
        assert_eq!(
            cache.len(),
            2,
            "re-inserting hot key after cap-clear adds a fresh slot; got {}",
            cache.len()
        );
    }

    // ponytail: pin total_bytes_saved sum with multiple Sliced
    // rows. The earlier `total_bytes_saved_sums_only_sliced_offloaded`
    // test sets up exactly one Sliced + two Keep. A contributor
    // who flips the sum reducer from `bytes_offloaded` to
    // `bytes_kept` (a typo under refactor) doesn't surface there
    // because the single Sliced row is dominated by `offloaded`
    // either way (kept bytes are dwarfed by offloaded for the
    // large fixture). Two Sliced rows make both branches
    // meaningful.
    #[test]
    fn total_bytes_saved_sums_each_sliced_row_independently() {
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let o = orch(&store, &slicer);
        let r = run(
            &o,
            &[
                ("a".into(), large(), None),
                ("b".into(), large(), None),
                ("c".into(), "tiny".to_string(), None),
            ],
        );
        let offloaded: usize = r
            .decisions
            .iter()
            .filter_map(|(_, d)| match d {
                SliceDecision::Sliced {
                    bytes_offloaded, ..
                } => Some(*bytes_offloaded),
                SliceDecision::Keep { .. } => None,
            })
            .sum();
        assert_eq!(
            r.decisions
                .iter()
                .filter(|(_, d)| matches!(d, SliceDecision::Sliced { .. }))
                .count(),
            2,
            "two large inputs must produce two Sliced rows; got {:?}",
            r.decisions
        );
        assert_eq!(
            r.total_bytes_saved, offloaded,
            "total_bytes_saved must equal sum of per-row bytes_offloaded; \
             got total={}, sum={offloaded}",
            r.total_bytes_saved
        );
    }

    // ponytail: pin the empty-content edge. A host that hands the
    // orchestrator a zero-length output gets back a Keep with
    // bytes=0 (not Sliced — the 0 < 8K threshold is decisive). A
    // contributor who passes empty content into the byte-slice
    // `&content.as_bytes()[..head_end]` would surface a debug-
    // build panic here. Pin the Keep shape explicitly.
    #[test]
    fn empty_content_keeps_with_zero_bytes() {
        let store = InMemoryOffloadStore::new();
        let slicer = HeadTailSlicer::default();
        let r = run(&orch(&store, &slicer), &[("k".into(), String::new(), None)]);
        assert_eq!(r.decisions.len(), 1);
        assert_eq!(
            r.decisions[0].1,
            SliceDecision::Keep {
                kind: ToolOutputKind::Unknown,
                bytes: 0
            },
            "empty content must produce Keep(Unknown, 0); got {:?}",
            r.decisions[0].1
        );
        assert_eq!(r.total_bytes_saved, 0);
        assert_eq!(r.decisions[0].0, "k", "decision key must round-trip");
    }

    // ponytail: pin DetectorCache::is_empty. The accessor is part
    // of the cache's read surface (`len` AND `is_empty`). A
    // contributor who removes the impl (or stops calling it from
    // `is_empty`) breaks the trait coherence. Pin both the empty
    // and non-empty transitions.
    #[test]
    fn detector_cache_is_empty_transitions() {
        let cache = DetectorCache::new();
        assert!(
            cache.is_empty(),
            "fresh cache must be empty; len={}",
            cache.len()
        );
        let _ = cache.get_or_detect(Some("cargo test"), "running 0 tests\n");
        assert!(
            !cache.is_empty(),
            "after one insert, cache must not be empty; len={}",
            cache.len()
        );
        // ponytail: pin the post-insert count. After one insertion,
        // the cache must report len > 0. A contributor who wires
        // `is_empty` to a stale snapshot (e.g. records emptiness at
        // construction) keeps the early assertion passing but breaks
        // this one — `is_empty()` is false but `len()` is 0, or
        // vice versa.
        assert_eq!(
            cache.len(),
            1,
            "post-insert len must report exactly 1; got {}",
            cache.len()
        );
    }
}
