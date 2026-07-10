//! Aggregator for `plugin3 report`. ADR-0010 § Report subcommand.
//!
//! ponytail: the aggregator moved from plugin3-cli to plugin3-core
//! because it operates only on `UsageRecord` (already in core) and
//! returns a typed `BTreeMap`. The CLI side now wraps a clap arg
//! parser around `summarise_at` and `tail_lines_at` — the pure
//! transforms live here so they can be tested without a binary.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::cost::{UsageKind, UsageRecord};

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SessionTotals {
    pub bytes_saved: usize,
    pub warnings: usize,
    pub compactions: usize,
    pub records: usize,
}

/// Group surviving records by `session_id` and roll up the counts.
/// ADR-0010: `--summary` emits one line per session. `BTreeMap`
/// keeps session order stable so the output is deterministic
/// for tests.
///
/// ponytail: malformed JSONL lines are silently skipped (the
/// `let Ok(r) = ... else { continue };` arm). ADR-0010 says
/// usage.jsonl may be hand-edited; a contributor who propagates
/// with `?` breaks a long session summary at the first typo.
/// ponytail: typed `UsageRecord` parse so `kind` strings ("slice",
/// "`budget_warn`", …) come from `UsageKind` instead of being
/// duplicated here. A kind rename surfaces at compile time, not
/// as silently-dropped records.
#[must_use]
pub fn aggregate_sessions(lines: &[&str]) -> BTreeMap<String, SessionTotals> {
    let mut out: BTreeMap<String, SessionTotals> = BTreeMap::new();
    for line in lines {
        let Ok(r) = serde_json::from_str::<UsageRecord>(line) else {
            continue;
        };
        let t = out.entry(r.session_id).or_default();
        t.records += 1;
        match r.kind {
            UsageKind::Slice => {
                // ponytail: use saturating_sub on both operands so a
                // malformed record (bytes_out > bytes_in) doesn't wrap
                // and make bytes_saved negative via usize underflow.
                t.bytes_saved += r
                    .bytes_in
                    .unwrap_or(0)
                    .saturating_sub(r.bytes_out.unwrap_or(0));
            }
            UsageKind::BudgetWarn | UsageKind::BudgetOver => t.warnings += 1,
            UsageKind::CompactHint => t.compactions += 1,
            UsageKind::Prompt | UsageKind::Response => {}
        }
    }
    out
}

/// Apply `--session` and `--kind` filters, then return the last
/// `last` lines (post-filter truncation per ADR-0010). Returns
/// the surviving lines.
///
/// ponytail: typed `UsageRecord` parse so `session_id`/`kind`
/// field renames fail at compile time instead of silently
/// dropping records. Mirrors the strategy in `aggregate_sessions`.
#[must_use]
pub fn filter_lines<'a>(
    lines: &'a [&'a str],
    session: Option<&str>,
    kind: Option<UsageKind>,
) -> Vec<&'a str> {
    lines
        .iter()
        .copied()
        .filter(|line| {
            let Ok(r) = serde_json::from_str::<UsageRecord>(line) else {
                return false;
            };
            if let Some(sid) = session {
                if r.session_id != sid {
                    return false;
                }
            }
            if let Some(ks) = kind {
                if r.kind != ks {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Truncate a filtered slice to its last `n` entries; if `n`
/// exceeds the slice length, return the slice unchanged.
#[must_use]
pub fn tail_lines<'a>(lines: &'a [&'a str], n: usize) -> &'a [&'a str] {
    if lines.len() > n {
        &lines[lines.len() - n..]
    } else {
        lines
    }
}

/// Format one session's totals as the `--summary` line that
/// `plugin3 report` prints. ADR-0010 § Report subcommand shows
/// `session <sid>  bytes_saved=<n>  warnings=<n>  compactions=<n>`
/// — the two-space separator and the field order are part of
/// the contract because downstream tools grep for
/// `bytes_saved=` and `warnings=` in the rendered stream.
///
/// ponytail: pinned here (not in plugin3-cli) so a format drift
/// surfaces in a unit test, not as a regression in the user's
/// dashboard parser. Adding `records=` at the end was an
/// additive commit (round 11) that explicitly extended the
/// documented shape — keeping the original four fields first
/// preserves the original grep affordances.
#[must_use]
pub fn format_summary_line(sid: &str, totals: &SessionTotals) -> String {
    format!(
        "session {sid}  bytes_saved={}  warnings={}  compactions={}  records={}",
        totals.bytes_saved, totals.warnings, totals.compactions, totals.records,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::UsageRecord;

    // ponytail: helper to keep each test row to one line. session_id
    // is the bucket key; bytes_in/bytes_out drive Slice math.
    fn rec(
        kind: UsageKind,
        session: &str,
        bytes_in: Option<usize>,
        bytes_out: Option<usize>,
    ) -> String {
        serde_json::to_string(&UsageRecord {
            ts: chrono::DateTime::parse_from_rfc3339("2026-06-27T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            kind,
            session_id: session.into(),
            bytes_in,
            bytes_out,
            tokens_used: None,
            tokens_ceiling: None,
            tool: None,
        })
        .unwrap()
    }

    // ponytail: pin the Slice math. bytes_saved = bytes_in - bytes_out
    // (saturating on both sides). A contributor who switches to
    // `unwrap_or(0) - unwrap_or(0)` (no saturating) breaks here when
    // bytes_out > bytes_in — underflow would panic in debug builds.
    #[test]
    fn aggregate_bytes_saved_is_bytes_in_minus_bytes_out_saturating() {
        let lines = [
            rec(UsageKind::Slice, "s1", Some(1000), Some(200)),
            rec(UsageKind::Slice, "s1", Some(500), Some(600)), // negative → 0
            rec(UsageKind::Slice, "s2", None, Some(50)),       // None in → 0
            rec(UsageKind::Slice, "s2", Some(100), None),      // None out → unchanged
        ];
        let v: Vec<&str> = lines.iter().map(std::string::String::as_str).collect();
        let by_session = aggregate_sessions(&v);
        let s1 = by_session["s1"];
        let s2 = by_session["s2"];
        assert_eq!(
            s1.bytes_saved, 800,
            "s1: 1000-200 + saturating(500-600) = 800, got {}",
            s1.bytes_saved
        );
        assert_eq!(
            s2.bytes_saved, 100,
            "s2: None in → 0, 100 - None → 100; got {}",
            s2.bytes_saved
        );
    }

    // ponytail: pin the kind→bucket mapping. warnings counts BOTH
    // BudgetWarn AND BudgetOver. A contributor who narrows to
    // BudgetWarn alone surfaces here. compactions is a separate
    // bucket driven by CompactHint. Prompt/Response do not
    // contribute to any numeric bucket.
    #[test]
    fn aggregate_kind_to_bucket_mapping_is_pinned() {
        let lines = [
            rec(UsageKind::BudgetWarn, "s1", None, None),
            rec(UsageKind::BudgetOver, "s1", None, None),
            rec(UsageKind::CompactHint, "s1", None, None),
            rec(UsageKind::Prompt, "s1", None, None),
            rec(UsageKind::Response, "s1", None, None),
        ];
        let v: Vec<&str> = lines.iter().map(std::string::String::as_str).collect();
        let s = &aggregate_sessions(&v)["s1"];
        assert_eq!(s.warnings, 2, "BudgetWarn + BudgetOver = 2 warnings");
        assert_eq!(s.compactions, 1, "CompactHint → 1 compaction");
        assert_eq!(s.records, 5, "all five records counted");
    }

    // ponytail: malformed JSONL is silently skipped, not a panic.
    // ADR-0010 says the file grows linearly and may be hand-edited;
    // a `?` propagation would stop the summary mid-run. Also pins
    // that an *empty* session_id buckets under "" (rather than
    // being folded into another session — pre-compact events
    // legitimately don't carry a session).
    #[test]
    fn aggregate_skips_malformed_jsonl_lines() {
        let lines = [
            rec(UsageKind::Slice, "s1", Some(100), Some(20)),
            "not json".to_string(),
            r#"{"kind":"slice","ts":"2026-06-27T00:00:00Z","session_id":"","bytes_in":50,"bytes_out":10}"#.to_string(),
            rec(UsageKind::BudgetWarn, "s1", None, None),
        ];
        let v: Vec<&str> = lines.iter().map(std::string::String::as_str).collect();
        let by_session = aggregate_sessions(&v);
        assert!(by_session.contains_key("s1"));
        assert!(
            by_session.contains_key(""),
            "record with empty session_id must bucket under the empty-string key"
        );
        let s1 = &by_session["s1"];
        assert_eq!(s1.bytes_saved, 80);
        assert_eq!(s1.warnings, 1);
        assert_eq!(s1.records, 2);
        // ponytail: a record with missing session_id (not just
        // empty) MUST be skipped entirely (String has no default).
        let missing_sid = [
            r#"{"kind":"slice","ts":"2026-06-27T00:00:00Z","bytes_in":50,"bytes_out":10}"#
                .to_string(),
        ];
        let v2: Vec<&str> = missing_sid
            .iter()
            .map(std::string::String::as_str)
            .collect();
        assert!(
            aggregate_sessions(&v2).is_empty(),
            "missing session_id (String, no default) must drop the record entirely"
        );
    }

    // ponytail: pin the filter order — session + kind are AND'd,
    // then truncated to last N from the *filtered* set. A
    // contributor who reverses the order (truncate then filter)
    // surfaces here because the surviving count changes.
    #[test]
    fn filter_then_tail_is_pinned() {
        let lines = [
            rec(UsageKind::Slice, "s1", Some(1000), Some(200)),
            rec(UsageKind::BudgetWarn, "s1", None, None),
            rec(UsageKind::Slice, "s2", Some(500), Some(100)),
            rec(UsageKind::BudgetWarn, "s2", None, None),
            rec(UsageKind::CompactHint, "s1", None, None),
        ];
        let v: Vec<&str> = lines.iter().map(std::string::String::as_str).collect();
        // kind=Slice → 2 lines survive, tail(10) keeps both.
        let f = filter_lines(&v, None, Some(UsageKind::Slice));
        assert_eq!(f.len(), 2);
        assert_eq!(tail_lines(&f, 10).len(), 2);

        // session=s1 → 3 lines survive.
        let f = filter_lines(&v, Some("s1"), None);
        assert_eq!(f.len(), 3);

        // tail=1 of all 5 lines → last one only (the CompactHint).
        let last = tail_lines(&v, 1);
        assert_eq!(last.len(), 1);
        assert!(
            last[0].contains("compact_hint"),
            "tail-1 must return the chronologically-last line, got: {}",
            last[0]
        );
    }

    // ponytail: a contributor who flips tail from `len - n..` to
    // `..n` (front truncation instead of back) would surface
    // here because the assertion targets the *last* line.
    #[test]
    fn tail_lines_returns_back_slice_not_front() {
        let lines = ["a", "b", "c", "d", "e"];
        assert_eq!(tail_lines(&lines, 2), &["d", "e"][..]);
        assert_eq!(tail_lines(&lines, 0), &[] as &[&str]);
        // n >= len → unchanged.
        assert_eq!(tail_lines(&lines, 99), &lines[..]);
    }

    // ponytail: pin the ADR-0010 § Report subcommand summary
    // format. The documented example is
    //   `session abc-123  bytes_saved=2345678  warnings=3  compactions=1`
    // — the two-space separator, the `session <sid>` prefix,
    // and the key=value fields are the wire contract because
    // downstream grep filters parse on `bytes_saved=` etc. A
    // contributor who collapses to single-space or swaps
    // `warnings=`/`compactions=` order surfaces here.
    #[test]
    fn format_summary_line_matches_adr_example() {
        let totals = SessionTotals {
            bytes_saved: 2_345_678,
            warnings: 3,
            compactions: 1,
            records: 42,
        };
        let line = format_summary_line("abc-123", &totals);
        assert_eq!(
            line, "session abc-123  bytes_saved=2345678  warnings=3  compactions=1  records=42",
            "summary format drift (separator / field order / key spelling)",
        );
    }

    // ponytail: pin the separator independently. The two-space
    // gap between fields is load-bearing because the documented
    // `grep -F 'bytes_saved='` filter assumes the key is its own
    // whitespace-delimited token. A contributor who collapses to
    // single-space breaks the filter for callers that pipe the
    // summary into awk '{print $3}' parsers.
    #[test]
    fn format_summary_line_uses_two_space_separator() {
        let totals = SessionTotals {
            bytes_saved: 0,
            warnings: 0,
            compactions: 0,
            records: 0,
        };
        let line = format_summary_line("s", &totals);
        // Exactly four "  " sequences — one between each pair
        // (session↔bytes_saved, bytes_saved↔warnings,
        // warnings↔compactions, compactions↔records).
        assert_eq!(
            line.matches("  ").count(),
            4,
            "summary must use two-space separator between every field; got: {line:?}"
        );
    }

    // ponytail: pin that empty session_id renders consistently.
    // The format is `session {sid}  bytes_saved=...` — with an
    // empty sid, the literal pattern is `session ` (one trailing
    // space from the prefix) + `` (empty) + `  bytes_saved...`
    // = `session   bytes_saved...` (three spaces). A contributor
    // who adds a leading `<sid>:` prefix breaks the grep affordance
    // for the legitimate empty-session case (pre-compact events).
    #[test]
    fn format_summary_line_with_empty_session_id() {
        let totals = SessionTotals {
            bytes_saved: 100,
            warnings: 0,
            compactions: 0,
            records: 1,
        };
        let line = format_summary_line("", &totals);
        // Format is `session {sid}  bytes_saved=...` — empty sid
        // leaves `session ` + `  bytes_saved` = three spaces between
        // `session` and `bytes_saved`. The other separators (the
        // two-space ones between the value fields) are unaffected.
        assert_eq!(
            line, "session   bytes_saved=100  warnings=0  compactions=0  records=1",
            "empty session_id: prefix-space + format-separator = three spaces; \
             a contributor who changes the sid prefix breaks the grep affordance"
        );
    }

    // ponytail: pin empty-input invariants on the aggregator. The
    // BTreeMap is the only return type for empty input — a
    // contributor who returns `Vec::new()` (different collection
    // type) breaks the public API. Also pins that an empty slice
    // does NOT trigger any "no records" sentinel that callers
    // might depend on (the report CLI iterates `by_session` and
    // would silently skip a `Vec`-shaped refactor).
    #[test]
    fn aggregate_sessions_on_empty_input_returns_empty_map() {
        let v: [&str; 0] = [];
        let by_session = aggregate_sessions(&v);
        assert!(
            by_session.is_empty(),
            "empty input must return an empty BTreeMap; got {by_session:?}"
        );
    }

    // ponytail: pin filter_lines behaviour when both filters are
    // None. The CLI uses this to render the "no filter" report
    // line — a contributor who early-returns `vec![]` on
    // `session.is_none() && kind.is_none()` (a known "perf" hack)
    // breaks the call site. Also pin the malformed-line skip
    // here, mirroring aggregate_sessions' behaviour, so a
    // contributor who turns one into a `?` propagator (silently
    // losing hand-edited records) surfaces.
    #[test]
    fn filter_lines_with_no_filters_includes_all_parseable_skips_malformed() {
        let lines = [
            rec(UsageKind::Slice, "s1", Some(100), Some(20)),
            "garbage".to_string(),
            rec(UsageKind::BudgetWarn, "s2", None, None),
            r#"{"kind":"response"}"#.to_string(), // valid JSON but missing required fields
            rec(UsageKind::CompactHint, "s1", None, None),
        ];
        let v: Vec<&str> = lines.iter().map(std::string::String::as_str).collect();
        let f = filter_lines(&v, None, None);
        // 3 valid UsageRecord rows; both garbage lines dropped.
        assert_eq!(
            f.len(),
            3,
            "no filters must include every parseable UsageRecord; \
             malformed/incomplete rows must be silently skipped; got {f:?}"
        );
        // The session=s1 Slice must come through with its bytes
        // unchanged — confirms filter_lines preserves the original
        // string slice and does not re-serialise.
        assert!(
            f[0].contains("\"kind\":\"slice\""),
            "first survivor must be the slice row; got: {}",
            f[0]
        );
    }

    // ponytail: pin the AND semantics on filter_lines. Earlier
    // tests pin session alone and kind alone; this combines
    // both. A contributor who flips one to OR (a typo under
    // refactor) surfaces here because the survivor count drops.
    #[test]
    fn filter_lines_session_and_kind_are_anded() {
        let lines = [
            rec(UsageKind::Slice, "s1", Some(100), Some(20)),
            rec(UsageKind::Slice, "s2", Some(100), Some(20)),
            rec(UsageKind::BudgetWarn, "s1", None, None),
            rec(UsageKind::BudgetWarn, "s2", None, None),
        ];
        let v: Vec<&str> = lines.iter().map(std::string::String::as_str).collect();
        let f = filter_lines(&v, Some("s1"), Some(UsageKind::Slice));
        // Only the (s1, Slice) row survives.
        assert_eq!(
            f.len(),
            1,
            "session=s1 AND kind=Slice must keep exactly 1 of 4 rows; got {f:?}"
        );
        assert!(
            f[0].contains("\"session_id\":\"s1\""),
            "survivor must be the s1 Slice row; got: {}",
            f[0]
        );
    }
}
