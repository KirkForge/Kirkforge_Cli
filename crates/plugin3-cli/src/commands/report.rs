//! `plugin3 report` — query cost-reporting records. ADR-0010.
//!
//! ponytail: this module is now a thin shell. The pure aggregator
//! (`aggregate_sessions`, `filter_lines`, `tail_lines`) lives in
//! plugin3-core where it can be tested without the binary. The
//! CLI side parses clap args and prints.

use plugin3_core::{cost::UsageKind, report, Paths};

pub(crate) fn run(
    last: usize,
    summary: bool,
    session: Option<String>,
    kind: Option<UsageKind>,
    as_json: bool,
) {
    let path = Paths::resolve().usage_log();
    at(&path, last, summary, session, kind, as_json);
}

// ponytail: extracted so a missing-file `--json` envelope is
// independently pinned (3 lines vs an inline ternary that the
// `--summary` precedence bug rode in on). `{}` for the summary
// path because aggregation returns a session→totals map; `[]`
// for the detailed path because each record is a list entry.
fn missing_file_envelope(summary: bool) -> &'static str {
    if summary {
        "{}"
    } else {
        "[]"
    }
}

// ponytail: path-parameterised `at` so tests in plugin3-core
// can drive it via a tempdir without touching the user's XDG
// data dir. Reads the file, applies filters (delegating to the
// pure aggregator), truncates, and prints per mode.
pub(crate) fn at(
    path: &std::path::Path,
    last: usize,
    summary: bool,
    session: Option<String>,
    kind: Option<UsageKind>,
    as_json: bool,
) -> usize {
    let Ok(s) = std::fs::read_to_string(path) else {
        // ponytail: missing usage.jsonl must still produce a
        // parseable `--json` envelope — `[]` for the detailed view
        // and `{}` for `--summary` — so a wrapper script's `jq`
        // doesn't blow up on a fresh install. Pre-fix, the
        // missing-file branch eprintln'd and returned 0 with empty
        // stdout; a `plugin3 --json report` on a clean XDG data
        // dir returned exit 0 + no output, which a downstream
        // `jq '.[]'` parser treats as a stream error rather than
        // "no records yet". The human branch keeps its eprintln
        // because users want the breadcrumb when an alias
        // unexpectedly returns nothing.
        if as_json {
            println!("{}", missing_file_envelope(summary));
        } else {
            eprintln!("plugin3: no usage.jsonl at {}", path.display());
        }
        return 0;
    };
    let all: Vec<&str> = s.lines().collect();
    let filtered = report::filter_lines(&all, session.as_deref(), kind);

    // ponytail: --summary wins over --json when both are set (same
    // way the human branch works). The previous ordering returned
    // raw records for `report --summary --json`, which is the
    // useless output for a dashboard reader that asked for
    // aggregated totals. Summary is the load-bearing shape on the
    // JSON path too — emit the same `{session: totals}` shape
    // either way.
    //
    // ponytail: aggregate the FULL filtered set under --summary,
    // not the last-N truncated slice. Per ADR-0010 § Report
    // subcommand, --last is the detailed-view knob ("Detailed
    // view: last N records, one per line"); the summary view is
    // "total bytes saved, total warnings, total compactions,
    // per-session totals". Pre-fix, `aggregate_sessions` ran on
    // `tail_lines(&filtered, last)` — so `report --summary --last 5`
    // on a 1000-record file with session "early" only in records
    // 1-50 would silently drop session "early" from the summary,
    // because tail-5 misses it entirely. The split is now:
    //   --summary path  → aggregate over `filtered` (full set)
    //   detailed path   → tail_lines(&filtered, last) (last N)
    if summary {
        let sessions = report::aggregate_sessions(&filtered);
        if as_json {
            // ponytail: serialise the BTreeMap directly. Keys are
            // session_id strings; values are SessionTotals (Copy,
            // serialise-only — no Deserialize needed for output).
            crate::json_out::print_json_or(&sessions, missing_file_envelope(true));
        } else {
            for (sid, t) in &sessions {
                println!("{}", report::format_summary_line(sid, t));
            }
        }
        return sessions.len();
    }
    let lines: &[&str] = report::tail_lines(&filtered, last);
    if as_json {
        let parsed: Vec<serde_json::Value> = lines
            .iter()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        crate::json_out::print_json_or(&parsed, missing_file_envelope(false));
        return lines.len();
    }
    for line in lines {
        println!("{line}");
    }
    lines.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin3_core::cost::{UsageKind, UsageRecord};
    use std::path::PathBuf;

    // ponytail: one-line UsageRecord builder so each test row stays
    // compact (mirrors plugin3-core/src/report.rs::tests::rec).
    fn rec(kind: UsageKind, session: &str) -> String {
        serde_json::to_string(&UsageRecord {
            ts: chrono::DateTime::parse_from_rfc3339("2026-06-29T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            kind,
            session_id: session.into(),
            bytes_in: None,
            bytes_out: None,
            tokens_used: None,
            tokens_ceiling: None,
            tool: None,
        })
        .unwrap()
    }

    // ponytail: write a JSONL usage log into a tempdir and return
    // its path. The `at()` function reads by path, so the test
    // never touches `Paths::resolve()` (which hits XDG dirs).
    fn write_log(dir: &std::path::Path, lines: &[String]) -> PathBuf {
        let p = dir.join("usage.jsonl");
        std::fs::write(&p, lines.join("\n")).unwrap();
        p
    }

    // ponytail: pin the missing-file branch shape. Return value
    // must be 0 in every variant — a contributor who early-returns
    // `lines.len()` before the file-exists check would only fail
    // when the file is absent. The envelope helper is tested
    // independently below so the JSON-shape contract is also
    // pinned.
    #[test]
    fn at_missing_file_returns_zero_in_every_mode() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("does-not-exist.jsonl");
        // summary + json
        assert_eq!(at(&absent, 10, true, None, None, true), 0);
        // summary + human
        assert_eq!(at(&absent, 10, true, None, None, false), 0);
        // detailed + json
        assert_eq!(at(&absent, 10, false, None, None, true), 0);
        // detailed + human
        assert_eq!(at(&absent, 10, false, None, None, false), 0);
    }

    // ponytail: pin the JSON envelopes literally. A `jq '.foo'`
    // on a fresh install MUST see an empty map (summary) or empty
    // array (detailed) — never a stream error. A contributor who
    // swaps the literals (or returns empty string) breaks every
    // wrapper script that pipes `plugin3 --json report` into jq.
    #[test]
    fn missing_file_envelope_is_pinned() {
        assert_eq!(
            missing_file_envelope(true),
            "{}",
            "summary branch must emit empty JSON object envelope"
        );
        assert_eq!(
            missing_file_envelope(false),
            "[]",
            "detailed branch must emit empty JSON array envelope"
        );
    }

    // ponytail: pin --summary wins over --json. The aggregate map
    // (object) and the raw records (array) are different JSON
    // shapes — the user asked for the aggregated shape, so they
    // get it regardless of `--json`. Here we verify via return
    // value: summary returns `sessions.len()` (count of distinct
    // sessions), while detailed would return `lines.len()`
    // (count of records). 4 records across 2 sessions → summary
    // returns 2, detailed returns 4.
    #[test]
    fn at_summary_returns_session_count_not_record_count() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::BudgetWarn, "s1"),
            rec(UsageKind::Slice, "s2"),
            rec(UsageKind::CompactHint, "s2"),
        ];
        let p = write_log(dir.path(), &lines[..]);
        // summary path → 2 distinct sessions.
        assert_eq!(
            at(&p, usize::MAX, true, None, None, false),
            2,
            "summary path returns session count, not record count"
        );
        // detailed path → 4 records (tail_lines with MAX passes through).
        assert_eq!(
            at(&p, usize::MAX, false, None, None, false),
            4,
            "detailed path returns record count, not session count"
        );
    }

    // ponytail: pin the bugfix from the inline comment — --summary
    // aggregates over the FULL filtered set, not the last-N
    // truncated slice. Setup: 4 records, only 2 distinct sessions.
    // Tail at 2 records → would surface only the last 2 records
    // (both session=s2). Aggregate-over-tail would yield 1 session.
    // Aggregate-over-filtered yields 2 sessions. A contributor
    // who regresses `report::aggregate_sessions(&filtered)` →
    // `report::aggregate_sessions(tail_lines(&filtered, last))`
    // surfaces here as count=1.
    #[test]
    fn at_summary_aggregates_full_filtered_set_not_last_n_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            rec(UsageKind::Slice, "early"), // record 0 — would be cut by tail=2
            rec(UsageKind::Slice, "early"), // record 1 — would be cut by tail=2
            rec(UsageKind::Slice, "late"),  // record 2 — kept by tail=2
            rec(UsageKind::Slice, "late"),  // record 3 — kept by tail=2
        ];
        let p = write_log(dir.path(), &lines[..]);
        // last=2 forces tail_lines truncation. If summary mistakenly
        // ran on the truncated slice, only `late` would be present.
        assert_eq!(
            at(&p, 2, true, None, None, false),
            2,
            "summary must aggregate the FULL filtered set; tail_lines \
             truncation must NOT affect the summary view — `early` would \
             be missing (count=1) if aggregation ran on the tail"
        );
    }

    // ponytail: pin the detailed-view tail behaviour. 5 records,
    // last=2 → returns 2. last=0 → returns 0. last≥len → returns
    // len. A contributor who drops the tail_lines call (passing
    // `filtered` straight to the printer) surfaces here because
    // last=2 would yield 5 not 2.
    #[test]
    fn at_detailed_view_truncates_to_last_n_lines() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::BudgetWarn, "s1"),
            rec(UsageKind::Slice, "s2"),
            rec(UsageKind::BudgetWarn, "s2"),
            rec(UsageKind::CompactHint, "s1"),
        ];
        let p = write_log(dir.path(), &lines[..]);
        assert_eq!(
            at(&p, 2, false, None, None, false),
            2,
            "detailed view with last=2 returns 2 records (the tail)"
        );
        assert_eq!(
            at(&p, 0, false, None, None, false),
            0,
            "detailed view with last=0 returns 0 records (empty tail)"
        );
        assert_eq!(
            at(&p, 99, false, None, None, false),
            5,
            "detailed view with last>=len returns full filtered set"
        );
    }

    // ponytail: pin the filter propagation into `at()`. The CLI
    // passes `session`/`kind` through to `report::filter_lines`
    // before aggregation; a contributor who drops the args (or
    // wires them to `None` always) surfaces here because the
    // record count drops. 4 records, session=s1 → 2 survive.
    #[test]
    fn at_session_filter_propagates_to_count() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::BudgetWarn, "s1"),
            rec(UsageKind::Slice, "s2"),
            rec(UsageKind::BudgetWarn, "s2"),
        ];
        let p = write_log(dir.path(), &lines[..]);
        // detailed + session=s1 → 2 surviving records
        assert_eq!(
            at(&p, 99, false, Some("s1".into()), None, false),
            2,
            "session=s1 must filter down to 2 records"
        );
        // summary + session=s1 → still 1 distinct session (s1)
        assert_eq!(
            at(&p, 99, true, Some("s1".into()), None, false),
            1,
            "session=s1 summary returns 1 distinct session"
        );
        // missing session → 0 (filter excludes every line)
        assert_eq!(
            at(&p, 99, false, Some("nope".into()), None, false),
            0,
            "session=nope returns 0 records (none match)"
        );
    }

    // ponytail: pin kind filter on the CLI path. 4 records, 2
    // Slice and 2 BudgetWarn. `kind=Slice` → 2 survive. A
    // contributor who forgets to forward `kind` to filter_lines
    // (typo under refactor: `kind.unwrap_or(None)` swallows it)
    // surfaces here as count=4.
    #[test]
    fn at_kind_filter_propagates_to_count() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::BudgetWarn, "s1"),
            rec(UsageKind::Slice, "s2"),
            rec(UsageKind::BudgetWarn, "s2"),
        ];
        let p = write_log(dir.path(), &lines[..]);
        assert_eq!(
            at(&p, 99, false, None, Some(UsageKind::Slice), false),
            2,
            "kind=Slice must filter to 2 slice records"
        );
        assert_eq!(
            at(&p, 99, false, None, Some(UsageKind::BudgetWarn), false),
            2,
            "kind=BudgetWarn must filter to 2 warn records"
        );
        assert_eq!(
            at(&p, 99, false, None, Some(UsageKind::CompactHint), false),
            0,
            "kind=CompactHint (absent) returns 0 records"
        );
    }
}
