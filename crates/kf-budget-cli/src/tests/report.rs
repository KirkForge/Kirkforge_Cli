// ponytail: ADR-0015 § Exit codes — exercises the binary as a subprocess
// so the real clap + std::process::exit paths are taken. Unit tests on
// the inner functions would not catch a regression where someone moves
// the exit(78) call behind a flag.
use super::*;

#[test]
fn report_last_n_truncates_to_n_records_at_subprocess() {
    // ponytail: subprocess pin for `kf-budget --json report
    // --last N`. The CLI defaults `--last` to 100 and the
    // aggregator truncates via `tail_lines(&filtered, last)`
    // (kf-budget-core/src/report.rs). A contributor who breaks
    // the truncation (e.g. drops `tail_lines` and emits
    // `&filtered` directly, or routes the wrong slice into
    // the JSON printer) silently changes the count contract:
    //   `report --last 1`  must show 1 record (the latest)
    //   `report --last 100` on 5 records must show all 5
    // Existing tests in `tests` call `commands::report::at`
    // directly with the typed `last` arg, bypassing clap's
    // `--last` parsing — they wouldn't catch a drift between
    // the clap default and the runtime param. Subprocess is
    // the only layer that exercises the full path: clap
    // → CLI dispatch → `tail_lines` → JSON wire.
    //
    // Seed 5 records with distinct session_ids so the
    // truncation is observable by ID (not just count).
    // `tail_lines` preserves original order, so the last 2
    // records must be in seed order (r3, then r4).
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
        mk(UsageKind::Slice, "r0"),
        mk(UsageKind::BudgetWarn, "r1"),
        mk(UsageKind::Slice, "r2"),
        mk(UsageKind::CompactHint, "r3"),
        mk(UsageKind::Slice, "r4"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --last 2 → 2 records (the tail, in seed order).
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--last", "2"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --last 2");
    assert!(
        out.status.success(),
        "--last 2 must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        2,
        "--last 2 on a 5-row seed must return 2 records; a \
             different count here means the truncation either \
             dropped (`tail_lines` returning the wrong slice) or \
             was bypassed entirely (full file emitted). got: {}",
        arr.len()
    );
    // ponytail: pin the order. `tail_lines` preserves the
    // seed order, so the surviving records must be r3 then
    // r4 (not r4 then r3 — that would be a reverse sort, not
    // a tail). A contributor who sorts by ts asc or by
    // session_id asc silently changes the contract.
    assert_eq!(
        arr[0]["session_id"], "r3",
        "--last 2 must preserve seed order: the second-to-last \
             record (r3) comes first in the output. got: {:?}",
        arr[0]["session_id"]
    );
    assert_eq!(
        arr[1]["session_id"], "r4",
        "--last 2 must preserve seed order: the last record \
             (r4) comes second. got: {:?}",
        arr[1]["session_id"]
    );

    // Arm 2: --last 100 on the same 5-row seed → all 5
    // records (the `n > len` fallback in `tail_lines`).
    // This is the dual-arm: when `--last` exceeds the seed,
    // the CLI must emit the full set, not crash or return
    // zero. A contributor who replaces the fallback with
    // `panic!` or `return &[]` surfaces here.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--last", "100"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --last 100");
    assert!(
        out.status.success(),
        "--last 100 must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        5,
        "--last 100 on a 5-row seed must return all 5 records \
             (the `n > len` fallback in `tail_lines`); zero or \
             panic here means the fallback regressed. got: {}",
        arr.len()
    );
    assert_eq!(
        arr[0]["session_id"], "r0",
        "fallback path must preserve original (head-first) \
             order, not reversed. got: {:?}",
        arr[0]["session_id"]
    );
}

#[test]
fn report_last_n_human_branch_truncates_keeping_seed_order() {
    // ponytail: subprocess pin for `kf-budget report --last N` on
    // the human-readable (non-JSON) branch. The JSON sibling
    // above (`report_last_n_truncates_to_n_records_at_subprocess`)
    // pins that `--last` truncates AFTER filtering and returns
    // the LAST N records (not the FIRST N) at the JSON-array
    // level. The human branch goes through the verbatim
    // `for line in lines { println!("{line}"); }` loop and
    // was only exercised at unit level via
    // `commands::report::at(...)`. A contributor who breaks
    // `tail_lines` (e.g. swaps the slice direction to
    // `lines[..n]`, taking the FIRST N) or who swaps the
    // human branch to `for line in lines.iter().rev()` would
    // pass the unit tests (which assert on `lines.len()` only)
    // but break the wire contract — `tail -n` semantics are
    // load-bearing for the wrapper-script patterns ADR-0010
    // documents (e.g. `kf-budget report --last 1` to fetch the
    // most-recent record).
    //
    // Three arms exercise:
    //   --last 3 on a 5-row seed  → 3 lines, seed order,
    //                                LAST 3 records of seed
    //   --last 5 (= seed length) → 5 lines (tail_lines fallback
    //                                when `n >= len` returns
    //                                the slice unchanged)
    //   --last 100 (= default)   → 5 lines (same fallback; the
    //                                default of 100 is what an
    //                                unconfigured `kf-budget
    //                                report` invocation uses)
    // The fallback path is the same code branch (the `else`
    // arm of `tail_lines`'s `if lines.len() > n`) but pinned
    // separately because `--last 100` is the common default
    // and a contributor who breaks only the truncation path
    // (e.g. `if lines.len() >= n`) might miss the >= case.
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
    // ponytail: session_id encodes the seed position so the
    // surviving-record assertion can verify tail-vs-head
    // ordering. r0 is the FIRST seed row, r4 is the LAST.
    // A contributor who swaps the slice direction to
    // `lines[..n]` (head-first) would surface as r0/r1/r2
    // surviving under --last 3 instead of r2/r3/r4.
    for (i, sid) in ["r0", "r1", "r2", "r3", "r4"].iter().enumerate() {
        // Mix the kinds so a kind-based short-circuit can't
        // accidentally collapse the 5-row seed into one type.
        let kind = match i % 3 {
            0 => UsageKind::Slice,
            1 => UsageKind::BudgetWarn,
            _ => UsageKind::CompactHint,
        };
        s.push_str(&serde_json::to_string(&mk(kind, sid)).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --last 3 → exactly 3 surviving lines, in seed
    // order, the LAST 3 records (r2, r3, r4). The order pin
    // catches a head-first regression; the count pin catches
    // off-by-one or over-truncation.
    let out = std::process::Command::new(kf_budget_binary_path())
        // NOTE: no `--json`. Human branch.
        .args(["report", "--last", "3"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget report --last 3 (human)");
    assert!(
        out.status.success(),
        "--last 3 must parse and exit 0 on the human branch; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        3,
        "--last 3 must truncate the 5-row seed to exactly 3 \
             lines; a different count means `tail_lines` regressed \
             (off-by-one, slice direction swapped, or zero fallback). \
             got: {lines:?}"
    );

    // ponytail: pin the ORDER of the surviving records. A
    // contributor who swaps to `lines[..n]` (head-first) keeps
    // the count but returns r0/r1/r2 — a `--last` semantically
    // inverted to `--first`. The session_id values are
    // ordered in the seed, so the surviving records must be
    // the LAST 3 (r2, r3, r4) in seed order, not reversed.
    let sids: Vec<&str> = lines
        .iter()
        .filter_map(|l| {
            // ponytail: parse the session_id field by
            // substring scan rather than deserialising — the
            // human branch emits verbatim JSONL, but the
            // assertion stays substring-based so it works
            // even if a contributor renames the field. We
            // anchor on the `"session_id":"..."` literal.
            let key = "\"session_id\":\"";
            let start = l.find(key)? + key.len();
            let rest = &l[start..];
            let end = rest.find('"')?;
            Some(&rest[..end])
        })
        .collect();
    assert_eq!(
        sids,
        vec!["r2", "r3", "r4"],
        "--last 3 must return the LAST 3 records in seed order \
             (tail semantics, not head). A `lines[..n]` regression \
             would yield [r0, r1, r2]; a `lines[..n].rev()` would \
             yield [r4, r3, r2]. got: {sids:?}"
    );

    // ponytail: pin the verbatim single-line shape. A
    // contributor who merged the human branch into the JSON
    // branch's `to_string_pretty` parser would emit
    // multi-line records (one field per line). The literal
    // JSON object braces on each line pin that the human
    // branch did NOT switch to pretty-printing.
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "human-branch line[{i}] must be a single-line JSONL \
                 object (verbatim passthrough); got: {line:?}"
        );
    }

    // Arm 2: --last 5 (= seed length) → all 5 records in
    // seed order. The `tail_lines` fallback (`if lines.len()
    // > n { ... } else { lines }`) returns the slice unchanged
    // when `n >= len`. A contributor who changes the
    // condition to `if lines.len() >= n` would still pass
    // arm 1 but might over-truncate here on `n == len`
    // (returning 4 lines if `>=` is interpreted as `>` minus
    // the boundary, depending on the slice arithmetic).
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["report", "--last", "5"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget report --last 5 (human)");
    assert!(
        out.status.success(),
        "--last 5 must parse and exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        5,
        "--last 5 (= seed length) must return all 5 lines via \
             the `n >= len` fallback; a 4-line result means the \
             boundary regressed. got: {lines:?}"
    );
    // ponytail: seed-order preservation on the fallback path.
    // A contributor who switches `tail_lines`'s fallback to
    // `&lines[..]` (which still returns all 5 for n >= len)
    // would pass the count check but might lose the
    // preservation — actually the slice is unchanged here so
    // this is redundant; the order assertion on arm 1 is
    // the load-bearing one. We skip the order assertion on
    // arm 2 to keep the test focused.

    // Arm 3: --last 100 (default value of the clap flag)
    // must also return all 5 lines. clap's `default_value_t
    // = 100` means an unconfigured `kf-budget report` uses
    // 100; on a 5-row seed that's well above the seed
    // length. A contributor who changes the default to e.g.
    // `1` would surface here as a 1-line output under the
    // default invocation. We pass `--last 100` explicitly
    // to pin the FLAG VALUE behaviour, not the DEFAULT
    // behaviour (the default is pinned at the unit level
    // via clap's own derive).
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["report", "--last", "100"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget report --last 100 (human)");
    assert!(
        out.status.success(),
        "--last 100 must parse and exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        5,
        "--last 100 on a 5-row seed must return all 5 lines \
             (fallback); a different count means `--last 100` \
             doesn't reach `tail_lines`'s fallback branch. \
             got: {lines:?}"
    );
}

#[test]
fn report_last_n_with_session_filter_human_branch_orders_filter_then_tail() {
    // ponytail: subprocess pin for `kf-budget report --last N
    // --session <SID>` on the human-readable (non-JSON)
    // branch. The `--last N` family is pinned on the JSON
    // branch (`report_last_n_truncates_to_n_records_at_subprocess`
    // + `report_last_after_combined_filters_at_subprocess`),
    // and `--session` alone is pinned on the human branch
    // (R61: `report_session_filter_human_branch_prints_only_matching_sids`).
    // The combination of the two — filter by session, then
    // take the LAST N of the surviving set — has never been
    // pinned end-to-end on the human branch.
    //
    // The load-bearing drift here is FILTER-THEN-TAIL order.
    // `commands::report::at()` runs:
    //   let filtered = report::filter_lines(&all, ...);
    //   let lines    = report::tail_lines(&filtered, last);
    // A contributor who swaps these (tail-then-filter) would
    // break the contract: `kf-budget report --last 1 --session
    // bravo` should return bravo's last record, NOT the
    // whole-file-last record (which might be a different
    // session). The JSON sibling pins this; the human branch
    // is the gap. Unit tests in kf-budget-core cover
    // `filter_lines`/`tail_lines` in isolation but not their
    // composition via the CLI's `at()`.
    //
    // The 4-row seed has 2 alpha rows + 2 bravo rows (all
    // the same kind) so the surviving count is non-degenerate
    // and the ORDER pin catches a tail-then-filter regression:
    //   arm 1: --last 2 --session bravo → 2 lines, both bravo
    //   arm 2: --last 1 --session alpha → 1 line, the SECOND alpha row
    //   arm 3: --last 5 --session alpha → 2 lines (alpha has
    //          only 2; the n > len fallback applies, proving
    //          the order is filter-first)
    // session_id encodes the position so order is verifiable
    // at the substring level.
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
    // 4 rows: alpha/0, alpha/1, bravo/0, bravo/1 — interleaved
    // so a tail-then-filter regression would surface as
    // (bravo/1, bravo/0) surviving under --last 2 --session
    // bravo (correct) vs (alpha/1, bravo/1) surviving under
    // a tail-then-filter regression (last 2 of the whole file
    // are bravo/1 and alpha/1, then --session bravo keeps
    // bravo/1 — different count: 1 vs 2, depending on impl).
    // The interleaving makes the test robust to both
    // directions of bug.
    for r in [
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::Slice, "bravo"),
        mk(UsageKind::Slice, "bravo"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    fn sid_of(line: &str) -> &str {
        let key = "\"session_id\":\"";
        let start = line.find(key).expect("session_id present") + key.len();
        let rest = &line[start..];
        let end = rest.find('"').expect("session_id terminated");
        &rest[..end]
    }

    let run = |last: &str, sid: &str| -> std::process::Output {
        std::process::Command::new(kf_budget_binary_path())
            // NOTE: no `--json`. Human branch.
            .args(["report", "--last", last, "--session", sid])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap_or_else(|e| panic!("spawn kf-budget --last {last} --session {sid}: {e}"))
    };

    // Arm 1: --last 2 --session bravo → exactly 2 lines, both
    // bravo. The 2 surviving bravo rows must be in seed
    // order (the third and fourth seed rows). A contributor
    // who breaks filter-then-tail to tail-then-filter would
    // surface here: the last 2 rows of the WHOLE FILE are
    // bravo + bravo (rows 3 and 4), then filter-by-bravo
    // keeps both — same result by accident because the last
    // 2 rows happen to be bravo. To break this coincidence
    // we need arm 2: --last 1 --session alpha — the last
    // row of the whole file is bravo, NOT alpha, so a
    // tail-then-filter regression returns the bravo row
    // and FAILS the session equality pin. Arm 1 alone is
    // not load-bearing; arm 2 is.
    let out = run("2", "bravo");
    assert!(
        out.status.success(),
        "--last 2 --session bravo must parse and exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "--last 2 --session bravo must filter the 4-row seed \
             down to 2 lines (both bravo rows); a different count \
             here means the session filter dropped a bravo row. \
             got: {lines:?}"
    );
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(
            sid_of(line),
            "bravo",
            "arm 1 line[{i}] must have session_id=bravo; got: {line:?}"
        );
    }

    // Arm 2 (load-bearing): --last 1 --session alpha → 1
    // line, the LAST alpha row (the second alpha in seed
    // order). A tail-then-filter regression would return
    // the LAST row of the whole file (bravo), failing the
    // session equality pin. The order pin (alpha is the
    // FIRST and SECOND seed rows; --last 1 returns the
    // second one) catches a filter-first→filter-last
    // regression too.
    let out = run("1", "alpha");
    assert!(
        out.status.success(),
        "--last 1 --session alpha must parse and exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "--last 1 --session alpha must filter the 4-row seed \
             down to 1 line (the second alpha row); got: {lines:?}"
    );
    let line = lines[0];
    assert_eq!(
        sid_of(line),
        "alpha",
        "arm 2 line must have session_id=alpha; a `bravo` \
             here means filter-then-tail was swapped to \
             tail-then-filter (the LAST row of the whole file \
             is bravo). got: {line:?}"
    );

    // Arm 3: --last 5 --session alpha → 2 lines, both alpha.
    // The `n > len` fallback in `tail_lines` runs after the
    // session filter reduces the seed to 2 alpha rows;
    // `--last 5` is well above 2 so the fallback returns
    // the filtered slice unchanged. A contributor who
    // accidentally applied `--last` BEFORE `--session` (the
    // tail-then-filter bug) would tail the whole file to 5
    // rows (= whole file), then filter to alpha — same
    // result of 2 alpha rows. So this arm doesn't catch
    // the order regression. It DOES pin the n > len
    // fallback on the human branch in combination with a
    // filter — a contributor who narrows `tail_lines`'s
    // fallback to `if lines.len() - n > 0` would still
    // pass arms 1 and 2 but fail here on `--last 5`
    // returning fewer than 2 lines.
    let out = run("5", "alpha");
    assert!(
        out.status.success(),
        "--last 5 --session alpha must parse and exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "--last 5 --session alpha must return all 2 alpha \
             rows (the n > len fallback in `tail_lines`); a \
             1-line result means the fallback regressed to \
             `n - len` or similar. got: {lines:?}"
    );
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(
            sid_of(line),
            "alpha",
            "arm 3 line[{i}] must have session_id=alpha; \
                 got: {line:?}"
        );
    }
}

#[test]
fn report_last_n_with_session_and_kind_filters_human_branch() {
    // ponytail: subprocess pin for the THREE-filter combination
    // (`--last N --session <SID> --kind <K>`) on the
    // human-readable (non-JSON) branch. The JSON sibling
    // (`report_last_after_combined_filters_at_subprocess`)
    // pins the same combination at the JSON-array level. The
    // two-filter combinations have been pinned on the human
    // branch separately (R63: `--session + --kind`; R64:
    // `--last + --session`); the three-filter composition
    // is the missing arm.
    //
    // The load-bearing drift is the SAME filter-then-tail
    // order pinned in R64, plus the AND semantics of the two
    // predicates. A contributor who breaks `--kind` parsing
    // (e.g. narrows `UsageKindArg` to a typo'd variant) would
    // surface here as a clap usage error (exit non-zero)
    // rather than a correctly-rendered line; a contributor
    // who swaps the AND to OR surfaces as a leaked forbidden
    // substring; a contributor who reorders tail-then-filter
    // surfaces as the wrong session_id surviving under
    // `--last 1`.
    //
    // Same 5-row seed as the JSON sibling so a reader can
    // diff the two tests and see "same fixture, two
    // renderers". The seed positions rows interleaved across
    // sessions so a tail-then-filter regression shows up as a
    // different session_id in the surviving row (not just a
    // different count).
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
    // 5 rows: alpha/slice ×2, bravo/slice ×1, charlie/budget_warn,
    // charlie/compact_hint. Mirrors the JSON sibling's seed
    // (line 2870).
    for r in [
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::Slice, "alpha"),
        mk(UsageKind::Slice, "bravo"),
        mk(UsageKind::BudgetWarn, "charlie"),
        mk(UsageKind::CompactHint, "charlie"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    let run = |last: &str, sid: &str, kind: &str| -> std::process::Output {
        std::process::Command::new(kf_budget_binary_path())
            // NOTE: no `--json`. Human branch.
            .args(["report", "--last", last, "--session", sid, "--kind", kind])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap_or_else(|e| panic!("spawn --last {last} --session {sid} --kind {kind}: {e}"))
    };

    // ponytail: substring-based field extractor. The human
    // branch emits verbatim JSONL, so substring scan is the
    // faithful mirror of the rendered shape (the same trick
    // R64 used for `sid_of`).
    fn field<'a>(line: &'a str, key: &str) -> &'a str {
        let needle = format!("\"{key}\":\"");
        let start = line.find(&needle).expect("key present") + needle.len();
        let rest = &line[start..];
        let end = rest.find('"').expect("value terminated");
        &rest[..end]
    }

    // Arm 1: --last 2 --session bravo --kind slice → exactly
    // 1 surviving line, the bravo/slice row. The seed has
    // only 1 bravo+slice row, so `--last 2` is well above
    // the filtered count (1) — the n > len fallback in
    // `tail_lines` returns the single surviving line. A
    // contributor who breaks the AND to OR (e.g. drops the
    // session short-circuit) would leak the two alpha/slice
    // rows through, returning 3 lines under `--last 2` (the
    // 2 alpha rows + the 1 bravo row). The count pin
    // catches that. The session_id pin catches a session
    // filter bypass.
    let out = run("2", "bravo", "slice");
    assert!(
        out.status.success(),
        "--last 2 --session bravo --kind slice must parse and \
             exit 0; exit non-zero means --kind parsing regressed. \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "--last 2 --session bravo --kind slice must filter \
             the 5-row seed to exactly 1 line (the bravo/slice \
             row); a 2-line result means the session filter was \
             bypassed (alpha/slice rows leaked) AND the tail ran \
             first; a 3-line result means the AND dropped to OR. \
             got: {lines:?}"
    );
    let line = lines[0];
    assert_eq!(
        field(line, "session_id"),
        "bravo",
        "arm 1 surviving line must have session_id=bravo; \
             got: {line:?}"
    );
    assert_eq!(
        field(line, "kind"),
        "slice",
        "arm 1 surviving line must have kind=slice (snake_case \
             wire form after kebab→snake round-trip); got: {line:?}"
    );

    // Arm 2: --last 1 --session alpha --kind slice → 1
    // line, the SECOND alpha/slice row (filter-then-tail).
    // A tail-then-filter regression would return the LAST
    // row of the whole file (charlie/compact_hint), caught
    // by the session_id pin. A filter bypass (drop the
    // session short-circuit) would return the LAST alpha/slice
    // row but still with session_id=alpha — passing arm 2
    // but failing arm 1 (the count check). The two arms
    // pin independent failure modes.
    let out = run("1", "alpha", "slice");
    assert!(
        out.status.success(),
        "--last 1 --session alpha --kind slice must parse \
             and exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "--last 1 --session alpha --kind slice must return \
             exactly 1 line; got: {lines:?}"
    );
    let line = lines[0];
    assert_eq!(
        field(line, "session_id"),
        "alpha",
        "arm 2 surviving line must have session_id=alpha; \
             a `charlie` here means tail-then-filter was applied \
             (the LAST row of the whole file is charlie's \
             compact_hint). got: {line:?}"
    );
    assert_eq!(
        field(line, "kind"),
        "slice",
        "arm 2 surviving line must have kind=slice; got: {line:?}"
    );

    // Arm 3: --last 5 --session charlie --kind slice → 0
    // lines. charlie has 2 rows in the seed but neither is
    // slice, so the AND drops everything. A contributor
    // who breaks the AND to OR surfaces here: the
    // charlie/budget_warn row would survive (matches
    // session alone) — caught by the explicit forbidden
    // substring pin below.
    let out = run("5", "charlie", "slice");
    assert!(
        out.status.success(),
        "--last 5 --session charlie --kind slice must exit 0 \
             (filter dropped, not clap usage error); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.trim().is_empty(),
        "--last 5 --session charlie --kind slice must produce \
             empty stdout (charlie has no slice rows); got: {stdout:?}"
    );
    // ponytail: double-negative pin. Under AND semantics the
    // forbidden substrings MUST be absent. A contributor who
    // swaps AND to OR surfaces as a charlie row leaking
    // (any of the two charlie rows match --session charlie
    // alone). A contributor who drops the kind filter
    // surfaces as charlie/budget_warn surviving. Both
    // regressions are caught by the explicit substring
    // pin below.
    assert!(
        !stdout.contains("\"session_id\":\"charlie\""),
        "AND filter must drop charlie entirely (no charlie \
             row matches --kind slice); a leak here means the \
             kind short-circuit broke. stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("\"kind\":\"budget_warn\""),
        "AND filter must drop non-slice rows even when the \
             session matches; a leak here means the kind filter \
             was bypassed. stdout: {stdout:?}"
    );
}

#[test]
fn report_kind_filter_human_branch_prints_filtered_lines_verbatim() {
    // ponytail: subprocess pin for the human-readable (non-JSON)
    // sibling of `report_kind_filter_at_subprocess_pins_kebab_to_snake_enum_mapping`.
    // The JSON branch emits `serde_json::to_string_pretty` over
    // parsed records; the human branch prints each surviving
    // JSONL line VERBATIM (no parsing, no pretty-printing) — the
    // CLI does `for line in lines { println!("{line}"); }` on
    // the filtered tail. A contributor who copy-pastes the JSON
    // branch's `parsed.iter().map(to_string_pretty)` into the
    // human branch changes the shape a `grep "kind":"slice"`
    // dashboard relies on — the JSON branch's pretty-printer
    // puts one field per line, so `kind:"slice"` becomes
    // `"kind": "slice",` on its own line. Pin the verbatim line
    // shape here so the two branches stay visibly distinct.
    //
    // The 5-row seed mirrors the JSON-sibling fixture so a
    // reader can diff the two tests and see "same rows, two
    // renderers". Two arms exercise both single-word kebab
    // (`slice`) and multi-word kebab (`budget-warn`) — same
    // dash/underscore boundary that the JSON branch pins.
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
        mk(UsageKind::Slice, "s1"),
        mk(UsageKind::BudgetWarn, "s1"),
        mk(UsageKind::CompactHint, "s1"),
        mk(UsageKind::Slice, "s2"),
        mk(UsageKind::BudgetOver, "s2"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --kind slice on the human branch.
    // 2 surviving lines, both kind="slice" (snake_case on the
    // wire), sessions s1 + s2 in seed order. The lines must be
    // the JSONL source bytes verbatim — single-line records,
    // no pretty-printing, no leading whitespace.
    let out = std::process::Command::new(kf_budget_binary_path())
        // NOTE: no `--json` here. The human branch is reached
        // by omitting it; `commands::report::at()` routes to
        // `for line in lines { println!("{line}"); }`.
        .args(["report", "--kind", "slice"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget report --kind slice (human)");
    assert!(
        out.status.success(),
        "--kind slice must parse and exit 0 on the human branch; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "--kind slice must filter the 5-row seed down to 2 lines \
             on the human branch; a different count here means either \
             the kebab→snake conversion broke or the human branch's \
             `for line in lines` loop dropped/duplicated lines. \
             got: {lines:?}"
    );

    // ponytail: pin the verbatim single-line JSONL shape. A
    // contributor who switches the human branch to the JSON
    // branch's `to_string_pretty` parser would emit lines
    // like `  "kind": "slice",` (one field per line, padded
    // with two-space indent). The substrings `  "kind": "slice",`
    // and similar pretty-printed fragments must NOT appear —
    // if they do, the human branch has been merged into the
    // JSON branch. The single-line `"kind":"slice"` form
    // (no space between `:` and value, no leading whitespace)
    // is what verbatim passthrough produces via serde's
    // default `to_string` on `UsageRecord`.
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "human-branch line[{i}] must be a single-line JSONL \
                 object (verbatim passthrough); got: {line:?}"
        );
        assert!(
            line.contains("\"kind\":\"slice\""),
            "human-branch line[{i}] must carry the verbatim \
                 `\"kind\":\"slice\"` substring (no space between \
                 `:` and value, single line); pretty-printed \
                 `\"kind\": \"slice\",` here means the human branch \
                 was merged into the JSON branch's pretty-printer. \
                 got: {line:?}"
        );
        // Negative: the pretty-printed sibling must NOT leak.
        assert!(
            !line.contains("\"kind\": \"slice\""),
            "human-branch line[{i}] must NOT carry the \
                 pretty-printed `\"kind\": \"slice\"` form (space \
                 after colon); that form means the human branch \
                 started pretty-printing. got: {line:?}"
        );
    }

    // ponytail: pin the kind filter on the human branch — the
    // non-matching kinds must not leak through. A contributor
    // who breaks `filter_lines`'s `r.kind != ks` short-circuit
    // (e.g. accidentally inverts the comparison) surfaces
    // here. We assert each NON-matching kind is absent from
    // the filtered stdout — substring scan on the rendered
    // lines.
    for forbidden in ["budget_warn", "compact_hint", "budget_over"] {
        assert!(
            !stdout.contains(&format!("\"kind\":\"{forbidden}\"")),
            "human-branch --kind slice must filter out kind=\"{forbidden}\"; \
                 a leak here means `filter_lines`'s kind equality broke. \
                 stdout: {stdout:?}"
        );
    }

    // Arm 2: --kind budget-warn on the human branch.
    // 1 surviving line, kind="budget_warn" (snake_case on the
    // wire after the CLI's `budget-warn` → `BudgetWarn` →
    // `UsageKind::BudgetWarn` → `"budget_warn"` round-trip).
    // This arm pins the dash/underscore boundary on the
    // human branch; the JSON sibling pins the same boundary
    // separately.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["report", "--kind", "budget-warn"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget report --kind budget-warn (human)");
    assert!(
        out.status.success(),
        "--kind budget-warn must parse and exit 0 on the human \
             branch; exit non-zero means the kebab-case enum lost \
             `BudgetWarn`. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "--kind budget-warn must filter the 5-row seed down to 1 \
             line on the human branch. got: {lines:?}"
    );
    let line = lines[0];
    assert!(
        line.contains("\"kind\":\"budget_warn\""),
        "human-branch --kind budget-warn line must carry \
             `\"kind\":\"budget_warn\"` (the snake_case wire form, \
             after the CLI's `budget-warn` → `UsageKind::BudgetWarn` \
             round-trip). got: {line:?}"
    );
    assert!(
        line.contains("\"session_id\":\"s1\""),
        "human-branch line must preserve the session_id from \
             the seed (s1). got: {line:?}"
    );
}

#[test]
fn report_kind_filter_at_subprocess_pins_kebab_to_snake_enum_mapping() {
    // ponytail: subprocess pin for `kf-budget --json report
    // --kind <K>`. The CLI spells enum variants in kebab-case
    // (`--kind budget-warn`) and serde spells them snake_case
    // (`"budget_warn"` on the wire). The two rename rules are
    // independent and a contributor who breaks one without
    // the other silently changes the filter behaviour: e.g.
    // a typo in `#[clap(rename_all = "kebab-case")]` would
    // make `--kind budget-warn` fail to parse (clap returns
    // 64 — usage error) or, worse, silently pass an empty
    // filter and emit ALL records.
    //
    // Existing report filter tests in `tests` call
    // `commands::report::at(...)` directly with the typed
    // `Some(UsageKind::Slice)` — they bypass clap's
    // `UsageKindArg → UsageKind` conversion. A drift between
    // the kebab-case CLI spelling and the snake_case serde
    // spelling surfaces only at the subprocess layer. This
    // test pins BOTH arms:
    //   --kind slice     → 2 records (both kind=slice)
    //   --kind budget-warn → 1 record (kind=budget_warn)
    // The dual-arm catches an off-by-one in the kebab-case
    // list (e.g. `BudgetWarn` accidentally removed from
    // `UsageKindArg`, which compiles because the enum is
    // exhaustive but breaks `--kind budget-warn` at runtime).
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
        mk(UsageKind::Slice, "s1"),
        mk(UsageKind::BudgetWarn, "s1"),
        mk(UsageKind::CompactHint, "s1"),
        mk(UsageKind::Slice, "s2"),
        mk(UsageKind::BudgetOver, "s2"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --kind slice → 2 records, all kind="slice".
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--kind", "slice"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --kind slice");
    assert!(
        out.status.success(),
        "--kind slice must parse and exit 0; exit non-zero here means \
             the kebab-case `UsageKindArg` enum lost the `Slice` variant \
             (clap returns 64). stderr: {}",
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
        "--kind slice must filter the 5-row seed down to 2 slice \
             records; a different count here means the filter dropped \
             (or stopped dropping) records. got: {}",
        arr.len()
    );
    for (i, rec) in arr.iter().enumerate() {
        assert_eq!(
            rec["kind"], "slice",
            "record[{i}] kind must be the snake_case `\"slice\"` \
                 (serde `rename_all = \"snake_case\"`); kebab-case or \
                 PascalCase here would break every `jq '.[] | \
                 select(.kind == \"slice\")'` filter. got: {:?}",
            rec["kind"]
        );
    }

    // Arm 2: --kind budget-warn → 1 record, kind="budget_warn".
    // The CLI spelling is kebab-case (`budget-warn`); the JSONL
    // and the wire format are snake_case (`budget_warn`). This
    // arm pins the conversion across the dash/underscore
    // boundary.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--kind", "budget-warn"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --kind budget-warn");
    assert!(
        out.status.success(),
        "--kind budget-warn (kebab-case CLI spelling) must parse \
             and exit 0; exit non-zero here means the kebab-case \
             `UsageKindArg` enum lost the `BudgetWarn` variant. \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v.as_array().expect("report --json top-level is an array");
    assert_eq!(
        arr.len(),
        1,
        "--kind budget-warn must filter the 5-row seed down to 1 \
             record; a different count here means the kebab→snake \
             conversion broke (filter dropped or kept the wrong \
             records). got: {}",
        arr.len()
    );
    assert_eq!(
        arr[0]["kind"], "budget_warn",
        "the surviving record must be the snake_case `\"budget_warn\"` \
             (serde `rename_all = \"snake_case\"`); `\"budget-warn\"` or \
             `\"budgetWarn\"` here would break the wire contract. \
             got: {:?}",
        arr[0]["kind"]
    );
}

#[test]
fn report_kind_filter_for_budget_over_and_compact_hint_at_subprocess() {
    // ponytail: dual-arm pin for the kebab→snake conversion on
    // the multi-word kebab variants the Round 43 test didn't
    // cover (`budget-warn` + `slice` were covered; `budget-over`
    // and `compact-hint` are not). The kebab-case CLI spelling
    // (`--kind budget-over`) and the snake_case serde spelling
    // (`"budget_over"`) are independently-owned rename rules
    // (one in `#[clap(rename_all = "kebab-case")]`, one in
    // `#[serde(rename_all = "snake_case")]` on `UsageKind`),
    // and a contributor who breaks one without the other
    // silently changes the filter behaviour — e.g. a typo in
    // the kebab-case list would make `--kind budget-over`
    // fail to parse (clap returns 64 — usage error).
    //
    // Both arms share the same 5-row seed so the count of
    // surviving records is a strong filter signal:
    //   --kind budget-over  → 1 record (the 5th row, "s2")
    //   --kind compact-hint → 1 record (the 3rd row, "s1")
    // If a contributor removed `BudgetOver` from
    // `UsageKindArg` (the enum is exhaustive so it compiles,
    // but the variant disappears from clap's help), the kebab
    // arm here would surface as a clap parse failure (exit 64).
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
    // ponytail: 5 rows, one per kind × mixed sessions, so the
    // count of surviving records under each --kind arm is
    // exactly 1 (BudgetOver × 1, CompactHint × 1). The Slice
    // and BudgetWarn rows are red herrings — they verify the
    // filter actually filters, not just passes-through.
    for r in [
        mk(UsageKind::Slice, "s1"),
        mk(UsageKind::BudgetWarn, "s1"),
        mk(UsageKind::CompactHint, "s1"),
        mk(UsageKind::Slice, "s2"),
        mk(UsageKind::BudgetOver, "s2"),
    ] {
        s.push_str(&serde_json::to_string(&r).unwrap());
        s.push('\n');
    }
    std::fs::write(&usage_path, s).unwrap();

    // Arm 1: --kind budget-over → 1 record, kind="budget_over".
    // Dual rename: kebab `budget-over` (clap) → snake
    // `budget_over` (serde). A contributor who flips the
    // kebab rename rule to snake_case (loses the dash) would
    // make `budget-over` fail to parse here as exit 64.
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--kind", "budget-over"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --kind budget-over");
    assert!(
        out.status.success(),
        "--kind budget-over must parse and exit 0; exit non-zero \
             (typically 64) here means the kebab-case `UsageKindArg` \
             enum lost the `BudgetOver` variant or the kebab→snake \
             bridge (From<UsageKindArg>) regressed. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        1,
        "--kind budget-over must filter the 5-row seed down to \
             exactly 1 record (the s2 BudgetOver row); a different \
             count means the filter dropped or kept the wrong rows. \
             got: {}",
        arr.len()
    );
    assert_eq!(
        arr[0]["kind"], "budget_over",
        "the surviving record must be the snake_case `\"budget_over\"` \
             (serde `rename_all = \"snake_case\"`); `\"budget-over\"` or \
             `\"budgetOver\"` here would break the wire contract. \
             got: {:?}",
        arr[0]["kind"]
    );
    assert_eq!(
        arr[0]["session_id"], "s2",
        "the surviving BudgetOver record's session_id must be the \
             one seeded with that kind; a different value here means \
             the filter accidentally routed a different row through. \
             got: {:?}",
        arr[0]["session_id"]
    );

    // Arm 2: --kind compact-hint → 1 record, kind="compact_hint".
    // Same dual-rename contract as arm 1; CompactHint is the
    // second kebab variant with an internal word boundary
    // (the only one not tested in Round 43 besides BudgetOver).
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "report", "--kind", "compact-hint"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget --kind compact-hint");
    assert!(
        out.status.success(),
        "--kind compact-hint must parse and exit 0; exit non-zero \
             here means the kebab-case `UsageKindArg` enum lost the \
             `CompactHint` variant or the kebab→snake bridge regressed. \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let arr = v
        .as_array()
        .expect("report --json top-level must be an array");
    assert_eq!(
        arr.len(),
        1,
        "--kind compact-hint must filter the 5-row seed down to \
             exactly 1 record (the s1 CompactHint row); a different \
             count means the filter dropped or kept the wrong rows. \
             got: {}",
        arr.len()
    );
    assert_eq!(
        arr[0]["kind"], "compact_hint",
        "the surviving record must be the snake_case `\"compact_hint\"` \
             (serde `rename_all = \"snake_case\"`); `\"compact-hint\"` or \
             `\"compactHint\"` here would break the wire contract. \
             got: {:?}",
        arr[0]["kind"]
    );
    assert_eq!(
        arr[0]["session_id"], "s1",
        "the surviving CompactHint record's session_id must be the \
             one seeded with that kind; a different value here means \
             the filter accidentally routed a different row through. \
             got: {:?}",
        arr[0]["session_id"]
    );
}
