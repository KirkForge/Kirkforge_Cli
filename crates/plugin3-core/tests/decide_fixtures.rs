//! ADR-0005 § decide drift corpus — pins the four-way branch
//! (Allow / Warn / Slice / Compact) plus the boundary arithmetic
//! around `SLICE_OVERHEAD = 256`. A contributor who reorders the
//! Under/Approaching/Over check, swaps `saturating_sub` for `sub`,
//! changes the `needed + SLICE_OVERHEAD` comparison from strict
//! to non-strict, or shrinks the `256` constant surfaces here.
//!
//! ponytail: same shape as `compaction_fixtures.rs` and
//! `should_slice_fixtures.rs` — a tiny loader, one drift test,
//! plus a fixture-file presence assertion. Six columns:
//! `ceiling | approaching_ratio | used | incoming | recent | expected`.
//! `recent` is either `-` (empty list) or `key:bytes,key:bytes`.
//! `expected` is `allow` / `warn(N)` / `slice(key,N)` / `compact`.
//!
//! Boundary coverage: the `SLICE_OVERHEAD` strict-inequality boundary
//! (`largest == needed + 256` → Compact; `largest == needed + 257`
//! → Slice) is exercised twice; the Off-by-one on
//! `used + incoming <= ceiling` is exercised by `warn(0)` (used at
//! the ceiling, incoming=0).

use std::path::PathBuf;

use plugin3_core::budget::{decide, Intervention, TokenBudget};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/decide.tsv")
}

fn parse_recent(s: &str) -> Vec<(String, usize)> {
    let s = s.trim();
    if s == "-" || s.is_empty() {
        return Vec::new();
    }
    s.split(',')
        .map(|kv| {
            let (k, v) = kv
                .split_once(':')
                .unwrap_or_else(|| panic!("bad recent entry {kv:?}; expected key:bytes"));
            (k.trim().to_string(), v.trim().parse().expect("bytes usize"))
        })
        .collect()
}

/// Matches the `Intervention` shape so the loader can compare
/// against the live `decide(...)` output without each row having
/// to spell out the full struct.
#[derive(Debug, PartialEq)]
enum ExpectedIntervention {
    Allow,
    Warn { remaining: usize },
    Slice { target_key: String, slice_to: usize },
    Compact,
}

fn parse_expected(s: &str) -> ExpectedIntervention {
    let s = s.trim();
    if s == "allow" {
        return ExpectedIntervention::Allow;
    }
    if s == "compact" {
        return ExpectedIntervention::Compact;
    }
    if let Some(inner) = s.strip_prefix("warn(").and_then(|x| x.strip_suffix(')')) {
        return ExpectedIntervention::Warn {
            remaining: inner.trim().parse().expect("warn usize"),
        };
    }
    if let Some(inner) = s.strip_prefix("slice(").and_then(|x| x.strip_suffix(')')) {
        let (k, v) = inner
            .split_once(',')
            .unwrap_or_else(|| panic!("bad slice(...) in fixture: {s:?}"));
        return ExpectedIntervention::Slice {
            target_key: k.trim().to_string(),
            slice_to: v.trim().parse().expect("slice_to usize"),
        };
    }
    panic!("unrecognised expected intervention: {s:?}");
}

type DecideCase = (
    TokenBudget,
    usize,
    Vec<(String, usize)>,
    ExpectedIntervention,
);

fn load_corpus() -> Vec<DecideCase> {
    let path = fixture_path();
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        if raw.starts_with('#') || raw.is_empty() {
            continue;
        }
        let mut cols = raw.splitn(6, '\t');
        let ceiling: usize = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing ceiling", path.display(), lineno + 1))
            .trim()
            .parse()
            .expect("ceiling usize");
        let ratio: f64 = cols
            .next()
            .unwrap_or_else(|| {
                panic!(
                    "{}:{}: missing approaching_ratio",
                    path.display(),
                    lineno + 1
                )
            })
            .trim()
            .parse()
            .expect("ratio f64");
        let used: usize = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing used", path.display(), lineno + 1))
            .trim()
            .parse()
            .expect("used usize");
        let incoming: usize = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing incoming", path.display(), lineno + 1))
            .trim()
            .parse()
            .expect("incoming usize");
        let recent_s = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing recent", path.display(), lineno + 1));
        let expected_s = cols
            .next()
            .unwrap_or_else(|| panic!("{}:{}: missing expected", path.display(), lineno + 1));
        out.push((
            TokenBudget {
                ceiling,
                approaching_ratio: ratio,
                used,
            },
            incoming,
            parse_recent(recent_s),
            parse_expected(expected_s),
        ));
    }
    out
}

fn to_expected(i: &Intervention) -> ExpectedIntervention {
    match i {
        Intervention::Allow => ExpectedIntervention::Allow,
        Intervention::Warn { remaining } => ExpectedIntervention::Warn {
            remaining: *remaining,
        },
        Intervention::Slice {
            target_key,
            slice_to,
        } => ExpectedIntervention::Slice {
            target_key: target_key.clone(),
            slice_to: *slice_to,
        },
        Intervention::Compact { .. } => ExpectedIntervention::Compact,
    }
}

#[test]
fn decide_matches_branch_corpus() {
    let cases = load_corpus();
    assert!(!cases.is_empty(), "decide corpus empty — add cases");
    let mut mismatches = Vec::new();
    for (budget, incoming, recent, expected) in &cases {
        let got = decide(budget, *incoming, recent);
        let got_e = to_expected(&got);
        if got_e != *expected {
            mismatches.push(format!(
                "ceiling={} ratio={} used={} incoming={} recent={:?} expected={:?} got={got:?}",
                budget.ceiling, budget.approaching_ratio, budget.used, incoming, recent, expected,
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "decide regressions ({} of {}):\n  {}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n  "),
    );
}

#[test]
fn slice_overhead_constant_is_pinned() {
    // ponytail: SLICE_OVERHEAD = 256 is the load-bearing constant
    // that gates the Slice→Compact fallback. Pin it here so a
    // contributor who tunes it surfaces the change in this fixture
    // file (via boundary rows) AND in this attribute assertion.
    assert_eq!(plugin3_core::budget::SLICE_OVERHEAD, 256);
}

#[test]
fn fixture_file_is_present() {
    let path = fixture_path();
    assert!(
        path.is_file(),
        "missing fixture {} — the decide corpus must live in tests/fixtures/",
        path.display()
    );
}
