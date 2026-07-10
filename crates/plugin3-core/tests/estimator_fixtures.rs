//! ADR-0005 § Negative — "the drift test pins the estimator's
//! output for a known corpus." A contributor who swaps the 3 vs 4
//! chars-per-token divisor surfaces here, not via a silently
//! inflated budget burn in production.
//!
//! ponytail: zero-deps loader — reads one TSV file at
//! `tests/fixtures/estimator.tsv`. Each non-comment line is
//! `<input>\t<expected_tokens>`. The corpus mixes prose, JSON,
//! and source so both branches of the heuristic (3 vs 4 chars
//! per token) are exercised.

use std::path::PathBuf;

use plugin3_core::estimate_tokens;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/estimator.tsv")
}

fn load_corpus() -> Vec<(String, usize)> {
    let path = fixture_path();
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        if raw.starts_with('#') || raw.is_empty() {
            continue;
        }
        let mut cols = raw.splitn(2, '\t');
        let input = cols.next().unwrap_or("").to_string();
        let expected: usize = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing expected value", path.display(), lineno + 1))
            .trim()
            .parse()
            .unwrap_or_else(|e| {
                panic!("{}:{}: bad expected value: {e}", path.display(), lineno + 1)
            });
        assert!(
            !input.is_empty(),
            "{}:{}: empty input",
            path.display(),
            lineno + 1
        );
        out.push((input, expected));
    }
    out
}

#[test]
fn estimator_matches_fixture_corpus() {
    let cases = load_corpus();
    assert!(!cases.is_empty(), "fixture corpus is empty — add cases");
    let mut mismatches = Vec::new();
    for (input, expected) in &cases {
        let got = estimate_tokens(input);
        if got != *expected {
            mismatches.push(format!(
                "expected={expected} got={got} bytes={} input={input:?}",
                input.len(),
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "estimator regressions ({} of {}):\n  {}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n  "),
    );
}

#[test]
fn fixture_file_is_present() {
    // ponytail: belt + suspenders — the loader above panics with
    // a path-named error if the file is missing, but a contributor
    // who refactors the path benefits from a dedicated test that
    // names the fixture directory.
    let path = fixture_path();
    assert!(
        path.is_file(),
        "missing fixture {} — the regression test corpus must live in tests/fixtures/",
        path.display(),
    );
}
