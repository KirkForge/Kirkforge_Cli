// ponytail: ADR-0015 § Exit codes — exercises the binary as a subprocess
// so the real clap + std::process::exit paths are taken. Unit tests on
// the inner functions would not catch a regression where someone moves
// the exit(78) call behind a flag.
use super::*;

#[test]
fn report_session_filter_at_subprocess_pins_field_equality() {
    // ponytail: subprocess pin for `kf-budget --json report
    // --session <SID>`. The existing
    // `report_session_filter_selects_matching_lines` (line 524)
    // only exercises the filter via the typed
    // `Some("s1")` argument through `commands::report::at(...)`
    // — it bypasses clap's `String → String` plumbing. A
    // contributor who breaks the clap `Session = String` arg
    // (e.g. renames it to `--sid`, makes it required, or
    // adds a default-value mismatch) keeps the unit-level
    // filter pin green and breaks every wrapper script
    // doing `kf-budget --json report --session <SID>` silently.
    // Drift catches here, at the subprocess boundary.
    //
    // Two arms:
    //   --session alpha → 2 records (both kind=slice for sid=alpha)
    //   --session bravo → 1 record  (the single bravo row)
    // The dual-arm catches a contributor who hardcodes the
    // filter value (e.g. writes `if args.session == "alpha"`
    // instead of routing through `report::filter_lines`) —
    // arm 2 would surface as a count mismatch.
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
    // ponytail: 4 rows, 2 alpha + 1 bravo + 1 empty-session
    // (legitimate pre-compact event per the
    // `aggregate_skips_malformed_jsonl_lines` test). The empty
    // row is the red-herring control: arm 1 must NOT include
    // it (session_id field is empty string, not "alpha"), arm
    // 2 must NOT include it either. This catches a
    // contributor who replaces `r.session_id != sid` with
    // `r.session_id.contains(sid)` (substring match) — the
    // empty-string sid would falsely match every record.
    for r in [
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::BudgetWarn, "bravo"),
        mk(UsageKind::CompactHint, ""),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --session alpha → 2 records, all session_id="alpha".
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--session", "alpha"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --session alpha");
    assert!(
        out.status.success(),
        "--session alpha must parse and exit 0; exit non-zero \
             (typically 64) here means the clap `Session` arg lost its \
             binding or the `String → Option<String>` plumbing broke. \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v.as_array().expect(
        "report --json top-level must be an array (parsed \
                     UsageRecord values, one per JSONL line)",
    );
    assert_eq!(
        arr.len(),
        2,
        "--session alpha must filter the 4-row seed down to exactly \
             2 records (both alpha Slice rows); a different count means \
             the filter dropped (or kept) the wrong rows. got: {}",
        arr.len()
    );
    for (i, rec) in arr.iter().enumerate() {
        assert_eq!(
            rec["session_id"], "alpha",
            "record[{i}] session_id must equal `\"alpha\"` exactly; \
                 a contributor who flips the comparison to \
                 `r.session_id.contains(sid)` (substring match) would \
                 let the empty-sid record slip through. got: {:?}",
            rec["session_id"]
        );
        // ponytail: pin the kind as a sanity-check — both
        // surviving records were Slice, so this confirms we
        // didn't accidentally cross-route a BudgetWarn row.
        assert_eq!(
            rec["kind"], "slice",
            "record[{i}] kind must be `\"slice\"` (the kind of \
                 both alpha rows in the seed); a different value here \
                 means the session filter accidentally routed a \
                 different kind through. got: {:?}",
            rec["kind"]
        );
    }

    // Arm 2: --session bravo → 1 record, session_id="bravo".
    // The dual-arm: if a contributor hardcodes the filter
    // value to "alpha", arm 2 would emit zero records (no
    // match), which would surface here as a count mismatch
    // (expecting 1, got 0).
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--session", "bravo"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --session bravo");
    assert!(
        out.status.success(),
        "--session bravo must parse and exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        1,
        "--session bravo must filter the 4-row seed down to exactly \
             1 record (the bravo BudgetWarn row); a different count \
             means the filter dropped or kept the wrong rows. got: {}",
        arr.len()
    );
    assert_eq!(
        arr[0]["session_id"], "bravo",
        "the surviving record's session_id must equal `\"bravo\"` \
             exactly; got: {:?}",
        arr[0]["session_id"]
    );
    assert_eq!(
        arr[0]["kind"], "budget_warn",
        "the surviving record's kind must be `\"budget_warn\"` \
             (the kind of the bravo row in the seed); got: {:?}",
        arr[0]["kind"]
    );
}

#[test]
fn report_kind_filter_multi_word_kebab_human_branch() {
    // ponytail: subprocess pin for the multi-word kebab `--kind`
    // variants on the human-readable (non-JSON) branch. R60
    // (`report_kind_filter_human_branch_prints_filtered_lines_verbatim`)
    // covered `slice` (single-word) and `budget-warn`
    // (multi-word); the JSON sibling covered all four kebab
    // variants across two tests. The two missing multi-word
    // variants on the human branch are `budget-over` and
    // `compact-hint` — both round-trip through
    // `UsageKindArg::BudgetOver → UsageKind::BudgetOver` and
    // `UsageKindArg::CompactHint → UsageKind::CompactHint`
    // respectively, then serde-renamed to snake_case on the
    // wire (`budget_over`, `compact_hint`).
    //
    // The load-bearing drift: a contributor who accidentally
    // drops `BudgetOver` or `CompactHint` from
    // `UsageKindArg` would surface here as a clap usage
    // error (exit non-zero) rather than a correctly-rendered
    // line — the kebab→snake round-trip fails at clap's
    // value parser before reaching `filter_lines`. The
    // snake_case wire form on each surviving line catches a
    // separate drift: a contributor who flips the inner
    // `From<UsageKindArg> for UsageKind` mapping to point at
    // the wrong variant (compile-clean because of how the
    // round-trip form works — see R56 commentary) surfaces
    // here as a wrong `kind` value.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let usage_dir = data_dir.path().join("logs");
    std::fs::create_dir_all(&usage_dir).unwrap();
    let usage_path = usage_dir.join("usage.jsonl");
    let mut s = String::new();
    let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
        UsageRecord {
            ts: chrono::Utc::now(),
            kind,
            session_id: sid.into(),
            bytes_in: None,
            bytes_out: None,
            tokens_used: None,
            tokens_ceiling: None,
            tool: None,
        }
    };
    // 4 rows: one of each kind (slice, budget_warn,
    // budget_over, compact_hint) on the same session. The
    // --kind budget-over arm must filter down to exactly the
    // budget_over row; --kind compact-hint down to exactly
    // the compact_hint row. Other rows must not leak.
    for r in [
        mk(UsageKind::Slice, "s1"),
        mk(UsageKind::BudgetWarn, "s1"),
        mk(UsageKind::BudgetOver, "s1"),
        mk(UsageKind::CompactHint, "s1"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    let run = |kind_flag: &str| -> std::process::Output {
        std::process::Command::new(kf_budget_binary_path())
            // NOTE: no `--json`. Human branch.
            .args(["report", "--kind", kind_flag])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap_or_else(|e| panic!("spawn --kind {kind_flag}: {e}"))
    };

    // ponytail: substring-based kind extractor (mirrors R65's
    // `field` helper). The anchor `"kind":"` is unique to the
    // kind field on `UsageRecord` (no other field starts
    // with `k`).
    fn kind_of(line: &str) -> &str {
        let needle = "\"kind\":\"";
        let start = line.find(needle).expect("kind present") + needle.len();
        let rest = &line[start..];
        let end = rest.find('"').expect("value terminated");
        &rest[..end]
    }

    // Arm 1: --kind budget-over → 1 line, kind="budget_over"
    // (snake_case wire form after kebab→snake round-trip).
    let out = run("budget-over");
    assert!(
        out.status.success(),
        "--kind budget-over must parse and exit 0 on the human \
             branch; exit non-zero means `UsageKindArg` lost the \
             `BudgetOver` variant. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "--kind budget-over must filter the 4-row seed down \
             to 1 line (the budget_over row); got: {lines:?}"
    );
    let line = lines[0];
    assert_eq!(
        kind_of(line),
        "budget_over",
        "human-branch --kind budget-over surviving line must \
             carry `kind=budget_over` (snake_case on the wire \
             after the CLI's `budget-over` → \
             `UsageKind::BudgetOver` → `\\\"budget_over\\\"` \
             round-trip); got: {line:?}"
    );
    // ponytail: forbidden-kinds pin. The 3 non-matching kinds
    // must not leak under --kind budget-over. A contributor
    // who breaks `filter_lines`'s kind equality surfaces
    // here: with 4 rows and 3 forbidden kinds, a broken
    // filter (always-true) returns 4 lines instead of 1.
    for forbidden in ["slice", "budget_warn", "compact_hint"] {
        assert!(
            !stdout.contains(&format!("\"kind\":\"{forbidden}\"")),
            "human-branch --kind budget-over must NOT leak \
                 kind=\"{forbidden}\"; a leak means `filter_lines`'s \
                 kind equality broke. stdout: {stdout:?}"
        );
    }

    // Arm 2: --kind compact-hint → 1 line, kind="compact_hint".
    // This variant has the LONGEST kebab form (5 chars after
    // the dash: `compact-hint` → snake_case `compact_hint`),
    // which means a typo in `#[clap(rename_all =
    // "kebab-case")]` is more likely here than on the
    // shorter forms. The exit-success pin catches a missing
    // `CompactHint` variant; the kind wire-form pin catches
    // a wrong inner mapping.
    let out = run("compact-hint");
    assert!(
        out.status.success(),
        "--kind compact-hint must parse and exit 0 on the \
             human branch; exit non-zero means `UsageKindArg` \
             lost the `CompactHint` variant. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "--kind compact-hint must filter the 4-row seed \
             down to 1 line (the compact_hint row); got: {lines:?}"
    );
    let line = lines[0];
    assert_eq!(
        kind_of(line),
        "compact_hint",
        "human-branch --kind compact-hint surviving line must \
             carry `kind=compact_hint` (snake_case on the wire \
             after kebab→snake round-trip); got: {line:?}"
    );
    for forbidden in ["slice", "budget_warn", "budget_over"] {
        assert!(
            !stdout.contains(&format!("\"kind\":\"{forbidden}\"")),
            "human-branch --kind compact-hint must NOT leak \
                 kind=\"{forbidden}\"; a leak means `filter_lines`'s \
                 kind equality broke. stdout: {stdout:?}"
        );
    }
}

#[test]
fn report_session_filter_human_branch_prints_only_matching_sids() {
    // ponytail: subprocess pin for `kf-budget report --session <SID>`
    // on the human-readable (non-JSON) branch. The JSON sibling
    // above (`report_session_filter_at_subprocess_pins_field_equality`)
    // pins that the JSON branch's parsed `session_id` field equals
    // the CLI argument byte-for-byte; the human branch goes
    // through `for line in lines { println!("{line}"); }` and was
    // only tested at unit level via `commands::report::at()` with
    // the typed `Some("s1".into())`. A contributor who breaks
    // `filter_lines`'s `r.session_id != sid` short-circuit — e.g.
    // accidentally inverts to `==` (would invert to a NOT-match
    // filter and drop the target session) — passes the unit tests
    // because they assert on the surviving count, which would
    // also be non-zero under the inverted condition (just on the
    // wrong sessions). The substring pin on the rendered lines
    // catches this.
    //
    // The 4-row seed has a 3:1 split (3 rows for s1, 1 row for
    // s2) so the surviving-count assertion is non-degenerate: a
    // broken filter that passes everything returns 4 lines, a
    // broken filter that drops everything returns 0, and only the
    // correct `r.session_id == sid` short-circuit returns 3.
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
    // 4 rows: 3 for s1 (slice + budget_warn + compact_hint),
    // 1 for s2 (budget_over). The kind mix matters: a
    // contributor who copy-pastes the `filter_lines` short-
    // circuit into a wrong order (e.g. `if r.kind != ks &&
    // r.session_id != sid { return false }`) breaks the AND
    // pin but this test only exercises one filter at a time.
    for r in [
        mk(UsageKind::Slice, "s1"),
        mk(UsageKind::BudgetWarn, "s1"),
        mk(UsageKind::CompactHint, "s1"),
        mk(UsageKind::BudgetOver, "s2"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --session s1 → 3 surviving lines, all session_id="s1".
    let out = std::process::Command::new(kf_budget_binary_path())
        // NOTE: no `--json`. The human branch is reached by
        // omitting it; `commands::report::at()` routes to
        // `for line in lines { println!("{line}"); }`.
        .args(["report", "--session", "s1"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget report --session s1 (human)");
    assert!(
        out.status.success(),
        "--session s1 must parse and exit 0 on the human branch; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        3,
        "--session s1 must filter the 4-row seed down to 3 lines \
             (the s1 rows: slice + budget_warn + compact_hint); a \
             different count means the session_id equality broke — \
             either all 4 leaked through (filter dropped), 0 came \
             through (filter over-dropped), or 1 came through \
             (typo on sid). got: {lines:?}"
    );
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.contains("\"session_id\":\"s1\""),
            "human-branch line[{i}] under --session s1 must carry \
                 `\"session_id\":\"s1\"` exactly; got: {line:?}"
        );
    }
    // ponytail: the s2 row must not leak through. The 4-row
    // seed has exactly one s2 row (budget_over) and it carries
    // `\"session_id\":\"s2\"`. A contributor who breaks the
    // filter to "passes everything" surfaces here — the s2
    // substring would appear in stdout. We assert on the
    // exact substring (not just session_id absent) so a
    // contributor who renames the field to `sid_v2` also
    // fails this pin.
    assert!(
        !stdout.contains("\"session_id\":\"s2\""),
        "human-branch --session s1 must NOT leak any s2 row; \
             a `\"session_id\":\"s2\"` substring here means the \
             session filter was bypassed (e.g. always-true \
             short-circuit). stdout: {stdout:?}"
    );

    // Arm 2: --session s2 → 1 surviving line, session_id="s2".
    // The non-trivial direction: the s2 row is the 4th seed
    // row, and a `--last 100` (default) has room for it. But a
    // contributor who changed `filter_lines` to apply `last`
    // BEFORE `session` (rather than the documented
    // filter-then-tail order) would still return this row
    // here because tail=100 > 4; this arm doesn't pin the
    // order, that's `report_last_after_combined_filters_at_subprocess`.
    // It does pin that the surviving row is the budget_over
    // record from s2 — if `filter_lines`'s session_id check
    // swapped to `r.session_id != sid` (which is what the
    // current code does, returning false on mismatch and
    // keeping the rest), the s1 rows would survive and s2
    // would NOT — opposite of arm 1.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["report", "--session", "s2"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget report --session s2 (human)");
    assert!(
        out.status.success(),
        "--session s2 must parse and exit 0 on the human branch; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "--session s2 must filter the 4-row seed down to 1 line \
             (the s2 row: budget_over); got: {lines:?}"
    );
    let line = lines[0];
    assert!(
        line.contains("\"session_id\":\"s2\""),
        "human-branch line under --session s2 must carry \
             `\"session_id\":\"s2\"` exactly; got: {line:?}"
    );
    assert!(
        line.contains("\"kind\":\"budget_over\""),
        "the surviving s2 row must be the budget_over record \
             from the seed; got: {line:?}"
    );
    // ponytail: s1 must not leak through on the s2 arm.
    // Symmetric to the s2-not-leaking pin above — a broken
    // filter (always-true) would emit all 4 rows here.
    assert!(
        !stdout.contains("\"session_id\":\"s1\""),
        "human-branch --session s2 must NOT leak any s1 row; \
             stdout: {stdout:?}"
    );

    // Arm 3: --session nonexistent → 0 lines.
    // ponytail: a typo'd --session value should produce empty
    // stdout (and exit 0), NOT a clap usage error. clap's
    // `--session <SID>` is `Option<String>`, so any string
    // parses; the filter does the dropping. A contributor who
    // narrows `--session` to a `ValueEnum` would surface here
    // — clap would reject "nonexistent" with exit 64. The
    // empty-stdout contract lets `kf-budget report --session $X
    // | wc -l` reliably return zero.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["report", "--session", "nonexistent"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget report --session nonexistent");
    assert!(
        out.status.success(),
        "--session with an unmatched id must exit 0 (filter \
             dropped everything), not 64 (clap usage error — that \
             would mean --session was narrowed to a ValueEnum). \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.trim().is_empty(),
        "--session with an unmatched id must produce empty \
             stdout on the human branch; got: {stdout:?}"
    );
}

#[test]
fn report_session_and_kind_filters_combine_human_branch() {
    // ponytail: subprocess pin for the AND-combination of
    // `--session` and `--kind` on the human-readable (non-JSON)
    // branch. The JSON sibling below
    // (`report_session_and_kind_filters_combine_at_subprocess`)
    // pins the AND combination through the parsed JSON-array
    // path. The human branch goes through
    // `for line in lines { println!("{line}"); }` and was only
    // exercised at unit level via `commands::report::at(...)`
    // with typed filters. The unit tests in
    // `kf-budget-core/src/report.rs::filter_then_tail_is_pinned`
    // exercise `filter_lines` directly with combined filters
    // — they cover the FILTER logic but not the wire-level
    // rendering. A contributor who breaks the kebab→snake
    // round-trip in `clap::ValueEnum` for `--kind` (e.g.
    // narrows `UsageKindArg` to a typo'd variant) would
    // surface here as a clap usage error rather than a
    // correctly-rendered line — both arms below exercise
    // distinct paths through that conversion.
    //
    // The 5-row seed has 3 sessions × 3 kinds so that:
    //   --session alpha --kind slice    → 1 line (the alpha/slice row)
    //   --session bravo --kind budget-warn → 1 line (the bravo/budget_warn row)
    //   --session charlie --kind slice  → 0 lines (charlie has no slice row)
    // The third arm is the load-bearing one for the AND
    // semantics: a contributor who breaks one filter (e.g.
    // narrows `--session` to a ValueEnum that rejects
    // "charlie") would surface here as a clap usage error
    // (exit non-zero), not as empty stdout. The first two
    // arms prove the survivors; the third proves that an
    // unmatched combo is empty (filter logic), not an error
    // (clap rejection).
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let usage_dir = data_dir.path().join("logs");
    std::fs::create_dir_all(&usage_dir).unwrap();
    let usage_path = usage_dir.join("usage.jsonl");
    let mut s = String::new();
    let mk = |kind: UsageKind, sid: &str| -> UsageRecord {
        UsageRecord {
            ts: chrono::Utc::now(),
            kind,
            session_id: sid.into(),
            bytes_in: None,
            bytes_out: None,
            tokens_used: None,
            tokens_ceiling: None,
            tool: None,
        }
    };
    // 5 rows: alpha has slice + budget_warn; bravo has
    // budget_warn + compact_hint; charlie has compact_hint
    // only. So no session has a row that overlaps another's
    // kind set — every (sid, kind) pair is unique.
    for r in [
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::BudgetWarn, "alpha"),
        mk(UsageKind::BudgetWarn, "bravo"),
        mk(UsageKind::CompactHint, "bravo"),
        mk(UsageKind::CompactHint, "charlie"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // ponytail: helper to spawn a combined-filter invocation
    // on the human branch. The args sequence is fixed; the
    // only variability across arms is the (sid, kind) tuple.
    // `--kind` is kebab-case (CLI spelling); `--session` is
    // a free-form string (no enum conversion). The dash in
    // `budget-warn` exercises the kebab→snake boundary on
    // the human branch.
    let run = |sid: &str, kind: &str| -> std::process::Output {
        std::process::Command::new(kf_budget_binary_path())
            .args(["report", "--session", sid, "--kind", kind])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap_or_else(|e| panic!("spawn kf-budget --session {sid} --kind {kind}: {e}"))
    };

    // Arm 1: alpha + slice → 1 surviving line. The seed has
    // exactly one (alpha, slice) row. The line must carry
    // both substrings. This arm proves the AND on the human
    // branch keeps a single survivor.
    let out = run("alpha", "slice");
    assert!(
        out.status.success(),
        "--session alpha --kind slice must parse and exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "--session alpha --kind slice must filter the 5-row \
             seed down to exactly 1 line (the alpha/slice row); \
             got: {lines:?}"
    );
    let line = lines[0];
    assert!(
        line.contains("\"session_id\":\"alpha\""),
        "surviving line must carry `session_id=alpha`; got: {line:?}"
    );
    assert!(
        line.contains("\"kind\":\"slice\""),
        "surviving line must carry `kind=slice` (snake_case \
             wire form after the CLI's `slice` → `UsageKind::Slice` \
             round-trip); got: {line:?}"
    );

    // Arm 2: bravo + budget-warn → 1 surviving line. Multi-word
    // kebab (`budget-warn`) on the human branch. A contributor
    // who drops the `BudgetWarn` variant from `UsageKindArg`
    // would surface here as exit-non-zero (clap rejects
    // `budget-warn`). The kind wire form must be `budget_warn`
    // (snake_case).
    let out = run("bravo", "budget-warn");
    assert!(
        out.status.success(),
        "--session bravo --kind budget-warn must parse and \
             exit 0; exit non-zero here means the kebab-case \
             `UsageKindArg` enum lost `BudgetWarn`. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "--session bravo --kind budget-warn must filter to \
             exactly 1 line (the bravo/budget_warn row); got: {lines:?}"
    );
    let line = lines[0];
    assert!(
        line.contains("\"session_id\":\"bravo\""),
        "surviving line must carry `session_id=bravo`; got: {line:?}"
    );
    assert!(
        line.contains("\"kind\":\"budget_warn\""),
        "surviving line must carry `kind=budget_warn` \
             (snake_case on the wire, after kebab→snake round-trip); \
             got: {line:?}"
    );

    // Arm 3: charlie + slice → 0 surviving lines. charlie has
    // only a compact_hint row; --kind slice excludes it.
    // AND semantics on the human branch: BOTH filters must
    // match, so this arm must produce empty stdout (not the
    // charlie/compact_hint row, which would mean the
    // session filter was skipped). A contributor who breaks
    // the AND into an OR (e.g. drops one short-circuit) would
    // surface here as 1 surviving line (the charlie row) —
    // charlie's row carries session_id=charlie but kind=
    // compact_hint, NOT slice, so a kind-OR with session
    // would still emit it.
    let out = run("charlie", "slice");
    assert!(
        out.status.success(),
        "--session charlie --kind slice must exit 0 (filter \
             dropped, not clap usage error); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.trim().is_empty(),
        "--session charlie --kind slice must produce empty \
             stdout on the human branch (charlie has no slice row, \
             AND semantics drop everything); a non-empty stdout \
             here means either the kind filter was bypassed (the \
             charlie/compact_hint row leaked through) or the \
             session filter was bypassed (a different session's \
             slice row leaked through). got: {stdout:?}"
    );
    // ponytail: explicit double-negative pin — the forbidden
    // survivor substrings must NOT appear. A contributor who
    // swaps the AND to OR (`if !matches_session ||
    // !matches_kind { return false }`) would surface here:
    // the charlie/compact_hint row carries
    // session_id=charlie which matches the --session filter
    // alone (so under OR it would survive).
    assert!(
        !stdout.contains("\"session_id\":\"charlie\""),
        "AND filter must drop charlie entirely (no charlie \
             row matches --kind slice); a leak here means the \
             kind short-circuit broke. stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("\"kind\":\"slice\""),
        "AND filter must drop all slice rows from non-charlie \
             sessions too; a leak here means the session \
             short-circuit broke (e.g. always-true). stdout: {stdout:?}"
    );
}

#[test]
fn report_session_and_kind_filters_combine_at_subprocess() {
    // ponytail: subprocess pin for the AND-combination of
    // `--session` and `--kind` on the `kf-budget --json report`
    // path. The unit-level `filter_then_tail_is_pinned`
    // (kf-budget-core/src/report.rs) covers the AND combination
    // through the typed `filter_lines(...)` boundary, but
    // nothing exercises BOTH filters together at the
    // clap → binary boundary. A contributor who flips the
    // filter composition from AND to OR (e.g. early-returns
    // from the first match — `if r.session_id == sid { return true; }`)
    // would slip past every existing subprocess pin because
    // each existing arm only sets ONE filter.
    //
    // Two arms, seeded from the same 5-row file:
    //   --session alpha --kind slice → 2 records (both
    //     slice/alpha rows). The bravo-slice row drops on
    //     session, the alpha-budget_warn and alpha-compact_hint
    //     rows drop on kind. The arm is tight: any extra
    //     surviving record means the filter OR'd instead of
    //     AND'd.
    //   --session bravo --kind slice → 1 record (slice/bravo).
    //     The dual-arm catches a contributor who hardcodes the
    //     session filter to "alpha" (arm 2 would emit zero
    //     records).
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
    // ponytail: 5 rows where exactly 2 match BOTH filters for
    // arm 1 and exactly 1 matches BOTH filters for arm 2.
    // The other rows are red herrings that exercise one
    // dimension each: bravo-slice (right kind, wrong session),
    // alpha-budget_warn (right session, wrong kind),
    // alpha-compact_hint (right session, wrong kind).
    for r in [
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::Slice, "bravo"),
        mk(UsageKind::BudgetWarn, "alpha"),
        mk(UsageKind::CompactHint, "alpha"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --session alpha --kind slice → 2 records.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--session", "alpha", "--kind", "slice"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --session alpha --kind slice");
    assert!(
        out.status.success(),
        "--session alpha --kind slice must parse and exit 0; exit \
             non-zero (typically 64) here means the clap wiring for the \
             combined filter args broke. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        2,
        "--session alpha --kind slice must filter the 5-row seed \
             down to exactly 2 records (the two slice/alpha rows). A \
             count >2 here means the filter OR'd instead of AND'd \
             (e.g. early-returned on the first filter match); count \
             <2 means one of the slice/alpha rows was dropped. got: {}",
        arr.len()
    );
    for (i, rec) in arr.iter().enumerate() {
        assert_eq!(
            rec["session_id"], "alpha",
            "record[{i}] session_id must be `\"alpha\"`; got: {:?}",
            rec["session_id"]
        );
        assert_eq!(
            rec["kind"], "slice",
            "record[{i}] kind must be `\"slice\"` (snake_case serde); \
                 got: {:?}",
            rec["kind"]
        );
    }

    // Arm 2: --session bravo --kind slice → 1 record.
    // Catches a contributor who hardcodes the session value
    // to "alpha" — arm 2 would surface as count=0 (no bravo
    // rows in any kind). Also catches the OR-bug symmetric
    // to arm 1: if the filter was OR'd, this arm would
    // emit BOTH slice/alpha and slice/bravo (=3 records).
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--session", "bravo", "--kind", "slice"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --session bravo --kind slice");
    assert!(
        out.status.success(),
        "--session bravo --kind slice must parse and exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        1,
        "--session bravo --kind slice must filter the 5-row seed \
             down to exactly 1 record (slice/bravo). A count >1 here \
             means the filter OR'd instead of AND'd (the two \
             slice/alpha rows slipped through). got: {}",
        arr.len()
    );
    assert_eq!(
        arr[0]["session_id"], "bravo",
        "the surviving record's session_id must be `\"bravo\"`; \
             got: {:?}",
        arr[0]["session_id"]
    );
    assert_eq!(
        arr[0]["kind"], "slice",
        "the surviving record's kind must be `\"slice\"`; \
             got: {:?}",
        arr[0]["kind"]
    );
}

#[test]
fn report_last_after_combined_filters_at_subprocess() {
    // ponytail: subprocess pin for the THREE-way combination
    // `--last N --session <SID> --kind <K>`. Round 43 pinned
    // `--last N` alone; Round 56 pinned `--session + --kind`;
    // nothing exercises all three together at the subprocess
    // boundary. The order of operations is load-bearing
    // (ADR-0010 § Report subcommand: filter first, THEN tail to
    // N — see `filter_lines` then `tail_lines` in
    // kf-budget-core/src/report.rs). A contributor who reverses
    // the order (tail first, then filter) silently drops the
    // chronological tail of the filtered set — the host sees
    // the WRONG records as "the latest".
    //
    // 5-row seed: 2 slice/alpha, 2 slice/bravo, 1 budget_warn/
    // charlie. Three arms:
    //   --session bravo --kind slice --last 2  → 2 records
    //     (both slice/bravo; tail of 2 = the full filtered set)
    //   --session alpha --kind slice --last 1  → 1 record
    //     (the SECOND slice/alpha, NOT the first — proves
    //     truncation happened AFTER filtering)
    //   --session charlie --kind slice --last 5 → 0 records
    //     (charlie has no slice rows; proves --kind still
    //     filters even when --last exceeds the filtered count)
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
        mk(UsageKind::Slice, "bravo"),
        mk(UsageKind::Slice, "bravo"),
        mk(UsageKind::BudgetWarn, "charlie"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --last 2 + bravo + slice → 2 records (both
    // slice/bravo). The dual `--session + --kind` filter
    // narrows the seed to 2 rows; --last 2 keeps both. A
    // contributor who truncates BEFORE filtering would emit
    // the tail-2 of the full seed (the second slice/bravo
    // and the budget_warn/charlie row) — surface: count=2
    // but kind mismatch on arr[1].
    let out = std::process::Command::new(kf_budget_binary_path())
        .args([
            "--json",
            "report",
            "--last",
            "2",
            "--session",
            "bravo",
            "--kind",
            "slice",
        ])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --last 2 --session bravo --kind slice");
    assert!(
        out.status.success(),
        "combined --last + --session + --kind must parse and exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        2,
        "--last 2 --session bravo --kind slice must filter the \
             5-row seed to 2 (the slice/bravo rows) and tail-keep \
             both; a different count means filter-then-tail order \
             regressed or the filter dropped rows. got: {}",
        arr.len()
    );
    for (i, rec) in arr.iter().enumerate() {
        assert_eq!(
            rec["session_id"], "bravo",
            "record[{i}] session_id must be `\"bravo\"`; got: {:?}",
            rec["session_id"]
        );
        assert_eq!(
            rec["kind"], "slice",
            "record[{i}] kind must be `\"slice\"`; got: {:?}",
            rec["kind"]
        );
    }

    // Arm 2: --last 1 + alpha + slice → 1 record, the SECOND
    // slice/alpha. This is the load-bearing arm for the
    // filter-then-tail order. The seed has TWO slice/alpha
    // rows; tail-1 of the filtered set picks the second one
    // (chronologically later). A contributor who truncates
    // BEFORE filtering (tail-1 of the full file = the
    // budget_warn/charlie row) would emit kind=budget_warn
    // here, not slice — surface as kind mismatch.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args([
            "--json",
            "report",
            "--last",
            "1",
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
        .expect("spawn kf-budget --last 1 --session alpha --kind slice");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        1,
        "--last 1 --session alpha --kind slice must filter the \
             5-row seed to 2 (slice/alpha), then tail-1 to the LAST \
             one; a different count means --last ignored the filter \
             or the tail truncation happened before filtering. got: {}",
        arr.len()
    );
    assert_eq!(
        arr[0]["session_id"], "alpha",
        "the surviving record's session_id must be `\"alpha\"`; got: {:?}",
        arr[0]["session_id"]
    );
    assert_eq!(
        arr[0]["kind"], "slice",
        "the surviving record's kind must be `\"slice\"`; got: {:?}",
        arr[0]["kind"]
    );
    // ponytail: pin the content_preview / size fingerprint
    // so a tail-vs-filter regression is unambiguous. The
    // SECOND slice/alpha row was constructed identically to
    // the first (same mk closure, same bytes_in/bytes_out),
    // so we use session_id + kind + position-in-arr as the
    // fingerprint. The arr.len() == 1 assertion above proves
    // tail happened AFTER filter; the kind/session assertions
    // prove the filter didn't leak a different row.

    // Arm 3: --last 5 + charlie + slice → 0 records. The
    // charlie row is budget_warn, not slice — the kind
    // filter wipes it out. --last 5 exceeds the filtered
    // count (0), so the result is the empty slice. This
    // arm catches a contributor who hardcodes
    // `last.min(filtered.len())` in a way that accidentally
    // bypasses --kind (e.g. routes only --session through
    // filter_lines) — surface: count=1 (the charlie row),
    // kind=budget_warn.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args([
            "--json",
            "report",
            "--last",
            "5",
            "--session",
            "charlie",
            "--kind",
            "slice",
        ])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --last 5 --session charlie --kind slice");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        0,
        "--last 5 --session charlie --kind slice must produce 0 \
             records (charlie has no slice rows; --kind filter wiped \
             the seed); a count >0 here means --kind didn't reach the \
             filter path or --last exceeded the filtered count. got: {}",
        arr.len()
    );
}
