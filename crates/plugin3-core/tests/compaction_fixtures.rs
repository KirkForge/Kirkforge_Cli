//! ADR-0008 § Implementation notes — the drift test pins
//! `local_summarise` output for a known corpus so a contributor
//! who tweaks the heuristic (line filter, `max_bytes` truncation,
//! long-line skip threshold) fails CI for review.
//!
//! ponytail: same shape as `estimator_fixtures.rs` and
//! `store_drift.rs` — hex-encoded TSV, a tiny loader, and one
//! drift test. Adding a fourth file would not gain readability;
//! keeping the loader local to this test file means the fixture
//! is self-describing.

use std::path::PathBuf;

use plugin3_core::compaction::local_summarise;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local_summarise.tsv")
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let pair = std::str::from_utf8(&bytes[i..i + 2])
            .unwrap_or_else(|e| panic!("non-utf8 hex pair at byte {i}: {e}"));
        out.push(
            u8::from_str_radix(pair, 16)
                .unwrap_or_else(|e| panic!("bad hex at byte {i}: {pair:?}: {e}")),
        );
        i += 2;
    }
    out
}

fn load_corpus() -> Vec<(Vec<u8>, usize, Vec<u8>)> {
    let path = fixture_path();
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        if raw.starts_with('#') || raw.is_empty() {
            continue;
        }
        let mut cols = raw.split('\t');
        let input_hex = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing input", path.display(), lineno + 1));
        let max_bytes: usize = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing max_bytes", path.display(), lineno + 1))
            .parse()
            .unwrap_or_else(|e| panic!("{}:{}: bad max_bytes: {e}", path.display(), lineno + 1));
        let expected_hex = cols.next().unwrap_or_else(|| {
            panic!("{}:{}: missing expected output", path.display(), lineno + 1)
        });
        assert!(
            cols.next().is_none(),
            "{}:{}: too many columns",
            path.display(),
            lineno + 1
        );
        let input = hex_to_bytes(input_hex);
        let expected = hex_to_bytes(expected_hex);
        // ponytail: empty input row is two columns (no expected output).
        // A row with an expected output is three columns. The skip
        // for empty input_hex still needs max_bytes for the truncation
        // boundary test, but the expected output is the empty string
        // which hex_to_bytes handles as `""` -> Vec::new().
        out.push((input, max_bytes, expected));
    }
    out
}

#[test]
fn local_summarise_matches_fixture_corpus() {
    let cases = load_corpus();
    assert!(!cases.is_empty(), "fixture corpus is empty — add cases");
    let mut mismatches = Vec::new();
    for (input, max_bytes, expected) in &cases {
        // local_summarise takes &str; the fixture is binary so we
        // round-trip through lossy UTF-8. The corpus only contains
        // ASCII (hex 0x00-0x7F) so lossy is safe — every byte maps
        // back exactly.
        let s = std::str::from_utf8(input)
            .unwrap_or_else(|e| panic!("fixture row has non-UTF8 input: {e}"));
        let got = local_summarise(s, *max_bytes);
        let got_bytes = got.as_bytes();
        if got_bytes != expected.as_slice() {
            mismatches.push(format!(
                "max_bytes={max_bytes} input={s:?}\n  expected={}\n  got={}",
                expected_hex(expected),
                expected_hex(got_bytes),
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "local_summarise regressions ({} of {}):\n  {}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n  "),
    );
}

fn expected_hex(b: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

#[test]
fn fixture_file_is_present() {
    let path = fixture_path();
    assert!(
        path.is_file(),
        "missing fixture {} — the regression test corpus must live in tests/fixtures/",
        path.display()
    );
}
