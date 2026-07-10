//! UTF-8-safe byte-index helpers shared by detector and slicer.
//! `pub(crate)` because two callers exist (slicing.rs, detector.rs);
//! not `pub` because no caller outside the crate needs them.

/// Greatest index ≤ `at` that is a UTF-8 char boundary. Returns 0 when
/// `at == 0` or when no boundary exists at-or-before `at` in the prefix.
pub(crate) fn floor_char_boundary(s: &str, at: usize) -> usize {
    let mut i = at.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest index ≥ `at` that is a UTF-8 char boundary. Returns `s.len()`
/// when no such boundary exists at-or-after `at`.
pub(crate) fn ceil_char_boundary(s: &str, at: usize) -> usize {
    let mut i = at.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_returns_zero_at_zero() {
        assert_eq!(floor_char_boundary("hi", 0), 0);
    }

    #[test]
    fn floor_finds_boundary_inside_cjk_run() {
        // Each `你` is 3 bytes. Byte 2 lands mid-codepoint; the floor
        // must back up to byte 0.
        let s = "你好";
        assert_eq!(floor_char_boundary(s, 2), 0);
        // Asking for the boundary at byte 3 (start of second codepoint) works.
        assert_eq!(floor_char_boundary(s, 3), 3);
    }

    #[test]
    fn floor_clamps_to_len() {
        // `at` past end of string must clamp, not panic.
        let s = "abc";
        assert_eq!(floor_char_boundary(s, 999), 3);
    }

    #[test]
    fn ceil_returns_len_past_end() {
        let s = "abc";
        assert_eq!(ceil_char_boundary(s, 999), 3);
    }

    #[test]
    fn ceil_advances_past_partial_codepoint() {
        let s = "你好"; // 6 bytes total
                        // Byte 2 is mid-codepoint; ceil must advance to 3.
        assert_eq!(ceil_char_boundary(s, 2), 3);
    }

    // ponytail: pin the empty-string short-circuit. Both helpers
    // must return 0 on empty input regardless of `at` — a
    // contributor who removes the `min(s.len())` guard and walks
    // a non-existent byte string surfaces here as a panic.
    #[test]
    fn floor_and_ceil_on_empty_string_both_return_zero() {
        assert_eq!(floor_char_boundary("", 0), 0);
        assert_eq!(
            floor_char_boundary("", 5),
            0,
            "overshoot on empty must clamp to 0"
        );
        assert_eq!(ceil_char_boundary("", 0), 0);
        assert_eq!(
            ceil_char_boundary("", 5),
            0,
            "overshoot on empty must clamp to 0"
        );
    }

    // ponytail: pin `at == s.len()`. The detector calls
    // `floor_char_boundary(input, 1024.min(input.len()))` so the
    // at-equals-len case is exactly the path taken on inputs
    // < 1024 bytes. A contributor who flips the loop's `i > 0` to
    // `i >= 0` and the `i -= 1` to `i = i.saturating_sub(1)` (no
    // change), but who removes the `min(s.len())` clamp, would
    // surface here when the un-clamped `at > s.len()` walks past
    // the end. Pin the at-equals-len happy path explicitly.
    #[test]
    fn floor_at_exact_len_returns_len_for_ascii() {
        let s = "abc"; // 3 bytes, all single-byte chars
        assert_eq!(
            floor_char_boundary(s, 3),
            3,
            "asking for the boundary at s.len() must return s.len()"
        );
    }

    // ponytail: pin `ceil` at exact boundary (no-op) and at
    // boundary = s.len() (no overshoot). Distinct from the
    // overshoot test because the loop guard `i < s.len()` means
    // asking for `ceil(s, s.len())` returns `s.len()` without
    // entering the loop — different code path from overshoot.
    #[test]
    fn ceil_at_exact_boundary_is_no_op() {
        let s = "你好"; // bytes 0,3 are boundaries; 6 is end
        assert_eq!(
            ceil_char_boundary(s, 0),
            0,
            "asking for boundary at byte 0 must return 0 (no advance)"
        );
        assert_eq!(
            ceil_char_boundary(s, 3),
            3,
            "asking for boundary at a valid position must return that position"
        );
        assert_eq!(
            ceil_char_boundary(s, 6),
            6,
            "asking for boundary at s.len() must return s.len() (loop guard prevents overshoot)"
        );
    }

    // ponytail: pin both helpers across UTF-8 byte-width diversity.
    // The existing CJK test only covers 3-byte codepoints — a
    // contributor who narrows `floor_char_boundary` to a "skip 3
    // bytes back" hack (treating CJK as the only multibyte case)
    // would silently corrupt 2-byte (Latin-1 diacritics) and 4-byte
    // (emoji, supplementary plane) inputs. Pin each width.
    #[test]
    fn floor_and_ceil_track_two_byte_diacritic_boundary() {
        // 'é' is 2 bytes (0xC3 0xA9); "café" = bytes [c,a,f,0xC3,0xA9].
        let s = "café";
        assert_eq!(s.len(), 5, "fixture sanity: 'café' is 5 bytes");
        // Mid-codepoint at byte 4 lands inside 'é' (bytes 3,4).
        assert_eq!(
            floor_char_boundary(s, 4),
            3,
            "floor must back up to byte 3 (start of 'é'), not byte 4"
        );
        assert_eq!(
            ceil_char_boundary(s, 4),
            5,
            "ceil must advance to byte 5 (past 'é')"
        );
        // Exactly at the boundary: both are no-ops.
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(ceil_char_boundary(s, 3), 3);
    }

    #[test]
    fn floor_and_ceil_track_four_byte_emoji_boundary() {
        // '🦀' is 4 bytes (UTF-8 0xF0 0x9F 0xA6 0x80). "a🦀b".
        let s = "a🦀b";
        assert_eq!(s.len(), 6, "fixture sanity: 'a🦀b' is 6 bytes");
        // Bytes 2..=4 are inside the 🦀 codepoint (byte 1 IS a
        // char boundary — start of the 4-byte emoji — so it is
        // not "mid"). Only bytes >1 and <5 are interior.
        for mid in 2..=4 {
            assert_eq!(
                floor_char_boundary(s, mid),
                1,
                "floor at mid-emoji byte {mid} must back up to byte 1; \
                 a 3-byte-skip hack would land at byte {}-3={}, surfacing here",
                mid,
                mid.saturating_sub(3)
            );
            assert_eq!(
                ceil_char_boundary(s, mid),
                5,
                "ceil at mid-emoji byte {mid} must advance to byte 5 (past '🦀')"
            );
        }
        // Exactly at start of emoji (byte 1) or after it (byte 5):
        // both helpers are no-ops.
        assert_eq!(floor_char_boundary(s, 1), 1);
        assert_eq!(ceil_char_boundary(s, 1), 1);
        assert_eq!(floor_char_boundary(s, 5), 5);
        assert_eq!(ceil_char_boundary(s, 5), 5);
    }

    // ponytail: pin the floor/ceil *result* property for in-range
    // `at`. When `at <= s.len()`, the helpers' `at.min(s.len())`
    // clamp is a no-op and the invariants hold: result is a char
    // boundary, floor ≤ at ≤ ceil. (Out-of-range `at` collapses
    // to `s.len()` via the clamp, so `ceil(s, s.len()+k) == s.len()`
    // violates `c ≥ at` — pinned separately by `ceil_returns_len_past_end`.)
    // A contributor who returns an interior byte for `floor`
    // (e.g. changes the loop to skip forward instead of back)
    // passes `floor_finds_boundary_inside_cjk_run` for the trivial
    // 3-byte case but breaks here for runs of multiple CJK chars.
    #[test]
    fn floor_and_ceil_results_are_char_boundaries_in_range() {
        let cases: &[&str] = &[
            "ascii only",
            "你好世界", // multiple 3-byte codepoints
            "café",     // 2-byte
            "a🦀b🦀c",  // 4-byte repeated
            "mix 你好 café 🦀",
        ];
        for s in cases {
            for &at in &[0_usize, 1, s.len() / 2, s.len()] {
                let f = floor_char_boundary(s, at);
                let c = ceil_char_boundary(s, at);
                assert!(
                    f <= s.len(),
                    "floor({:?}, {at}) = {f} exceeds s.len()={}",
                    s,
                    s.len()
                );
                assert!(
                    c <= s.len(),
                    "ceil({:?}, {at}) = {c} exceeds s.len()={}",
                    s,
                    s.len()
                );
                assert!(
                    s.is_char_boundary(f),
                    "floor({s:?}, {at}) = {f} must be a char boundary"
                );
                assert!(
                    s.is_char_boundary(c),
                    "ceil({s:?}, {at}) = {c} must be a char boundary"
                );
                assert!(f <= at, "floor({s:?}, {at}) = {f} must be ≤ at");
                assert!(c >= at, "ceil({s:?}, {at}) = {c} must be ≥ at");
            }
        }
    }
}
