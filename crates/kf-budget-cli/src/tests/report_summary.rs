// ponytail: ADR-0015 § Exit codes — exercises the binary as a subprocess
// so the real clap + std::process::exit paths are taken. Unit tests on
// the inner functions would not catch a regression where someone moves
// the exit(78) call behind a flag.
use super::*;

#[test]
fn report_summary_json_envelope_shape_is_pinned() {
    // ponytail: pin the `kf-budget --json report --summary` wire
    // shape. The CLI emits a JSON object keyed by session_id
    // (BTreeMap → sorted), each value a `SessionTotals` with
    //   {bytes_saved, warnings, compactions, records}
    // (all snake_case, all `usize`). ADR-0010 § Report
    // subcommand names this shape; the existing tests pin the
    // *return value* (`sessions.len()`) but never parsed stdout
    // to verify the actual field set. A contributor who renames
    // `bytes_saved` → `bytes_dropped` (or splits `records` into
    // `records_seen` + `records_kept`) breaks every dashboard
    // jq filter silently — `jq '.s1.records'` returns null, no
    // error. Drift catches here.
    //
    // Seed two sessions across two record kinds so we exercise
    // the aggregation across multiple `session_id` keys (the
    // BTreeMap branch, not the degenerate single-session case).
    // Slice records contribute to `bytes_saved` and `records`;
    // BudgetWarn contributes to `warnings` and `records`.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let usage_dir = data_dir.path().join("logs");
    std::fs::create_dir_all(&usage_dir).unwrap();
    let usage_path = usage_dir.join("usage.jsonl");
    let mut s = String::new();
    // ponytail: inline the UsageRecord build (the `tests::rec`
    // helper is in a sibling test module and isn't `pub(crate)`).
    // Slice records carry bytes_in/bytes_out so the aggregator's
    // bytes_saved math has something to roll up.
    let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
        let mut r = UsageRecord {
            ts: chrono::Utc::now(),
            kind,
            session_id: sid.into(),
            bytes_in: None,
            bytes_out: None,
            tokens_used: None,
            tokens_ceiling: None,
            tool: None,
        };
        if matches!(r.kind, UsageKind::Slice) {
            r.bytes_in = Some(1000);
            r.bytes_out = Some(400);
        }
        r
    };
    for r in [
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::BudgetWarn, "alpha"),
        mk(UsageKind::CompactHint, "bravo"),
        mk(UsageKind::BudgetOver, "bravo"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--summary"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect(
        "report --summary --json top-level must be an object \
                     (BTreeMap<String, SessionTotals>), not an array",
    );
    // ponytail: assert the BTreeMap ordering — sessions appear
    // sorted by session_id. A contributor who switches to
    // HashMap keeps the wire contract valid but loses the
    // determinism that makes diffs reviewable. This is a
    // separate drift from the field-set pin above, but the
    // cost of asserting is one line.
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["alpha", "bravo"],
        "BTreeMap ordering must be alphabetical; switching to \
             HashMap loses diff-stable output. got: {keys:?}"
    );
    assert_eq!(
        obj.len(),
        2,
        "two distinct session_ids must produce two top-level keys"
    );

    // ponytail: pin the per-session field set. SessionTotals
    // lives in kf-budget-core and is serde-derived; the JSON
    // shape is a direct reflection of the struct. A
    // contributor who adds a field (e.g. `tokens_used`) without
    // updating this test surfaces here.
    let expected_fields: std::collections::BTreeSet<&str> =
        ["bytes_saved", "compactions", "records", "warnings"]
            .into_iter()
            .collect();
    for (sid, totals) in obj {
        let tobj = totals
            .as_object()
            .unwrap_or_else(|| panic!("{sid} totals must be object"));
        let fields: std::collections::BTreeSet<&str> = tobj.keys().map(String::as_str).collect();
        assert_eq!(
            fields, expected_fields,
            "{sid} field set drifted from SessionTotals; got: {fields:?}"
        );
    }

    // ponytail: assert the aggregation itself on one session
    // — alpha has 2 Slice (bytes_in=1000, bytes_out=400, so
    // bytes_saved = (1000-400)*2 = 1200) + 1 BudgetWarn
    // (records=1). bravo has 1 CompactHint + 1 BudgetOver
    // (records=2, no slice contribution). These numbers are
    // the contract: a contributor who breaks
    // `aggregate_sessions`'s slice byte accounting (e.g.
    // drops `bytes_in - bytes_out` and just counts rows)
    // surfaces here. The wire shape pin above and the
    // value pin here are independent — both belong.
    let alpha = &v["alpha"];
    assert_eq!(
        alpha["records"], 3,
        "alpha saw 3 records (2 Slice + 1 BudgetWarn); got: {}",
        alpha["records"]
    );
    assert_eq!(
        alpha["warnings"], 1,
        "alpha saw 1 BudgetWarn; got: {}",
        alpha["warnings"]
    );
    assert_eq!(
        alpha["bytes_saved"], 1200,
        "alpha's 2 Slice records saved (1000-400)*2 = 1200 bytes; \
             got: {}",
        alpha["bytes_saved"]
    );
    assert_eq!(
        alpha["compactions"], 0,
        "alpha had no CompactHint; got: {}",
        alpha["compactions"]
    );

    let bravo = &v["bravo"];
    assert_eq!(
        bravo["records"], 2,
        "bravo saw 2 records (1 CompactHint + 1 BudgetOver); got: {}",
        bravo["records"]
    );
    assert_eq!(
        bravo["compactions"], 1,
        "bravo saw 1 CompactHint; got: {}",
        bravo["compactions"]
    );
    assert_eq!(
        bravo["warnings"], 1,
        "bravo saw 1 BudgetOver; got: {}",
        bravo["warnings"]
    );
    assert_eq!(
        bravo["bytes_saved"], 0,
        "bravo had no Slice records; got: {}",
        bravo["bytes_saved"]
    );
}

#[test]
fn report_summary_human_text_output_shape_is_pinned() {
    // ponytail: pin the `kf-budget report --summary` NON-JSON wire
    // shape. The JSON sibling is pinned above
    // (`report_summary_json_envelope_shape_is_pinned`); this
    // one pins the human-readable text that ADR-0010 § Report
    // subcommand documents as the default output. The unit
    // tests in commands::report::at() drive `format_summary_line`
    // directly and would still pass if a contributor replaced
    // the call with `{sid:?} {t:?}` — the subprocess boundary
    // is the only place the exact line shape is observable
    // end-to-end. A regex-pin via `format_summary_line`'s
    // return type would only pin the contract; this pins the
    // rendering.
    //
    // The seeded rows and the alpha/bravo per-session totals
    // mirror the JSON-sibling test above so a reader can
    // diff the two and see "same fixture, two renderers".
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let usage_dir = data_dir.path().join("logs");
    std::fs::create_dir_all(&usage_dir).unwrap();
    let usage_path = usage_dir.join("usage.jsonl");
    let mut s = String::new();
    let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
        let mut r = UsageRecord {
            ts: chrono::Utc::now(),
            kind,
            session_id: sid.into(),
            bytes_in: None,
            bytes_out: None,
            tokens_used: None,
            tokens_ceiling: None,
            tool: None,
        };
        if matches!(r.kind, UsageKind::Slice) {
            r.bytes_in = Some(1000);
            r.bytes_out = Some(400);
        }
        r
    };
    for r in [
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::BudgetWarn, "alpha"),
        mk(UsageKind::CompactHint, "bravo"),
        mk(UsageKind::BudgetOver, "bravo"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    let out = std::process::Command::new(kf_budget_binary_path())
        // NOTE: no `--json` here — this is the human-readable
        // branch. `commands::report::at()` routes to the
        // `format_summary_line` loop (per-session println!).
        .args(["report", "--summary"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");

    // ponytail: pin "one line per session, no envelope". A
    // contributor who wraps the output in `[...]` (treating it
    // like a JSON list) or who emits `--- summary ---` framing
    // surfaces here — the line count must equal the number of
    // distinct session_ids. Stdout must not be empty; an empty
    // stdout would mean the human branch silently dropped the
    // aggregate, which a wrapper script's `grep -c session`
    // interprets as zero records rather than an error.
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "two distinct session_ids must produce exactly two \
             summary lines; got: {lines:?}"
    );

    // ponytail: pin BTreeMap ordering on the human-readable
    // branch too. JSON sibling asserts this above; the human
    // branch goes through the same `BTreeMap<String,
    // SessionTotals>` so the order matches. A contributor who
    // sorts the JSON branch but not the human branch (or vice
    // versa) breaks the diff-stable review expectation.
    assert!(
        lines[0].starts_with("session alpha  "),
        "first summary line must be alpha (BTreeMap order); \
             got: {}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("session bravo  "),
        "second summary line must be bravo (BTreeMap order); \
             got: {}",
        lines[1]
    );

    // ponytail: pin the exact line shape
    //   session <sid>  bytes_saved=N  warnings=N  compactions=N  records=N
    // with TWO spaces between fields (not tabs, not single
    // spaces, not `key=value` quoting). The fixture gives:
    //   alpha: 2 Slice (bytes_in=1000, bytes_out=400 → 1200) + 1 BudgetWarn
    //   bravo: 1 CompactHint + 1 BudgetOver (no slice contribution)
    // The substring scan is exact — no wildcards, no
    // normalisation. A contributor who switches to `key = value`
    // (single space, padded) or drops the two-space gutter
    // surfaces here. The chosen field set mirrors SessionTotals;
    // adding a field to that struct (e.g. `tokens_used`) without
    // updating this pin surfaces here too.
    let alpha_line = lines[0];
    assert!(
        alpha_line.contains("session alpha  "),
        "alpha line must lead with `session alpha  ` (note the \
             two-space gutter between sid and the first field); \
             got: {alpha_line}"
    );
    assert!(
        alpha_line.contains("  bytes_saved=1200  "),
        "alpha line must carry `bytes_saved=1200` with the \
             two-space gutter (2 Slice rows × (1000-400) bytes); \
             got: {alpha_line}"
    );
    assert!(
        alpha_line.contains("  warnings=1  "),
        "alpha line must carry `warnings=1` (1 BudgetWarn); \
             got: {alpha_line}"
    );
    assert!(
        alpha_line.contains("  compactions=0  "),
        "alpha line must carry `compactions=0` (no CompactHint); \
             got: {alpha_line}"
    );
    assert!(
        alpha_line.contains("  records=3"),
        "alpha line must carry `records=3` (2 Slice + 1 \
             BudgetWarn); `records=` must be the last field (no \
             trailing gutter). got: {alpha_line}"
    );

    let bravo_line = lines[1];
    assert!(
        bravo_line.contains("session bravo  "),
        "bravo line must lead with `session bravo  `; got: {bravo_line}"
    );
    assert!(
        bravo_line.contains("  bytes_saved=0  "),
        "bravo line must carry `bytes_saved=0` (no Slice); \
             got: {bravo_line}"
    );
    assert!(
        bravo_line.contains("  warnings=1  "),
        "bravo line must carry `warnings=1` (1 BudgetOver); \
             got: {bravo_line}"
    );
    assert!(
        bravo_line.contains("  compactions=1  "),
        "bravo line must carry `compactions=1` (1 CompactHint); \
             got: {bravo_line}"
    );
    assert!(
        bravo_line.contains("  records=2"),
        "bravo line must carry `records=2` (1 CompactHint + 1 \
             BudgetOver); got: {bravo_line}"
    );

    // ponytail: negative pin — the JSON-sibling envelope
    // markers must NOT leak into the human branch. A
    // contributor who copy-pastes the JSON branch's
    // `serde_json::to_string_pretty` call into the human
    // branch surfaces here (stdout would contain `{` / `}` /
    // `"` markers that don't belong on the human branch).
    assert!(
        !stdout.contains('{') && !stdout.contains('}'),
        "human summary branch must NOT emit JSON envelope \
             markers; got stdout: {stdout:?}"
    );
}

#[test]
fn report_summary_with_session_and_kind_filters_at_subprocess() {
    // ponytail: subprocess pin for `kf-budget --json report
    // --summary --session <SID> --kind <K>`. The Round 50
    // --summary envelope test pins the no-filter wire shape;
    // the Round 56 combined-filter test pins the detailed-view
    // (array) wire shape. Neither pins the COMBINATION under
    // --summary. A contributor who breaks one of these
    // regressions stays silent under the existing pins:
    //
    //   (a) routing --session/--kind into `aggregate_sessions`
    //       (e.g. summary path bypasses `filter_lines` and
    //       reads the raw file) → emits ALL session keys
    //       instead of the filtered subset.
    //   (b) summary path with filters routes through
    //       `tail_lines(&filtered, last)` instead of
    //       `aggregate_sessions(&filtered)` (the Round 52
    //       bug-shape, but with filters applied — the
    //       per-session totals would silently vanish).
    //   (c) summary's `aggregate_sessions` reads the
    //       wrong field for Slice byte math under a kind
    //       filter (e.g. forgets to honour bytes_in/out on
    //       filtered rows).
    //
    // Three arms from one seed exercise each:
    //   --summary                  → 3 sessions (full set)
    //   --summary --session alpha --kind slice
    //                              → alpha only (records=2,
    //                                bytes_saved=1200)
    //   --summary --session charlie (no --kind)
    //                              → charlie only
    //                                (records=2, warnings=1,
    //                                 compactions=1)
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let usage_dir = data_dir.path().join("logs");
    std::fs::create_dir_all(&usage_dir).unwrap();
    let usage_path = usage_dir.join("usage.jsonl");
    let mut s = String::new();
    let mk =
        |kind: UsageKind, sid: &str, b_in: Option<usize>, b_out: Option<usize>| -> UsageRecord {
            UsageRecord {
                ts: chrono::Utc::now(),
                kind,
                session_id: sid.into(),
                bytes_in: b_in,
                bytes_out: b_out,
                tokens_used: None,
                tokens_ceiling: None,
                tool: None,
            }
        };
    // ponytail: 5 rows across 3 sessions, 4 kinds. The
    // charlie rows deliberately carry no Slice so the
    // charlie-only arm proves the filter actually filtered
    // (bytes_saved must be 0, not 0 by coincidence).
    //   alpha:  2 Slice (1000→400 each)
    //   bravo:  1 Slice (500→100)
    //   charlie: 1 BudgetWarn + 1 CompactHint
    for r in [
        mk(UsageKind::Slice, "alpha", Some(1000), Some(400)),
        mk(UsageKind::Slice, "alpha", Some(1000), Some(400)),
        mk(UsageKind::Slice, "bravo", Some(500), Some(100)),
        mk(UsageKind::BudgetWarn, "charlie", None, None),
        mk(UsageKind::CompactHint, "charlie", None, None),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --summary only → 3 session keys, full totals.
    // Anchor arm — establishes the seed's expected
    // aggregation without filters. Used as the baseline to
    // confirm the filtered arms are stricter (fewer keys,
    // narrower totals).
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--summary"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --summary");
    assert!(
        out.status.success(),
        "--summary must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v
        .as_object()
        .expect("--summary --json top-level must be an object (BTreeMap)");
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["alpha", "bravo", "charlie"],
        "--summary with no filters must surface all 3 sessions; \
             a different set here means aggregate_sessions bypassed \
             the file or dropped a session key. got: {keys:?}"
    );

    // Arm 2: --summary --session alpha --kind slice → alpha
    // only. The combination is the load-bearing case (Round
    // 56 pins it on the detailed view; this pins it on
    // summary). Catches:
    //   (a) summary path bypasses filter_lines → would emit
    //       alpha+bravo+charlie (=3 keys, count mismatch)
    //   (b) summary path routes through tail_lines → still
    //       3 keys but alpha's records would be tail-truncated
    let out = std::process::Command::new(kf_budget_binary_path())
        .args([
            "--json",
            "report",
            "--summary",
            "--session",
            "alpha",
            "--kind",
            "slice",
        ])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --summary --session alpha --kind slice");
    assert!(
        out.status.success(),
        "--summary with combined filters must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level is an object");
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["alpha"],
        "combined --session alpha --kind slice under --summary \
             must produce exactly {{alpha}} (BTreeMap ordering). \
             A wider set here means the summary path bypassed \
             filter_lines and routed the unfiltered file through \
             aggregate_sessions. got: {keys:?}"
    );
    let alpha = &v["alpha"];
    assert_eq!(
        alpha["records"], 2,
        "alpha has 2 Slice records; got: {}",
        alpha["records"]
    );
    assert_eq!(
        alpha["bytes_saved"], 1200,
        "alpha's 2 Slice records saved (1000-400)*2 = 1200 bytes; \
             a different value means the summary path dropped a row \
             (filter-then-tail bug) or mis-summed bytes_in/bytes_out. \
             got: {}",
        alpha["bytes_saved"]
    );
    assert_eq!(
        alpha["warnings"], 0,
        "alpha had no BudgetWarn/BudgetOver; got: {}",
        alpha["warnings"]
    );
    assert_eq!(
        alpha["compactions"], 0,
        "alpha had no CompactHint; got: {}",
        alpha["compactions"]
    );

    // Arm 3: --summary --session charlie (no --kind) →
    // charlie only, with BOTH warnings AND compactions
    // populated. Catches a contributor who wires --session
    // to work but accidentally narrows the kind implicitly
    // (e.g. `--session` filter hardcodes kind=slice). The
    // charlie rows are deliberately non-Slice so the
    // expectations differ from arm 2's alpha totals.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--summary", "--session", "charlie"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --summary --session charlie");
    assert!(
        out.status.success(),
        "--summary --session charlie must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level is an object");
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["charlie"],
        "--summary --session charlie must produce exactly \
             {{charlie}}; a wider set means --session didn't \
             reach the summary path. got: {keys:?}"
    );
    let charlie = &v["charlie"];
    assert_eq!(
        charlie["records"], 2,
        "charlie has 2 records (1 BudgetWarn + 1 CompactHint); got: {}",
        charlie["records"]
    );
    assert_eq!(
        charlie["warnings"], 1,
        "charlie's BudgetWarn must count as 1 warning; got: {}",
        charlie["warnings"]
    );
    assert_eq!(
        charlie["compactions"], 1,
        "charlie's CompactHint must count as 1 compaction; got: {}",
        charlie["compactions"]
    );
    assert_eq!(
        charlie["bytes_saved"], 0,
        "charlie has no Slice records (filter must not have \
             leaked a Slice row); got: {}",
        charlie["bytes_saved"]
    );
}

#[test]
fn report_json_emits_empty_envelope_when_usage_log_missing() {
    // ponytail: pin the missing-usage.jsonl contract for
    // `--json` mode. The bug-fixed behaviour: when
    // `data_dir/logs/usage.jsonl` doesn't exist (fresh
    // install, never-run hooks, freshly-rotated logs), the
    // CLI emits a parseable envelope — `[]` for the detailed
    // view, `{}` for `--summary` — and exits 0 with no
    // stderr noise. Pre-fix, the missing-file branch
    // eprintln'd and returned 0 with empty stdout; a wrapper
    // doing `kf-budget --json report | jq '.[]'` on a clean
    // XDG data dir got exit 0 + no output, which `jq`
    // treats as a stream error rather than "no records
    // yet". The human branch keeps its eprintln (users want
    // the breadcrumb when an alias unexpectedly returns
    // nothing).
    //
    // Three arms:
    //   1. `kf-budget --json report`            → `[]`
    //   2. `kf-budget --json report --summary`  → `{}`
    //   3. `kf-budget report` (no --json)       → exit 0, eprintln
    // Arm 3 pins that the breadcrumb is preserved on the
    // human branch — a contributor who drops the
    // `as_json ? "[]" : eprintln!(...)` ternary to a flat
    // `println!("[]")` silences the breadcrumb and surfaces
    // here as a stderr-empty mismatch.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");

    // arm 1: detailed --json → []
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget");
    assert!(
        out.status.success(),
        "missing usage.jsonl on --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout,
        b"[]\n",
        "missing usage.jsonl on --json must emit `[]` (a parseable \
             JSON array), not empty stdout — a wrapper `jq '.[]' | ...` \
             treats empty stdout as a stream error rather than 'no \
             records yet'. got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.stderr.is_empty(),
        "missing usage.jsonl on --json must NOT eprintln the \
             'no usage.jsonl' breadcrumb — the breadcrumb is for the \
             human branch only. got: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    // arm 2: --summary --json → {}
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--summary"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget");
    assert!(
        out.status.success(),
        "missing usage.jsonl on --summary --json must exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout,
        b"{}\n",
        "missing usage.jsonl on --summary --json must emit a \
             parseable JSON object (empty BTreeMap, `{{}}`); a \
             different value here means the missing-file branch \
             hardcodes only the array case. got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.stderr.is_empty(),
        "missing usage.jsonl on --summary --json must NOT \
             eprintln — same contract as arm 1. got: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    // arm 3: human branch (no --json) keeps the breadcrumb.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["report"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget");
    assert!(
        out.status.success(),
        "missing usage.jsonl on the human branch must exit 0 \
             (it's a 'no records yet' state, not an error); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no usage.jsonl"),
        "human branch must keep the 'no usage.jsonl at ...' \
             breadcrumb so a user whose `report` alias suddenly \
             returns empty gets a hint about why. got: {stderr}"
    );
}
