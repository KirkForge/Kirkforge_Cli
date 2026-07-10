//! ADR-0006 § `should_slice` drift corpus — pins the per-kind
//! slicing threshold table. A contributor who bumps
//! `TestRunner` from 8 KiB to 16 KiB (or splits `BuildLog` from
//! `Json`) surfaces here for review instead of silently changing
//! the slice behaviour.
//!
//! ponytail: same shape as `compaction_fixtures.rs` and
//! `detector_fixtures.rs` — a tiny loader, one drift test, plus a
//! fixture-file presence assertion. Three columns:
//! `<kind>\t<bytes>\t<expected>`. The expected column is
//! `keep` or `slice(<head>,<tail>)` so the spec'd `4096/4096`
//! defaults are also pinned (a contributor who changes the
//! default head/tail bytes breaks CI for every kind's slice row).
//!
//! The corpus exercises each kind's threshold boundary twice:
//! once just below (keep) and once at-or-above (slice). The
//! exception is `FileContent`, whose threshold is `usize::MAX` —
//! we assert 1 MiB still returns keep, matching
//! `file_content_never_sliced` in `detector.rs`.

use std::path::PathBuf;

use plugin3_core::detector::{should_slice, Decision, ToolOutputKind};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/should_slice.tsv")
}

fn parse_kind(s: &str) -> ToolOutputKind {
    match s {
        "TestRunner" => ToolOutputKind::TestRunner,
        "Compiler" => ToolOutputKind::Compiler,
        "BuildLog" => ToolOutputKind::BuildLog,
        "GenericShell" => ToolOutputKind::GenericShell,
        "SearchResults" => ToolOutputKind::SearchResults,
        "FileContent" => ToolOutputKind::FileContent,
        "Json" => ToolOutputKind::Json,
        "Unknown" => ToolOutputKind::Unknown,
        other => panic!("fixture uses unknown kind {other:?}; add it to the parser"),
    }
}

/// `keep` or `slice(<head>,<tail>)` per fixture column 3.
fn parse_expected(s: &str) -> ExpectedDecision {
    let s = s.trim();
    if s == "keep" {
        return ExpectedDecision::Keep;
    }
    if let Some(inner) = s.strip_prefix("slice(").and_then(|x| x.strip_suffix(')')) {
        let (h, t) = inner
            .split_once(',')
            .unwrap_or_else(|| panic!("bad slice(...) in fixture: {s:?}"));
        return ExpectedDecision::Slice {
            keep_head: h.trim().parse().expect("head usize"),
            keep_tail: t.trim().parse().expect("tail usize"),
        };
    }
    panic!("unrecognised expected decision: {s:?}");
}

#[derive(Debug, PartialEq)]
enum ExpectedDecision {
    Keep,
    Slice { keep_head: usize, keep_tail: usize },
}

fn load_corpus() -> Vec<(ToolOutputKind, usize, ExpectedDecision)> {
    let path = fixture_path();
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        if raw.starts_with('#') || raw.is_empty() {
            continue;
        }
        let mut cols = raw.splitn(3, '\t');
        let kind_s = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing kind", path.display(), lineno + 1));
        let bytes_s = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing bytes", path.display(), lineno + 1));
        let expected_s = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing expected", path.display(), lineno + 1));
        out.push((
            parse_kind(kind_s),
            bytes_s.trim().parse().unwrap_or_else(|e| {
                panic!(
                    "{}:{}: bad bytes {bytes_s:?}: {e}",
                    path.display(),
                    lineno + 1
                )
            }),
            parse_expected(expected_s),
        ));
    }
    out
}

#[test]
fn should_slice_matches_threshold_corpus() {
    let cases = load_corpus();
    assert!(!cases.is_empty(), "threshold corpus empty — add cases");
    let mut mismatches = Vec::new();
    for (kind, bytes, expected) in &cases {
        let got = should_slice(*kind, *bytes);
        let got_e = match got {
            Decision::Keep => ExpectedDecision::Keep,
            Decision::Slice {
                keep_head,
                keep_tail,
            } => ExpectedDecision::Slice {
                keep_head,
                keep_tail,
            },
        };
        if got_e != *expected {
            mismatches.push(format!(
                "kind={kind:?} bytes={bytes} expected={expected:?} got={got_e:?}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "should_slice regressions ({} of {}):\n  {}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n  "),
    );
}

#[test]
fn fixture_file_is_present() {
    let path = fixture_path();
    assert!(
        path.is_file(),
        "missing fixture {} — the threshold corpus must live in tests/fixtures/",
        path.display()
    );
}
