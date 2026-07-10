//! ADR-0016 § Drift tests #1 (`OffloadStore` drift) — the load-bearing
//! drift test per the ADR. Pins `plugin3_core::store::make_key`
//! against a fixture of canonical BLAKE3 outputs so a contributor
//! who swaps the hash function or changes the truncation length
//! fails CI. ADR-0004 § Key format calls `make_key` "byte-compatible
//! with Stratum's `make_offload_key`"; this fixture IS that contract.
//!
//! ponytail: zero-deps loader — reads one TSV file at
//! `tests/fixtures/store_keys.tsv`. Each non-comment line is
//! `<hex_input>\t<expected_key>`. The input is hex-encoded so the
//! fixture is text-safe (binary strings in TSV need escaping; hex
//! doesn't). The empty-input row pins the canonical BLAKE3 spec
//! test vector.

use std::path::PathBuf;

use plugin3_core::store::{make_key, validate_key};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/store_keys.tsv")
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    if s.is_empty() {
        return Vec::new();
    }
    assert!(s.len() % 2 == 0, "odd hex length: {s:?}");
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

fn load_corpus() -> Vec<(Vec<u8>, String)> {
    let path = fixture_path();
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        if raw.starts_with('#') || raw.is_empty() {
            continue;
        }
        let mut cols = raw.splitn(2, '\t');
        let hex = cols.next().unwrap_or("").to_string();
        let expected: String = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing expected key", path.display(), lineno + 1))
            .trim()
            .to_string();
        assert_eq!(
            expected.len(),
            24,
            "{}:{}: expected key must be 24 hex chars, got {expected:?}",
            path.display(),
            lineno + 1
        );
        out.push((hex_to_bytes(&hex), expected));
    }
    out
}

#[test]
fn store_drift_matches_fixture_corpus() {
    let cases = load_corpus();
    assert!(!cases.is_empty(), "fixture corpus is empty — add cases");
    let mut mismatches = Vec::new();
    for (bytes, expected) in &cases {
        let got = make_key(bytes);
        if got != *expected {
            mismatches.push(format!(
                "expected={expected} got={got} bytes_len={}",
                bytes.len(),
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "OffloadStore drift regressions ({} of {}):\n  {}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n  "),
    );
}

#[test]
fn fixture_keys_are_valid_hex() {
    // ponytail: belt + suspenders — the drift test above assumes
    // the fixture's expected keys are well-formed. If a contributor
    // fat-fingers a hex digit in the fixture (e.g. types 'g'), the
    // drift test fails with a confusing "expected X got Y" diff
    // rather than a clear "fixture is invalid". This test catches
    // the fixture typo at source.
    for (bytes, expected) in load_corpus() {
        validate_key(&expected).unwrap_or_else(|e| {
            panic!(
                "fixture key {expected:?} failed validate_key: {e} (bytes_len={})",
                bytes.len()
            )
        });
    }
}

#[test]
fn empty_input_pins_canonical_blake3_vector() {
    // ponytail: regression guard for the BLAKE3 spec test vector.
    // The empty-input hash af1349b9f5f9a1a6a0404dea is the
    // canonical reference; anyone who accidentally swaps blake3
    // for sha256 gets d8a... instead and the drift test above
    // fails with a noisy diff. This test pins the *single most
    // important* row so the failure mode is obvious.
    assert_eq!(make_key(b""), "af1349b9f5f9a1a6a0404dea");
}

#[test]
fn fixture_file_is_present() {
    let path = fixture_path();
    assert!(
        path.is_file(),
        "missing fixture {} — the drift corpus must live in tests/fixtures/",
        path.display(),
    );
}
