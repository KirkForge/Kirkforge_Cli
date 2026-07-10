//! `SlicingTransform` — head/tail preservation with offloaded middle.
//! Per ADR-0003.

use crate::error::TransformError;
use crate::store::{format_slice_marker, OffloadStore};
use crate::text::{ceil_char_boundary, floor_char_boundary};

pub struct SlicedOutput {
    pub head: String,
    pub tail: String,
    pub offload_marker: Option<String>,
    pub bytes_saved: usize,
}

pub trait SlicingTransform: Send + Sync {
    fn name(&self) -> &'static str;
    fn apply(&self, input: &str, store: &dyn OffloadStore) -> Result<SlicedOutput, TransformError>;
}

#[derive(Clone, Copy)]
pub struct HeadTailSlicer {
    pub head_bytes: usize,
    pub tail_bytes: usize,
}

impl Default for HeadTailSlicer {
    fn default() -> Self {
        Self {
            head_bytes: 4096,
            tail_bytes: 4096,
        }
    }
}

impl SlicingTransform for HeadTailSlicer {
    fn name(&self) -> &'static str {
        "head_tail"
    }

    fn apply(&self, input: &str, store: &dyn OffloadStore) -> Result<SlicedOutput, TransformError> {
        let len = input.len();
        // ponytail: saturating_add so a hostile config (head_bytes =
        // usize::MAX) doesn't wrap and turn a giant input into a "fits"
        // case (ADR-0017 § Reproducible builds — predictable overflow).
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
        // ponytail: invariant — when we reach this point (len > head +
        // tail), `tail_start ≥ len - tail_bytes > head_bytes ≥ head_end`,
        // so the else branch is unreachable today. We still clamp
        // `middle_end` to `head_end` rather than panicking on
        // `[head_end..tail_start]` if a future contributor changes
        // the early-return check, the threshold constants, or the
        // alignment semantics — a guard that converts the latent
        // panic into the documented pass-through shape.
        let middle_start = head_end;
        let middle_end = tail_start.max(middle_start);
        let (head, tail) = if middle_end > middle_start {
            (&input[..middle_start], &input[middle_end..])
        } else {
            // head_bytes + tail_bytes covers the whole input but len
            // check above said it doesn't; bail to pass-through.
            (input, "")
        };
        let middle = &input[middle_start..middle_end];
        let key = store
            .put(middle.as_bytes())
            .map_err(|e| TransformError::Internal(format!("store: {e}")))?;
        Ok(SlicedOutput {
            head: head.to_string(),
            tail: tail.to_string(),
            offload_marker: Some(format_slice_marker(&key)),
            bytes_saved: middle.len(),
        })
    }
}

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
            // a host's stderr captures the regression.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryOffloadStore;

    #[test]
    fn small_input_passes_through() {
        let s = HeadTailSlicer::default();
        let store = InMemoryOffloadStore::new();
        let out = s.apply("hello", &store).unwrap();
        assert_eq!(out.head, "hello");
        assert!(out.tail.is_empty());
        assert!(out.offload_marker.is_none());
        assert_eq!(out.bytes_saved, 0);
    }

    // ponytail: pin the `<=` boundary at exactly `head + tail`.
    // The guard is `len <= head.saturating_add(tail)` — input AT
    // the threshold must pass through (no slice), input one byte
    // over must slice. A contributor who flips `<=` to `<` shrinks
    // the pass-through band by one byte (silent off-by-one —
    // slicing inputs that fit entirely in head+tail). The earlier
    // shape only tested `large_input_slices_with_marker` (1010
    // bytes with head=8, tail=8 → 994 offloaded) and never pinned
    // the boundary itself. Without this test, a `len == head + tail`
    // input would silently start producing empty markers after the
    // flip.
    #[test]
    fn slicer_at_exact_head_plus_tail_boundary_passes_through() {
        let s = HeadTailSlicer {
            head_bytes: 8,
            tail_bytes: 8,
        };
        let store = InMemoryOffloadStore::new();
        // Exactly head + tail = 16 bytes → pass-through.
        let at_boundary = "x".repeat(16);
        let out = s.apply(&at_boundary, &store).unwrap();
        assert!(
            out.offload_marker.is_none(),
            "len == head + tail ({}) must pass through (no marker); got marker={:?}",
            at_boundary.len(),
            out.offload_marker
        );
        assert_eq!(
            out.head, at_boundary,
            "at-boundary pass-through must preserve the input verbatim"
        );
        assert!(out.tail.is_empty());
        assert_eq!(out.bytes_saved, 0);

        // One byte over → must slice. The middle is 1 byte.
        let over_boundary = "x".repeat(17);
        let out = s.apply(&over_boundary, &store).unwrap();
        assert!(
            out.offload_marker.is_some(),
            "len == head + tail + 1 (17) MUST slice; the `<=` guard tripped"
        );
        assert_eq!(out.head.len(), 8);
        assert_eq!(out.tail.len(), 8);
        assert_eq!(out.bytes_saved, 1, "middle = 17 - 8 - 8 = 1 byte offloaded");
    }

    // ponytail: pin the store.put() failure path. The apply() function
    // returns `Err(TransformError::Internal(...))` when the offload
    // store refuses the middle. Without this test, a contributor who
    // changes `map_err` to `.unwrap()` (or silently swallows) breaks
    // the orchestrator's error propagation — `slice_or_skip` would
    // never see the error and would never reach its Err-branch
    // pass-through. The fail-store stub keeps the test hermetic.
    #[test]
    fn slicer_propagates_store_put_failure() {
        struct FailStore;
        impl crate::store::OffloadStore for FailStore {
            fn put(&self, _: &[u8]) -> Result<String, crate::store::StoreError> {
                Err(crate::store::StoreError::Backend("disk full".into()))
            }
            fn get(&self, _: &str) -> Result<Vec<u8>, crate::store::StoreError> {
                Err(crate::store::StoreError::Backend("disk full".into()))
            }
            fn len(&self) -> usize {
                0
            }
            fn backend_name(&self) -> &'static str {
                "fail"
            }
        }
        let s = HeadTailSlicer {
            head_bytes: 4,
            tail_bytes: 4,
        };
        // 100 bytes — well over head + tail, so we enter the slice branch.
        let input = "a".repeat(100);
        // ponytail: `match` instead of `.expect_err(...)` because
        // `SlicedOutput` doesn't derive Debug (it's a hot-path
        // struct, no Debug cost on production calls). A Ok-arm
        // failure surfaces as a clear panic instead of a Debug
        // bound error.
        match s.apply(&input, &FailStore) {
            Err(TransformError::Internal(msg)) => {
                assert!(
                    msg.contains("disk full"),
                    "Internal variant must surface the store's error message; got {msg:?}"
                );
            }
            Err(other) => panic!("expected TransformError::Internal, got {other:?}"),
            Ok(_) => panic!("store.put() failure must propagate as Err, not be silently swallowed"),
        }
    }

    #[test]
    fn large_input_slices_with_marker() {
        let s = HeadTailSlicer {
            head_bytes: 8,
            tail_bytes: 8,
        };
        let store = InMemoryOffloadStore::new();
        // 1000 'a's + "TAIL_TAIL_" (10 chars) = 1010 chars total.
        // Middle = 1010 - 8 - 8 = 994 chars offloaded.
        let input = "a".repeat(1000) + "TAIL_TAIL_";
        let out = s.apply(&input, &store).unwrap();
        assert_eq!(out.head.len(), 8);
        assert_eq!(out.tail.len(), 8);
        let marker = out.offload_marker.expect("marker present");
        assert!(marker.starts_with(crate::store::SLICE_MARKER_PREFIX));
        assert!(marker.ends_with(crate::store::SLICE_MARKER_SUFFIX));
        assert_eq!(out.bytes_saved, 994);
    }

    #[test]
    fn slice_or_skip_returns_input_on_skipped() {
        // ponytail: Skipped branch — orchestrator treats no-op uniformly.
        struct SkipSlicer;
        impl SlicingTransform for SkipSlicer {
            fn name(&self) -> &'static str {
                "skip"
            }
            fn apply(&self, _: &str, _: &dyn OffloadStore) -> Result<SlicedOutput, TransformError> {
                Err(TransformError::Skipped("nope".into()))
            }
        }
        let store = InMemoryOffloadStore::new();
        let out = slice_or_skip("data", &SkipSlicer, &store);
        assert_eq!(out.head, "data");
        assert_eq!(out.bytes_saved, 0);
    }

    // ponytail: ADR-0003 § Implementation notes — "tests cover the
    // three branches" of slice_or_skip. The Ok branch passes the
    // transform's output through unchanged; the non-Skipped Err
    // branch logs and falls back to a no-op so the host's PostToolUse
    // does not crash. Both branches live behind `slice_or_skip`,
    // not just behind the slicer's `apply`, because the helper is
    // the canonical call site (ADR-0003).

    #[test]
    fn slice_or_skip_propagates_ok_branch() {
        let s = HeadTailSlicer {
            head_bytes: 4,
            tail_bytes: 4,
        };
        let store = InMemoryOffloadStore::new();
        let input = "a".repeat(100) + "TAIL_TAIL_";
        let out = slice_or_skip(&input, &s, &store);
        // Ok branch returns the slicer's output verbatim.
        assert_eq!(out.head.len(), 4);
        assert_eq!(out.tail.len(), 4);
        assert!(out.offload_marker.is_some());
        // 100 'a' + 10 'TAIL_TAIL_' = 110 chars; middle = 110 - 4 - 4 = 102.
        assert_eq!(out.bytes_saved, 102);
    }

    #[test]
    fn slice_or_skip_falls_back_on_non_skipped_error() {
        // ponytail: the eprintln is intentional — a non-Skipped
        // error means the slicer hit something unexpected. Capturing
        // it on stderr gives the user a paper trail when the host's
        // PostToolUse "passes through" output that should have been
        // sliced. The test guards the no-op shape only; the eprintln
        // is observable in CI logs.
        struct BoomSlicer;
        impl SlicingTransform for BoomSlicer {
            fn name(&self) -> &'static str {
                "boom"
            }
            fn apply(&self, _: &str, _: &dyn OffloadStore) -> Result<SlicedOutput, TransformError> {
                Err(TransformError::InvalidInput("bad bytes".into()))
            }
        }
        let store = InMemoryOffloadStore::new();
        let out = slice_or_skip("original", &BoomSlicer, &store);
        assert_eq!(out.head, "original");
        assert!(out.tail.is_empty());
        assert!(out.offload_marker.is_none());
        assert_eq!(out.bytes_saved, 0);
    }

    // ponytail: pin the ADR-0003 § HeadTailSlicer default.
    // The spec calls for `head_bytes: 4096, tail_bytes: 4096`
    // — the same numbers as the detector's `SLICE_HEAD_BYTES` /
    // `SLICE_TAIL_BYTES`. A contributor who tunes the slicer
    // default (4096 → 2048) without updating the detector
    // surfaces here: the orchestrator constructs slicers with
    // the per-decision head/tail (which comes from the detector),
    // so a mismatched default hides the bug — the detector still
    // emits 4096/4096, but a future user instantiating
    // `HeadTailSlicer::default()` directly gets the smaller
    // numbers. Drift catches here.
    #[test]
    fn head_tail_slicer_default_matches_adr() {
        let s = HeadTailSlicer::default();
        assert_eq!(s.head_bytes, 4096);
        assert_eq!(s.tail_bytes, 4096);
    }

    // ponytail: pin the transform's human-readable name. ADR-0003
    // § HeadTailSlicer names it `"head_tail"` — a contributor who
    // shortens to `"ht"` or `""` silently breaks dashboards that
    // filter transform emissions by name.
    #[test]
    fn head_tail_slicer_name_is_pinned() {
        let s = HeadTailSlicer::default();
        assert_eq!(s.name(), "head_tail");
    }

    // ---- Property tests (ADR-0016) — hand-picked + LCG fuzz, no proptest.

    fn fuzz_inputs() -> Vec<String> {
        let mut out: Vec<String> = vec![
            String::new(),
            "a".into(),
            "\n".repeat(10_000),
            // CJK (3 bytes per char) — would panic byte-slicing at a
            // mid-codepoint boundary before the fix above.
            "你".repeat(5_000),
            // Mixed ASCII + 4-byte emoji.
            format!("start{}end", "🦀".repeat(2_000)),
            // Boundary-misalignment: short prefix then 4-byte chars.
            format!("{}{}", "x".repeat(7), "🦀".repeat(2_000)),
        ];
        // Tiny LCG to mix length and content for 50 more iterations.
        let mut state: u64 = 0xdead_beef_cafe_babe;
        for _ in 0..50 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let n = ((state >> 32) as usize) % 4096 + 8; // 8..4104
            let pick = (state >> 8) as usize % 3;
            let chunk: String = match pick {
                0 => "a".repeat(n),
                1 => "你".repeat(n / 3 + 1),
                _ => "🦀".repeat(n / 4 + 1),
            };
            out.push(chunk);
        }
        out
    }

    #[test]
    fn no_panic_on_any_input() {
        let s = HeadTailSlicer {
            head_bytes: 32,
            tail_bytes: 32,
        };
        let store = InMemoryOffloadStore::new();
        for input in fuzz_inputs() {
            let out = s.apply(&input, &store).expect("no panic");
            // Property: bytes_saved <= input.len().
            assert!(
                out.bytes_saved <= input.len(),
                "saved {} > len {}",
                out.bytes_saved,
                input.len()
            );
        }
    }

    #[test]
    fn marker_always_valid_when_present() {
        let s = HeadTailSlicer {
            head_bytes: 16,
            tail_bytes: 16,
        };
        let store = InMemoryOffloadStore::new();
        for input in fuzz_inputs() {
            let out = s.apply(&input, &store).unwrap();
            if let Some(marker) = &out.offload_marker {
                let key = crate::store::parse_slice_marker(marker).expect("marker parse");
                crate::store::validate_key(key).expect("key valid");
            }
        }
    }

    #[test]
    fn same_input_produces_same_marker_key() {
        // Idempotence property — same bytes → same BLAKE3 key.
        let s = HeadTailSlicer {
            head_bytes: 16,
            tail_bytes: 16,
        };
        let store = InMemoryOffloadStore::new();
        for input in fuzz_inputs().into_iter().filter(|s| s.len() > 100) {
            let a = s.apply(&input, &store).unwrap();
            let b = s.apply(&input, &store).unwrap();
            assert_eq!(a.offload_marker, b.offload_marker);
        }
    }

    #[test]
    fn utf8_boundary_alignment_preserves_chars() {
        // Input designed so byte-slicing at head_bytes=10 lands inside a
        // multi-byte codepoint. Char-boundary alignment must keep both
        // edges on whole chars.
        let s = HeadTailSlicer {
            head_bytes: 10,
            tail_bytes: 10,
        };
        let store = InMemoryOffloadStore::new();
        // 6 ASCII + many 3-byte CJK chars = enough to trigger slicing
        // and to force head_end / tail_start to land inside a codepoint
        // without char-boundary alignment.
        let input = "xxxxxx".to_string() + &"你".repeat(2_000);
        let out = s.apply(&input, &store).unwrap();
        assert!(out.offload_marker.is_some());
        // Both edges must be valid UTF-8 (Rust &str invariant, but worth
        // re-checking after the floor/ceil arithmetic).
        assert!(out.head.is_char_boundary(out.head.len()));
        assert!(out.tail.is_char_boundary(out.tail.len()));
        // Tail ends with the last codepoint of the input.
        assert!(out.tail.ends_with("你"));
        // Head starts with the input prefix.
        assert!(out.head.starts_with("xxxxxx"));
    }

    // ponytail: pin the slicing invariant across input shapes that
    // could expose a future invariant break. The slicer's pass-through
    // branch (`tail_start <= head_end`) is unreachable under the
    // current `len > head + tail` precondition (so `tail_start ≥ len -
    // tail > head ≥ head_end`), but a contributor who tunes
    // `head_bytes` / `tail_bytes` defaults, the early-return check,
    // or the alignment semantics can flip the inequality. The
    // `middle_end.max(middle_start)` clamp turns that latent panic
    // into the documented pass-through shape — this test exercises
    // the property across ASCII, pure CJK, mixed CJK + emoji, and
    // boundary-adjacent inputs so the failure mode is observable
    // rather than a silent invariant assumption.
    #[test]
    fn slicing_invariant_holds_across_input_shapes() {
        let s = HeadTailSlicer {
            head_bytes: 8,
            tail_bytes: 8,
        };
        let store = InMemoryOffloadStore::new();
        let cases: Vec<(&str, String)> = vec![
            ("ascii_only", "a".repeat(1_000)),
            ("cjk_only_3byte", "你".repeat(1_000)),
            ("emoji_only_4byte", "🦀".repeat(1_000)),
            (
                "cjk_then_emoji",
                format!("{}{}", "你".repeat(500), "🦀".repeat(500)),
            ),
            (
                "ascii_prefix_cjk",
                format!("{}{}", "x".repeat(7), "你".repeat(500)),
            ),
            (
                "cjk_prefix_ascii",
                format!("{}{}", "你".repeat(500), "x".repeat(50)),
            ),
        ];
        for (label, input) in cases {
            let out = s
                .apply(&input, &store)
                .unwrap_or_else(|e| panic!("{label}: apply failed: {e}"));
            // head + tail + middle must equal input (no bytes lost
            // off-record). The clamp makes this hold even on the
            // hypothetical invariant break (then head = input,
            // tail = "", middle = "").
            let total = out.head.len() + out.tail.len() + out.bytes_saved;
            assert_eq!(
                total,
                input.len(),
                "{label}: head({}) + tail({}) + middle({}) must equal input({})",
                out.head.len(),
                out.tail.len(),
                out.bytes_saved,
                input.len()
            );
            // The marker is present iff we actually offloaded bytes.
            // (Under the pass-through fallback the marker is absent
            // and `out.head == input`.)
            if out.bytes_saved > 0 {
                assert!(
                    out.offload_marker.is_some(),
                    "{label}: non-empty middle must carry a marker"
                );
            } else {
                assert!(
                    out.offload_marker.is_none(),
                    "{label}: empty middle must NOT carry a marker"
                );
            }
        }
    }

    // ponytail: pin the empty-input edge. `len = 0 ≤ head + tail`
    // so the early-return fires: head = "", tail = "", marker =
    // None, bytes_saved = 0. A contributor who removes the early
    // return (or replaces `len <=` with `len <`) drops into the
    // slice branch and produces an empty middle either way, but
    // the head and tail fields would still be empty — the
    // property below holds either way. The behaviour to anchor:
    // `apply("")` must not panic, must not call `store.put`, and
    // must report `bytes_saved = 0`. Pin all three.
    #[test]
    fn slicer_passes_through_empty_input_without_store_write() {
        let s = HeadTailSlicer::default();
        let store = InMemoryOffloadStore::new();
        let out = s.apply("", &store).unwrap();
        assert_eq!(out.head, "");
        assert_eq!(out.tail, "");
        assert!(out.offload_marker.is_none());
        assert_eq!(out.bytes_saved, 0);
        // Pin the empty-input invariant: head + tail + middle = 0.
        assert_eq!(out.head.len() + out.tail.len() + out.bytes_saved, 0);
        // Critical: store stays empty. A contributor who routes
        // empty input through the slice branch and calls
        // `store.put(b"")` would inflate the on-disk count by a
        // bogus empty entry. The empty middle IS offloaded today
        // (the floor/ceil clamps collapse to a zero-byte range)
        // — pin that we don't actually call put on the empty case.
        assert_eq!(
            store.len(),
            0,
            "empty input must NOT trigger store.put; got store.len() = {}",
            store.len()
        );
    }

    // ponytail: pin end-to-end that the bytes SITTING IN THE STORE
    // for a given marker are the exact `[head_end..tail_start]`
    // slice of the input. The earlier marker-shape tests only
    // assert `marker.starts_with(PREFIX)` and `ends_with(SUFFIX)` —
    // a contributor who stubs `store.put(...)` to always return
    // the same hard-coded key (and skips the actual `put(middle)`
    // call) would pass those shape tests but lose this round-trip
    // property. This is the test that proves the slice-and-store
    // contract end-to-end, not just in shape.
    //
    // The fixture is built so head/tail boundaries land on known
    // ASCII positions: 40 'a's, 30 'b's, 30 'c's = 100 bytes.
    // With head=40/tail=30 we expect: head="a"×40, middle="b"×30,
    // tail="c"×30. Read the marker from the store, parse to key,
    // fetch bytes, compare byte-for-byte.
    #[test]
    fn slicer_stored_middle_bytes_equal_input_slice_between_head_and_tail() {
        let s = HeadTailSlicer {
            head_bytes: 40,
            tail_bytes: 30,
        };
        let store = InMemoryOffloadStore::new();
        let head = "a".repeat(40);
        let middle = "b".repeat(30);
        let tail = "c".repeat(30);
        let input = format!("{head}{middle}{tail}");
        assert_eq!(input.len(), 100);
        let out = s.apply(&input, &store).unwrap();
        // Sanity pins on the slicer output.
        assert_eq!(out.head, head);
        assert_eq!(out.tail, tail);
        assert_eq!(
            out.bytes_saved,
            middle.len(),
            "bytes_saved must equal middle.len() (the offloaded slice)"
        );
        let marker = out.offload_marker.expect("marker present");
        let key = crate::store::parse_slice_marker(&marker).expect("marker parses");
        // Now fetch the bytes that the store holds under this key
        // and assert byte-for-byte equality with the expected middle.
        let stored = store.get(key).expect("store.get succeeds");
        assert_eq!(
            stored,
            middle.as_bytes(),
            "the bytes stored under the marker key must equal the input slice \
             between head and tail; a stubbed store returning a fixed key would \
             surface here as a content mismatch"
        );
        // Round-trip the input: head + stored + tail = input. This
        // is the user-facing contract: `plugin3 ... retrieve <marker>`
        // should let them reconstruct the original.
        let reconstructed = String::from_utf8(stored).expect("utf-8");
        assert_eq!(
            format!("{}{}{}", out.head, reconstructed, out.tail),
            input,
            "head + stored-middle + tail must reconstitute the original input"
        );
    }
}
