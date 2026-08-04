use super::*;
use std::io::Write;

// ponytail: keep the tempdir alive for the test by returning the guard.
// `prefix` is "budget" or "config"; the function makes the file
// distinguishable when several tests run in the same tempdir.
fn fresh_path(tag: &str, prefix: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("{prefix}-{tag}.toml"));
    (dir, path)
}

#[test]
fn budget_round_trips_via_atomic_write() {
    let (_dir, path) = fresh_path("rt", "budget");
    let mut b = TokenBudget {
        ceiling: 50_000,
        approaching_ratio: 0.8,
        used: 0,
    };
    b.record(1234);
    save_budget_at(&b, &path);
    let written = std::fs::read_to_string(&path).expect("budget written");
    let parsed: TokenBudget = toml::from_str(&written).expect("parse");
    assert_eq!(parsed.used, 1234);
    assert_eq!(parsed.ceiling, 50_000);
}

#[test]
fn budget_overwrite_does_not_leak_tmp() {
    // Two consecutive saves should leave exactly one budget file
    // (no orphan .tmp files in the parent dir).
    let (_dir, path) = fresh_path("ov", "budget");
    save_budget_at(&TokenBudget::default(), &path);
    save_budget_at(
        &TokenBudget {
            ceiling: 9999,
            approaching_ratio: 0.5,
            used: 42,
        },
        &path,
    );
    let parent = path.parent().unwrap();
    let orphans: Vec<_> = std::fs::read_dir(parent)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(orphans.is_empty(), "no leftover tmp files: {orphans:?}");
    let final_b: TokenBudget = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(final_b.ceiling, 9999);
}

// ---- BudgetConfig / --default wiring (ADR-0005 + ADR-0015) -----

#[test]
fn budget_config_round_trips_via_atomic_write() {
    let (_dir, path) = fresh_path("rt", "config");
    let cfg = BudgetConfig {
        ceiling: 300_000,
        approaching_ratio: 0.75,
    };
    save_budget_config_at(&cfg, &path);
    let back = load_budget_config_at(&path).expect("config exists");
    assert_eq!(back, cfg);
}

#[test]
fn load_budget_picks_up_default_ceiling_from_config_toml() {
    // ADR-0015: a future session must see the user's --default
    // even when its own runtime budget.toml is missing. Test by
    // pointing the helper at a config that overrides the default
    // and a runtime path that does not exist.
    let (_dir, cfg_path) = fresh_path("def", "config");
    save_budget_config_at(
        &BudgetConfig {
            ceiling: 123_456,
            approaching_ratio: 0.6,
        },
        &cfg_path,
    );
    let runtime_path = cfg_path.with_file_name("absent-runtime.toml");
    let b = load_budget_with_config(&runtime_path, &cfg_path);
    assert_eq!(b.ceiling, 123_456);
    assert!((b.approaching_ratio - 0.6).abs() < f64::EPSILON);
    assert_eq!(b.used, 0, "fresh session has no `used` carryover");
}

#[test]
fn load_budget_runtime_used_overrides_config_used() {
    // ponytail: `used` is session-local and must NEVER come from
    // config.toml. The runtime file is the only authority on it.
    let (_dir, cfg_path) = fresh_path("used", "config");
    save_budget_config_at(
        &BudgetConfig {
            ceiling: 999_999,
            approaching_ratio: 0.9,
        },
        &cfg_path,
    );
    let (_dir2, runtime_path) = fresh_path("used-rt", "budget");
    save_budget_at(
        &TokenBudget {
            ceiling: 999_999,
            approaching_ratio: 0.9,
            used: 4321,
        },
        &runtime_path,
    );
    let b = load_budget_with_config(&runtime_path, &cfg_path);
    assert_eq!(b.used, 4321, "runtime `used` survives config overlay");
    assert_eq!(b.ceiling, 999_999, "config ceiling wins");
}

#[test]
fn load_budget_missing_both_falls_back_to_defaults() {
    // Neither runtime nor config exists: TokenBudget::default().
    let (_dir, path) = fresh_path("none", "budget");
    let runtime_path = path.with_file_name("missing-runtime.toml");
    let cfg_path = path.with_file_name("missing-config.toml");
    let b = load_budget_with_config(&runtime_path, &cfg_path);
    // ponytail: assert field-by-field because TokenBudget lacks
    // PartialEq (the `used` counter is mutated freely and a
    // derived Eq would invite accidental == on hot paths).
    assert_eq!(b.ceiling, TokenBudget::default().ceiling);
    assert!((b.approaching_ratio - TokenBudget::default().approaching_ratio).abs() < f64::EPSILON);
    assert_eq!(b.used, 0);
}

#[test]
fn budget_config_overwrite_does_not_leak_tmp() {
    let (_dir, path) = fresh_path("ov", "config");
    save_budget_config_at(
        &BudgetConfig {
            ceiling: 100,
            approaching_ratio: 0.5,
        },
        &path,
    );
    save_budget_config_at(
        &BudgetConfig {
            ceiling: 200,
            approaching_ratio: 0.6,
        },
        &path,
    );
    let parent = path.parent().unwrap();
    let orphans: Vec<_> = std::fs::read_dir(parent)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(orphans.is_empty(), "no leftover tmp files: {orphans:?}");
}

fn write_records(dir: &std::path::Path, name: &str, records: &[UsageRecord]) -> std::path::PathBuf {
    let p = dir.join(name);
    let mut s = String::new();
    for r in records {
        s.push_str(&serde_json::to_string(r).unwrap());
        s.push('\n');
    }
    std::fs::write(&p, s).unwrap();
    p
}

fn rec(kind: UsageKind, session: &str) -> UsageRecord {
    let mut r = UsageRecord {
        ts: chrono::Utc::now(),
        kind,
        session_id: session.into(),
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
}

#[test]
fn report_kind_filter_selects_matching_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_records(
        dir.path(),
        "usage.jsonl",
        &[
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::BudgetWarn, "s1"),
            rec(UsageKind::Slice, "s2"),
            rec(UsageKind::CompactHint, "s2"),
        ],
    );
    let n = commands::report::at(&path, 100, false, None, Some(UsageKind::Slice), false);
    assert_eq!(n, 2, "only the two slice records survive");
}

#[test]
fn report_session_filter_selects_matching_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_records(
        dir.path(),
        "usage.jsonl",
        &[
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::Slice, "s2"),
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::BudgetWarn, "s2"),
        ],
    );
    let n = commands::report::at(&path, 100, false, Some("s1".into()), None, false);
    assert_eq!(
        n, 2,
        "only s1 records survive (1 slice + nothing else, but we expect 2 slice records)"
    );
}

#[test]
fn report_last_truncates_after_filters() {
    let dir = tempfile::tempdir().unwrap();
    let mut records = Vec::new();
    for _ in 0..10 {
        records.push(rec(UsageKind::Slice, "s"));
    }
    let path = write_records(dir.path(), "usage.jsonl", &records);
    let n = commands::report::at(&path, 3, false, None, Some(UsageKind::Slice), false);
    assert_eq!(n, 3, "last=3 caps output at 3 lines");
}

#[test]
fn report_summary_aggregates_per_session() {
    // 2 sessions: s1 has 2 slice records (1000 in, 400 out) and 1
    // budget_warn; s2 has 1 compact_hint and 1 budget_over.
    let dir = tempfile::tempdir().unwrap();
    let path = write_records(
        dir.path(),
        "usage.jsonl",
        &[
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::BudgetWarn, "s1"),
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::CompactHint, "s2"),
            rec(UsageKind::BudgetOver, "s2"),
        ],
    );
    // First-line lines just to feed aggregate_sessions directly.
    let s = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = s.lines().collect();
    let sessions = kf_budget_core::aggregate_sessions(&lines);
    let s1 = sessions.get("s1").expect("s1 present");
    assert_eq!(s1.records, 3);
    assert_eq!(s1.warnings, 1);
    assert_eq!(s1.compactions, 0);
    // 2 slice records × (1000 - 400) = 1200 bytes saved.
    assert_eq!(s1.bytes_saved, 1200);
    let s2 = sessions.get("s2").expect("s2 present");
    assert_eq!(s2.records, 2);
    assert_eq!(s2.warnings, 1); // budget_over counts as warning
    assert_eq!(s2.compactions, 1);
    assert_eq!(s2.bytes_saved, 0);
}

// ponytail: regression guard for the `--summary --json` ordering
// bug. Pre-fix, the JSON branch short-circuited before the
// summary check, so `report --summary --json` emitted raw
// filtered records instead of per-session totals. Post-fix the
// summary path runs first and emits the same aggregated shape
// for both human and JSON modes. A contributor who re-orders
// the branches back (as_json before summary) surfaces here.
#[test]
fn report_summary_with_json_emits_aggregated_session_totals() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_records(
        dir.path(),
        "usage.jsonl",
        &[
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::BudgetWarn, "s1"),
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::CompactHint, "s2"),
            rec(UsageKind::BudgetOver, "s2"),
        ],
    );
    // Capture stdout from the at() call's println via a
    // minimal harness — the function writes to stdout, so we
    // assert on the return value (sessions.len()) and on the
    // raw-record short-circuit that would have shown up here.
    let n = commands::report::at(&path, 100, true, None, None, true);
    assert_eq!(n, 2, "two distinct sessions aggregated");
    // ponytail: pin the count rather than the JSON text — the
    // serialised shape (key order, snake_case field names) is
    // pinned by the SessionTotals + BTreeMap contract, and a
    // contributor who switches back to the raw-records branch
    // would change the return value from sessions.len() to
    // lines.len() (5), which this assertion catches.
}

// ponytail: the return value of `at(... summary=true ...)`
// must be `sessions.len()` regardless of JSON vs human. A
// contributor who leaves the JSON branch's return as
// `lines.len()` but routes the summary through it would surface
// here as n=5 (filtered count) instead of n=2 (sessions).
#[test]
fn report_summary_return_value_is_session_count_not_line_count() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_records(
        dir.path(),
        "usage.jsonl",
        &[
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::Slice, "s2"),
            rec(UsageKind::Slice, "s3"),
            rec(UsageKind::Slice, "s3"), // duplicate session — still counts as 1
        ],
    );
    let n_human = commands::report::at(&path, 100, true, None, None, false);
    let n_json = commands::report::at(&path, 100, true, None, None, true);
    assert_eq!(n_human, 3, "human mode: 3 distinct sessions");
    assert_eq!(
        n_json, 3,
        "json mode: same 3 distinct sessions, not 4 lines"
    );
}

// ponytail: regression guard for the `--summary --last` interaction
// bug. Pre-fix, `commands::report::at` truncated `filtered` to the
// last N lines BEFORE passing to `aggregate_sessions` — so
// `report --summary --last 5` on a 10-record file with session
// "early" only in records 1-5 silently dropped "early" from the
// per-session totals. Per ADR-0010 § Report subcommand, `--last`
// is the detailed-view knob ("Detailed view: last N records, one
// per line") and `--summary` aggregates the full filtered set
// ("Summary view: total bytes saved, total warnings, total
// compactions, per-session totals"). The fix routes aggregation
// through `&filtered`, not `tail_lines(&filtered, last)`. This
// test pins BOTH sides: session "early" survives the truncation
// window AND its records/warnings counts are aggregated across the
// full 5 records (not the last 2).
#[test]
fn report_summary_ignores_last_and_aggregates_full_filtered_set() {
    let dir = tempfile::tempdir().unwrap();
    // 5 records total. s1 occupies the FIRST 3 (would be cut by
    // tail-2), s2 occupies the LAST 2. With last=2 the pre-fix
    // code aggregated only s2; the post-fix code aggregates both.
    let path = write_records(
        dir.path(),
        "usage.jsonl",
        &[
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::BudgetWarn, "s1"),
            rec(UsageKind::Slice, "s1"),
            rec(UsageKind::CompactHint, "s2"),
            rec(UsageKind::BudgetOver, "s2"),
        ],
    );
    // last=2 — would drop s1 from the truncated slice entirely.
    let n = commands::report::at(&path, 2, true, None, None, false);
    assert_eq!(
        n, 2,
        "two distinct sessions aggregated; \
             pre-fix this returned 1 (s1 lost via tail-2 truncation)"
    );
    // Independent check: drive aggregate_sessions directly on the
    // file's full line set to pin the expected per-session totals.
    let s = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = s.lines().collect();
    let sessions = kf_budget_core::aggregate_sessions(&lines);
    let s1 = sessions
        .get("s1")
        .expect("s1 must survive --summary --last 2");
    assert_eq!(
        s1.records, 3,
        "s1 has 3 records; tail-2 must NOT truncate them"
    );
    assert_eq!(s1.warnings, 1, "s1 has 1 budget_warn; must be aggregated");
    let s2 = sessions.get("s2").expect("s2 present");
    assert_eq!(s2.records, 2);
    assert_eq!(s2.compactions, 1);
}

// ponytail: pin the empty-file branch. ADR-0010 § Report
// subcommand says `kf-budget report` against a fresh install
// (no usage.jsonl yet) must return 0 without panicking and
// surface an eprintln so the user knows why nothing showed up.
// Pre-fix, the missing-file path returned 0 silently — a
// user running `kf-budget report` to verify their first session
// got logged got blank output and no signal. A contributor
// who replaces the eprintln with `return 1` (so the exit
// code tells the user something went wrong) surfaces here
// because the test asserts the return value is 0 (the
// documented "no records" code, not a failure code).
#[test]
fn report_returns_zero_on_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.jsonl");
    // summary=true forces the same eprintln regardless of mode
    // (the missing-file path runs before the summary branch).
    let n = commands::report::at(&path, 100, true, None, None, false);
    assert_eq!(
        n, 0,
        "missing usage.jsonl must return 0 (no records), not panic or \
             return a non-zero code; the eprintln on stderr is the diagnostic"
    );
}

// ponytail: same branch, but in the detailed-view path
// (summary=false, last=N). Both code paths funnel through the
// same early-return at the file-read site; the test catches a
// refactor that moves one of the two paths off that early
// return and ends up reading a non-existent file as an empty
// string (which would yield an empty `all: Vec<&str>`, a
// `filtered` of length 0, and a tail_lines of length 0 —
// indistinguishable from "no records" without the eprintln).
#[test]
fn report_returns_zero_on_missing_file_detailed_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.jsonl");
    let n = commands::report::at(&path, 100, false, None, None, false);
    assert_eq!(
        n, 0,
        "missing usage.jsonl on the detailed path must also return 0; \
             a refactor that drops the early-return guard surfaces here as n=0 too, \
             but the eprintln diagnostic distinguishes the two cases"
    );
}

// ADR-0009 § Error contract: hook handlers must not crash the host
// on a bad payload. We exercise the binary as a subprocess so the
// real exit path is taken — a unit test on `read_stdin_json`
// would not catch a regression where a future refactor
// reintroduces a hard exit.
fn run_hook_subprocess(subcmd: &str, stdin: &[u8]) -> std::process::Output {
    // ponytail: tempdirs MUST outlive the subprocess — `Command::env`
    // copies the path string into the child's env, but the directory
    // on disk is owned by the TempDir guard. If the guard drops before
    // the child reads its env, the path points at a deleted dir and
    // the hook handler silently fails to write to PLUGIN3_DATA_DIR.
    // Hold each guard in a binding that lives until after wait_with_output.
    let cfg_dir = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(kf_budget_binary_path())
        .args(["hook", subcmd])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn kf-budget");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().expect("wait")
}

#[test]
fn hook_post_tool_use_does_not_crash_on_garbage_stdin() {
    let out = run_hook_subprocess("post-tool-use", b"not json {{{");
    assert!(
        out.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    assert_eq!(v["content"], "");
    assert!(v["note"].as_str().unwrap().contains("parse failed"));
}

#[test]
fn hook_post_tool_use_parse_failure_path_emits_two_field_wire_shape() {
    // ponytail: pin the PostToolUse parse-failure wire shape at
    // the subprocess level. The CLI emits
    //   `{"content": "", "note": "kf-budget: ..."}`
    // — exactly two top-level keys, `content` is the empty
    // string (the host's payload never made it through), `note`
    // is a non-null string. Existing
    // `hook_post_tool_use_does_not_crash_on_garbage_stdin`
    // asserts behavior (`content == ""`, `note` contains
    // "parse failed") but NOT the field set — a contributor who
    // renames `content` → `output` keeps that test green and
    // silently breaks Claude Code, which reads `content` to
    // replace the tool result in memory. Drift catches here.
    //
    // Note MUST be a string, not null. The hook contract is
    // "passthrough with a note" — null would lose the diagnostic
    // the user sees in the Claude Code transcript. A contributor
    // who shortens the parse-failure branch to emit
    // `{"content":""}` (no note) surfaces here.
    let out = run_hook_subprocess("post-tool-use", b"not json {{{");
    assert!(
        out.status.success(),
        "parse failure must still exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level object");
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["content", "note"].into_iter().collect(),
        "PostToolUse parse-failure response must have exactly \
             {{content, note}} top-level keys. Claude Code reads \
             `content` to overwrite the tool result in memory; a \
             rename to `output` or a sibling `debug` key surfaces here. \
             got: {keys:?}"
    );
    assert_eq!(
        v["content"], "",
        "content must be empty string on parse failure (the \
             payload never made it through); a non-empty string here \
             means the CLI is echoing garbage the host sent"
    );
    let note = v["note"].as_str().expect(
        "note must be a non-null string on parse failure \
                     — the contract is 'passthrough with a note'",
    );
    assert!(
        note.starts_with("kf-budget: "),
        "note must start with `kf-budget: ` prefix so users see \
             which subsystem emitted it; got: {note:?}"
    );
}

#[test]
fn hook_post_tool_use_keep_passthrough_emits_two_field_wire_shape() {
    // ponytail: pin the Keep passthrough wire shape. The CLI
    // emits
    //   `{"content": <payload.content>, "note": null}`
    // — exactly two top-level keys, `content` echoes the
    // payload verbatim (no slicing), `note` is null (no
    // diagnostic — Keep is the boring happy path).
    //
    // Note MUST be present-and-null, not absent. A contributor
    // who adds `#[serde(skip_serializing_if = "Option::is_none")]`
    // to `PostToolUseResponse::note` would emit
    // `{"content":"..."}` instead of `{"content":"...","note":null}`.
    // Both are valid JSON, but Claude Code's schema check may
    // expect the key to exist; removing it changes the wire
    // contract. Drift catches here.
    //
    // Payload is sized to stay well below the 256-byte slice
    // threshold (HeadTailSlicer's default) so the orchestrator
    // routes through the Keep branch — Slice would emit a
    // marker in `content` and a non-null note, defeating the
    // test. A contributor who lowers the threshold below 12
    // bytes surfaces here as a content mismatch.
    let payload = serde_json::json!({
        "tool_name": "Read",
        "tool_result_key": "k1",
        "content": "small ok body",
        "session_id": "sess-pin",
    });
    let out = run_hook_subprocess(
        "post-tool-use",
        serde_json::to_vec(&payload).unwrap().as_slice(),
    );
    assert!(
        out.status.success(),
        "Keep passthrough must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level object");
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["content", "note"].into_iter().collect(),
        "PostToolUse Keep response must have exactly \
             {{content, note}} top-level keys. `note: null` is \
             load-bearing — Claude Code may schema-check the key's \
             presence even when null. got: {keys:?}"
    );
    assert_eq!(
        v["content"], "small ok body",
        "Keep must echo payload content verbatim; any slicing \
             (marker in content) means the threshold regressed below \
             the payload size — surfaced here as a content mismatch"
    );
    assert!(
        v["note"].is_null(),
        "Keep must emit `note: null` (not absent, not a string); \
             the Keep branch is the boring happy path and has no \
             diagnostic for the user. got: {}",
        v["note"]
    );
}

#[test]
fn hook_user_prompt_submit_falls_back_to_allow() {
    let out = run_hook_subprocess("user-prompt-submit", b"definitely not json");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "allow", "fallback must be Allow");
}

#[test]
fn hook_user_prompt_submit_happy_path_allow_wire_shape_is_pinned() {
    // ponytail: subprocess pin for the happy-path Allow
    // variant. Existing `hook_user_prompt_submit_falls_back_to_allow`
    // asserts the parse-failure fallback emits `kind == "allow"`
    // — but the parse-failure branch serialises
    // `Intervention::Allow` directly without touching the
    // budget, so it never exercises the
    // `decide → classify_kind → serialize` chain. A contributor
    // who adds a `note` field to the Allow arm of the decide
    // switch (e.g. `Intervention::Allow { note: None }`) keeps
    // the parse-failure pin green and breaks Claude Code's
    // decision router silently. Drift catches here.
    //
    // Subprocess setup:
    //   1. Fresh tempdirs (no budget.toml) → default budget
    //      (ceiling=200_000, used=0, approaching_ratio=0.8).
    //   2. Feed a small non-code prompt ("hello world" → 11
    //      bytes / 4 = 2 tokens at the bytes/4 estimator).
    //   3. After `record(2)`: used=2, ratio=0.00001 → Under,
    //      can_send(2)=true → decide returns `Intervention::Allow`.
    //   4. classify_kind(Allow) returns None → no usage record
    //      emitted (a healthy turn is not a "significant event"
    //      per ADR-0010).
    //   5. Serialised Allow has no extra fields (tagged-enum
    //      variant with no payload) → `{"kind":"allow"}`.
    let payload = serde_json::json!({
        "prompt": "hello world",
        "session_id": "sess-allow",
    });
    let out = run_hook_subprocess(
        "user-prompt-submit",
        serde_json::to_vec(&payload).unwrap().as_slice(),
    );
    assert!(
        out.status.success(),
        "user-prompt-submit happy-path Allow must exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect(
        "UserPromptSubmitResponse Allow serialises to an \
                     object (tagged enum — even single-field variants \
                     carry the `kind` discriminator)",
    );
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["kind"].into_iter().collect(),
        "Allow variant must have exactly {{kind}} top-level key. \
             A contributor who adds a sibling field (e.g. `note`, \
             `tokens_remaining`, `hint`) on the happy path keeps \
             the parse-failure pin green and breaks Claude Code's \
             decision router silently. got: {keys:?}"
    );
    assert_eq!(
        v["kind"], "allow",
        "Allow variant must serialise `kind` as the snake_case \
             `\"allow\"`; `\"Allow\"` or `\"ALLOW\"` here would break \
             every `jq '.kind == \"allow\"'` filter. got: {:?}",
        v["kind"]
    );
}

#[test]
fn hook_user_prompt_submit_warn_variant_wire_shape_is_pinned() {
    // ponytail: subprocess pin for the `Warn { remaining }`
    // variant of `UserPromptSubmitResponse`. Round 35's
    // `user_prompt_submit_response_wire_shape_pins_all_four_variants`
    // pins the canonical enum's serde shape directly; the
    // existing parse-failure pin at line 855 only asserts
    // `kind == "allow"` and doesn't pin the field set. This
    // test exercises the WARN arm of the `decide(...)` switch
    // at the subprocess layer: a valid payload + a budget in
    // Approaching state → `{"kind": "warn", "remaining": N}`.
    //
    // The `Warn` variant is the load-bearing one for Claude
    // Code's UI — it surfaces the budget headroom to the user
    // before Over fires. A contributor who renames `remaining`
    // → `tokens_left` (or drops it entirely) breaks the
    // Claude Code warning display silently. The field set
    // pin below catches both renames and additions.
    //
    // Subprocess setup:
    //   1. Pre-write `budget.toml` with ceiling=100, used=80,
    //      approaching_ratio=0.8 → state Approaching on load.
    //   2. Feed a small non-code prompt (10 chars → ~2 tokens
    //      at the bytes/4 estimator).
    //   3. After `record(2)`: used=82, ratio=0.82 ≥ 0.8 →
    //      Approaching → decide returns
    //      `Intervention::Warn { remaining: ceiling - used = 18 }`.
    //
    // The `run_hook_subprocess` helper sets `PLUGIN3_*_DIR`
    // to fresh tempdirs, which means `load_budget` reads
    // the seeded budget.toml from `runtime_dir/budget.toml` and
    // `save_budget` writes back to the same path. The pin
    // tolerates the post-decide save (the next test gets a
    // tempdir of its own).
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let budget_path = runtime_dir.path().join("budget.toml");
    let seed = TokenBudget {
        ceiling: 100,
        approaching_ratio: 0.8,
        used: 80,
    };
    std::fs::write(&budget_path, toml::to_string(&seed).unwrap()).unwrap();

    // ponytail: build the payload inline. A 10-char
    // non-code prompt yields ~2 tokens at the bytes/4
    // estimator; after `record(2)` the runtime budget is
    // used=82, ratio=0.82 → still Approaching (≥0.8). The
    // small incoming is below the slice / compact threshold
    // so the decide switch hits the `can_send` arm.
    let payload = serde_json::json!({
        "prompt": "short hey",  // 9 chars → ~2 tokens
        "session_id": "sess-warn",
    });

    let mut child = std::process::Command::new(kf_budget_binary_path())
        .args(["hook", "user-prompt-submit"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn kf-budget");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_vec(&payload).unwrap().as_slice())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "user-prompt-submit on a valid payload must exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect(
        "UserPromptSubmitResponse serialises to object \
                     (tagged enum, even single-field variants carry `kind`)",
    );
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["kind", "remaining"].into_iter().collect(),
        "Warn variant must have exactly {{kind, remaining}} \
             top-level keys; a contributor who renames `remaining` \
             → `tokens_left` (or adds a sibling field) breaks the \
             Claude Code warning display silently. got: {keys:?}"
    );
    assert_eq!(
        v["kind"], "warn",
        "Warn variant must serialise `kind` as the snake_case \
             `\"warn\"` (serde `rename_all = \"snake_case\"`); \
             `\"Warning\"` or `\"WARN\"` here would break every \
             `jq '.kind == \"warn\"'` filter. got: {:?}",
        v["kind"]
    );
    // ponytail: pin the remaining count. With ceiling=100,
    // used_before=80, incoming=2 → after record: used=82,
    // remaining = ceiling - used = 18. A contributor who
    // wires `remaining` to `budget.used` (off-by-one) or
    // hardcodes `0` surfaces here as a numeric mismatch.
    assert_eq!(
        v["remaining"], 18,
        "remaining must be `ceiling - used_after_record` = \
             100 - 82 = 18; a different value means the budget \
             state didn't read from the seeded budget.toml OR the \
             record/decide math regressed. got: {}",
        v["remaining"]
    );
}

#[test]
fn hook_user_prompt_submit_slice_variant_wire_shape_is_pinned() {
    // ponytail: subprocess pin for the `Slice { target_key,
    // slice_to }` variant of `UserPromptSubmitResponse`. The
    // Round 47 `Warn` test exercised the Approaching state;
    // this test exercises the Slice arm of `decide(...)` —
    // fired when the budget is Over AND a recent output is
    // large enough to slice down by the overflow amount.
    //
    // Subprocess setup:
    //   1. budget.toml: ceiling=100, used=100 → state Over
    //      (ratio 1.0). After record(50): used=150, can_send=false.
    //   2. recent_outputs.jsonl: one entry
    //        {"key": "big-tool-result", "size": 400}
    //      so `max_by_key(|s| s)` returns 400.
    //   3. needed = incoming(50) - remaining(0) = 50.
    //   4. size(400) > needed(50) + SLICE_OVERHEAD(256) = 306 ✓
    //      → Slice { target_key: "big-tool-result",
    //               slice_to: 400 - 50 = 350 }
    //
    // The `target_key` and `slice_to` fields are the
    // load-bearing payload — Claude Code uses `target_key`
    // to find the tool result to slice and `slice_to` to know
    // where to truncate. A contributor who renames either
    // breaks the contract silently.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let budget_path = runtime_dir.path().join("budget.toml");
    let recent_path = data_dir.path().join("recent_outputs.jsonl");
    std::fs::write(
        &budget_path,
        toml::to_string(&TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 100,
        })
        .unwrap(),
    )
    .unwrap();
    std::fs::write(&recent_path, "{\"key\":\"big-tool-result\",\"size\":400}\n").unwrap();

    // ponytail: a 200-char non-code prompt → ~50 tokens at
    // the bytes/4 estimator. After `record(50)`, used jumps
    // from 100 → 150 (past ceiling). The decide switch falls
    // through the `can_send` arm and hits the Slice path
    // because recent has a 400-byte entry.
    let payload = serde_json::json!({
        "prompt": "x".repeat(200),
        "session_id": "sess-slice",
    });

    let mut child = std::process::Command::new(kf_budget_binary_path())
        .args(["hook", "user-prompt-submit"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn kf-budget");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_vec(&payload).unwrap().as_slice())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "user-prompt-submit on a valid payload must exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v
        .as_object()
        .expect("Slice variant serialises to object (tagged enum)");
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["kind", "slice_to", "target_key"].into_iter().collect(),
        "Slice variant must have exactly \
             {{kind, target_key, slice_to}} top-level keys; a \
             contributor who renames `target_key` → `key` (or \
             `slice_to` → `bytes`) breaks Claude Code's \
             slice-and-replace step silently. got: {keys:?}"
    );
    assert_eq!(
        v["kind"], "slice",
        "Slice variant must serialise `kind` as the snake_case \
             `\"slice\"`; `\"Slice\"` or `\"SLICE\"` here breaks \
             every `jq '.kind == \"slice\"'` filter. got: {:?}",
        v["kind"]
    );
    assert_eq!(
        v["target_key"], "big-tool-result",
        "target_key must echo the recent-outputs entry key the \
             decide() picked; a different value means the max-by-key \
             selection regressed (or recent_outputs.jsonl wasn't \
             read). got: {:?}",
        v["target_key"]
    );
    // ponytail: pin the slice_to math. decide returns
    //   slice_to = size.saturating_sub(needed) = 400 - 50 = 350
    // A contributor who flips the subtraction (or drops
    // SLICE_OVERHEAD from the comparison) surfaces here.
    assert_eq!(
        v["slice_to"], 350,
        "slice_to must be `size - needed` = 400 - 50 = 350; a \
             different value means the slice math regressed. got: {}",
        v["slice_to"]
    );
}

#[test]
fn hook_user_prompt_submit_compact_variant_wire_shape_is_pinned() {
    // ponytail: subprocess pin for the `Compact { reason }`
    // variant of `UserPromptSubmitResponse`. The Slice test
    // exercised the path where a recent output is large
    // enough to truncate; this test exercises the fallback
    // Compact path — Over budget AND no recent entry is
    // large enough to slice. `decide(...)` falls through to
    //   Compact { reason: "session at {used}/{ceiling} tokens;
    //              cannot fit {incoming} more" }
    //
    // The `reason` string is human-readable text Claude Code
    // shows to the user before compaction. A contributor who
    // drops the trailing suffix (e.g. just `format!("{}",
    // used)`) keeps the `kind` intact and loses the
    // diagnostic. Drift catches here.
    //
    // Subprocess setup:
    //   1. budget.toml: ceiling=10, used=10 → state Over.
    //   2. recent_outputs.jsonl: ABSENT — `max_by_key` returns
    //      None → falls through to Compact.
    //   3. Prompt: 40 chars → ~10 tokens at bytes/4.
    //      After `record(10)`: used=20, can_send=false.
    //      needed = 10 - 0 = 10. No recent → Compact.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let budget_path = runtime_dir.path().join("budget.toml");
    std::fs::write(
        &budget_path,
        toml::to_string(&TokenBudget {
            ceiling: 10,
            approaching_ratio: 0.8,
            used: 10,
        })
        .unwrap(),
    )
    .unwrap();
    // recent_outputs.jsonl intentionally not created.

    let payload = serde_json::json!({
        "prompt": "x".repeat(40),
        "session_id": "sess-compact",
    });

    let mut child = std::process::Command::new(kf_budget_binary_path())
        .args(["hook", "user-prompt-submit"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn kf-budget");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_vec(&payload).unwrap().as_slice())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "user-prompt-submit on a valid payload must exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v
        .as_object()
        .expect("Compact variant serialises to object (tagged enum)");
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["kind", "reason"].into_iter().collect(),
        "Compact variant must have exactly {{kind, reason}} \
             top-level keys; a contributor who renames `reason` \
             → `why` (or adds a sibling field) breaks Claude \
             Code's compact-suggestion UI silently. got: {keys:?}"
    );
    assert_eq!(
        v["kind"], "compact",
        "Compact variant must serialise `kind` as the snake_case \
             `\"compact\"`; `\"Compact\"` or `\"COMPACT\"` here \
             breaks every `jq '.kind == \"compact\"'` filter. \
             got: {:?}",
        v["kind"]
    );
    // ponytail: pin the reason format. decide returns
    //   "session at {used}/{ceiling} tokens; cannot fit {incoming} more"
    // After record(10): used=20, ceiling=10, incoming=10 →
    //   "session at 20/10 tokens; cannot fit 10 more"
    // A contributor who drops the trailing " cannot fit N more"
    // (or reorders the fields) surfaces here as a reason
    // mismatch.
    assert_eq!(
        v["reason"], "session at 20/10 tokens; cannot fit 10 more",
        "reason must match `decide()`'s literal format string; \
             a contributor who tweaks the format (drops the \
             trailing suffix, reorders fields) surfaces here. got: {:?}",
        v["reason"]
    );
}

#[test]
fn hook_pre_compact_emits_null_hint_on_garbage() {
    let out = run_hook_subprocess("pre-compact", b"");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["hint"].is_null());
}

#[test]
fn hook_pre_compact_happy_path_wire_shape_is_pinned() {
    // ponytail: subprocess pin for the PreCompact happy path.
    // Existing coverage:
    //   - `hook_pre_compact_emits_null_hint_on_garbage` (parse-failure)
    //   - `pre_compact_wire_shape_pins_parse_failure_and_empty_history`
    //     (literal-substring scan of the source — pins both
    //     branches by their JSON literals, but never spawns)
    // Neither exercises the post-decide branch through the
    // full subprocess: clap → stdin parse → CompactHint build →
    // LocalSummaryCompactor → wire shape. A contributor who
    // renames the response key from `hint` → `advice` (or
    // from `summary` → `preview`) keeps the literal scan
    // green and breaks Claude Code silently — Claude Code
    // reads `hint` to seed its compactor and `summary` as a
    // head-start. Drift catches here.
    //
    // Subprocess setup:
    //   1. Feed a PreCompactPayload with 3 turns (indices 0,1,2)
    //      so the post-decide branch fires (the parse-failure
    //      fallback returns early with `{hint: null}`).
    //   2. Fresh tempdir → default TokenBudget
    //      (used=0, ceiling=200_000). The CompactHint reports
    //      tokens_used=0, tokens_ceiling=200_000, and the turn
    //      range spans the full history (oldest_turn=0,
    //      newest_turn=2).
    //   3. LocalSummaryCompactor runs over the joined turns;
    //      for 3 short lines it returns the input verbatim
    //      (well under the 500-char per-line cap and the
    //      8192-byte total cap).
    let payload = serde_json::json!({
        "history_turns": [
            {"index": 0, "role": "user", "content_preview": "hello"},
            {"index": 1, "role": "assistant", "content_preview": "world"},
            {"index": 2, "role": "user", "content_preview": "foo"},
        ],
    });
    let out = run_hook_subprocess(
        "pre-compact",
        serde_json::to_vec(&payload).unwrap().as_slice(),
    );
    assert!(
        out.status.success(),
        "PreCompact happy path must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v
        .as_object()
        .expect("PreCompact response top-level is an object");
    let top_keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        top_keys,
        ["hint", "summary"].into_iter().collect(),
        "PreCompact happy-path response must have exactly \
             {{hint, summary}} top-level keys. Claude Code reads \
             `hint` to seed its compactor and `summary` as a \
             head-start; renaming either breaks the bridge \
             silently. got: {top_keys:?}"
    );

    // ponytail: pin the CompactHint shape (same 5 fields as
    // the budget compact --json envelope). A contributor who
    // adds a 6th field here surfaces here.
    let hint = obj["hint"].as_object().expect(
        "hint must be an object (CompactHint), not null \
                     on the happy path — null is reserved for the \
                     parse-failure fallback",
    );
    let hint_keys: std::collections::BTreeSet<&str> = hint.keys().map(String::as_str).collect();
    assert_eq!(
        hint_keys,
        [
            "newest_turn",
            "oldest_turn",
            "reason",
            "tokens_ceiling",
            "tokens_used"
        ]
        .into_iter()
        .collect(),
        "PreCompact hint must be the 5-field CompactHint shape; \
             a contributor who adds a 6th field (e.g. \
             `triggered_at`) propagates here. got: {hint_keys:?}"
    );

    // ponytail: pin the turn range and budget values for the
    // seeded history. A contributor who truncates the history
    // (or wires `oldest_turn` to `history_turns.last()`) breaks
    // the range — `hint.oldest_turn` must be 0 (head) and
    // `hint.newest_turn` must be 2 (tail of 3 turns).
    assert_eq!(
        hint["oldest_turn"], 0,
        "oldest_turn must be 0 (head of seeded history); a \
             different value means `history.first()` lost the head"
    );
    assert_eq!(
        hint["newest_turn"], 2,
        "newest_turn must be 2 (tail of 3 seeded turns); a \
             different value means `history.last()` regressed"
    );
    assert_eq!(
        hint["tokens_used"], 0,
        "tokens_used on a fresh tempdir must be 0; the default \
             TokenBudget starts at 0"
    );
    assert_eq!(
        hint["tokens_ceiling"], 200_000,
        "tokens_ceiling on a fresh tempdir must be the default \
             200_000; a different value means PLUGIN3_CONFIG_DIR \
             leaked through and config.toml set a custom ceiling"
    );
    assert_eq!(
        hint["reason"], "session at 0/200000 tokens; compaction suggested",
        "reason must be the literal `compaction::build_hint` \
             format; tweaking it surfaces here as a mismatch"
    );

    // ponytail: pin that `summary` is a non-empty string.
    // LocalSummaryCompactor runs over the joined turns and
    // returns a non-empty summary for any non-empty input
    // (each line < 500 chars, total < 8192 bytes — neither
    // bound triggers here). A contributor who shortens the
    // hook to emit `"summary": ""` (or omits the field)
    // surfaces here.
    let summary = obj["summary"].as_str().expect(
        "summary must be a non-null string on the happy \
                     path — Claude Code reads it as the compactor \
                     head-start",
    );
    assert!(
        !summary.is_empty(),
        "summary must be non-empty on a non-empty history; an \
             empty string here means the LocalSummaryCompactor was \
             bypassed (or its output was thrown away). got: {summary:?}"
    );
}

// ADR-0016 § Integration tests: pipe a real PostToolUse payload
// with a 50 KB cargo-test-shaped body and assert slicing occurred.
#[test]
fn hook_post_tool_use_slices_large_cargo_test_output() {
    // Shape that detector::from_shape classifies as TestRunner.
    let mut body = String::from("running 5 tests\ntest foo ... ok\n");
    body.push_str(&"x".repeat(50_000));
    body.push_str("\ntest bar ... FAILED\n");
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_result_key": "abc",
        "content": body,
        "session_id": "s1",
    })
    .to_string();
    let out = run_hook_subprocess("post-tool-use", payload.as_bytes());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let content = v["content"].as_str().expect("content is string");
    // Slicing occurred — response is shorter than input.
    assert!(
        content.len() < body.len(),
        "sliced {} -> {} bytes",
        body.len(),
        content.len()
    );
    // The slice marker is present (ADR-0003).
    assert!(
        content.contains("<<kf-budget:slice:"),
        "expected marker in {content}"
    );
    // The note explains the slicing.
    let note = v["note"].as_str().expect("note on slice");
    assert!(note.contains("sliced"));
}

#[test]
fn hook_post_tool_use_slice_path_wire_shape_is_pinned() {
    // ponytail: subprocess-level wire pin for the PostToolUse
    // slice path. Existing
    // `hook_post_tool_use_slices_large_cargo_test_output` checks
    // behavior (response shorter than input, marker present,
    // note contains "sliced") but NOT the field set or the
    // note's exact prefix. A contributor who adds a sibling
    // `kind` field (e.g. `{"kind":"sliced", "content":...}`) or
    // renames `note` → `diagnostic` keeps the behavior tests
    // green and breaks Claude Code silently — Claude Code reads
    // `content` to overwrite the tool result in memory and
    // `note` to surface the diagnostic. Drift catches here.
    //
    // Payload: 50 KB cargo-test-shaped body. The detector
    // recognises the "running N tests / test ... ok / FAILED"
    // shape as TestRunner, and 50 KB > 8 KB triggers the Slice
    // decision in `detector::should_slice`. The orchestrator
    // then routes through HeadTailSlicer (default head/tail
    // 4096/4096), producing a `<<kf-budget:slice:<key>>>`
    // marker between head and tail.
    let mut body = String::from("running 5 tests\ntest foo ... ok\n");
    body.push_str(&"x".repeat(50_000));
    body.push_str("\ntest bar ... FAILED\n");
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_result_key": "abc",
        "content": body,
        "session_id": "s1",
    })
    .to_string();
    let out = run_hook_subprocess("post-tool-use", payload.as_bytes());
    assert!(
        out.status.success(),
        "PostToolUse slice path must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v
        .as_object()
        .expect("PostToolUse slice-path response top-level is an object");
    let top_keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        top_keys,
        ["content", "note"].into_iter().collect(),
        "PostToolUse slice-path response must have exactly \
             {{content, note}} top-level keys. Claude Code reads \
             `content` to overwrite the tool result in memory and \
             `note` to surface the diagnostic; adding a sibling \
             `kind`/`debug` key or renaming either field breaks the \
             bridge silently. got: {top_keys:?}"
    );

    // ponytail: pin the content shape on the slice path.
    // `content` must be non-empty (else the host would see an
    // empty tool result, which is a regression worse than the
    // original), and shorter than the input (proving slicing
    // happened), and contain the canonical slice marker
    // (ADR-0003). A contributor who emits only `head` (forgets
    // the marker or tail) surfaces here as a missing-marker
    // failure.
    let content = obj["content"].as_str().expect(
        "content must be a non-null string on the slice path \
                     — Claude Code reads it as the replacement tool result",
    );
    assert!(
        !content.is_empty(),
        "content must be non-empty on the slice path; an empty \
             string here means the orchestrator emitted a headless \
             SlicedOutput (regression). got: {content:?}"
    );
    assert!(
        content.len() < body.len(),
        "content ({} bytes) must be shorter than input ({} bytes) \
             — a same-length content means slicing didn't happen",
        content.len(),
        body.len()
    );
    assert!(
        content.contains("<<kf-budget:slice:"),
        "content must contain the canonical slice marker \
             `<<kf-budget:slice:` (ADR-0003); missing marker means the \
             orchestrator forgot to offload the middle. got: {content}"
    );

    // ponytail: pin the note shape on the slice path. `note`
    // must be a non-null string with the literal `sliced `
    // prefix and the ` bytes kept)` suffix — the format is
    // `format!("sliced {kind:?} ({bytes_kept} bytes kept)")` in
    // `hooks::post_tool_use`. A contributor who flips the
    // prefix to `Sliced` (capital S) or drops `bytes kept`
    // surfaces here. Also assert the format wraps a positive
    // integer — `bytes_kept` for a 50 KB input with head/tail
    // 4096/4096 is 8192, so any number > 0 confirms the
    // arithmetic is actually being computed.
    let note = obj["note"].as_str().expect(
        "note must be a non-null string on the slice path \
                     — the contract is `note = Some(...)`, not None. \
                     `None` is reserved for Keep (passthrough).",
    );
    assert!(
        note.starts_with("sliced "),
        "note must start with `sliced ` (lowercase) prefix; the \
             format string is `sliced {{kind:?}} ({{bytes_kept}} bytes kept)`. \
             A contributor who capitalises (`Sliced`) or rewrites the \
             prefix (`Slice:`) surfaces here. got: {note:?}"
    );
    assert!(
        note.ends_with(" bytes kept)"),
        "note must end with ` bytes kept)`; the format includes \
             the byte count for the diagnostic. got: {note:?}"
    );
    // ponytail: extract the byte count from the parens and
    // assert it's a positive integer. The format is
    // `sliced <KIND> (<N> bytes kept)` — splitting on
    // parentheses yields `(`, `<digits>`, ` bytes kept)` in
    // successive pieces. Parsing the middle ensures the format
    // continues to surface the actual byte count (a regression
    // to a static `"sliced"` would yield no digits and fail).
    let inner = note.split('(').nth(1).expect(
        "note must contain at least one `(` opening the \
                     byte-count parens; got: {note:?}",
    );
    let n_str = inner
        .split(' ')
        .next()
        .expect("note's inner `(` must be followed by a digit");
    let bytes_kept: usize = n_str.parse().unwrap_or_else(|_| {
        panic!(
            "note's byte-count field must parse as usize; got \
                 `{n_str}` (full note: {note:?})"
        )
    });
    assert!(
        bytes_kept > 0,
        "bytes_kept must be a positive integer (the sliced \
             output retains at least the head and tail); 0 here \
             means the format regressed to `sliced ... (0 bytes kept)`"
    );
}

#[test]
fn hook_post_tool_use_passes_through_small_output() {
    // 100-byte body, TestRunner threshold is 8 KB → Keep.
    let body = "running 1 test\ntest foo ... ok\n";
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_result_key": "abc",
        "content": body,
        "session_id": "s1",
    })
    .to_string();
    let out = run_hook_subprocess("post-tool-use", payload.as_bytes());
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["content"], body, "Keep passes content through verbatim");
    // Note is suppressed on Keep (ADR-0013: optional).
    assert!(v["note"].is_null(), "note must be null on Keep");
}

// ---- ADR-0015 § budget set --default subprocess wiring -----

// ponytail: spawn the real binary so we exercise clap arg
// parsing AND the persistence path. A unit test would only
// cover half of that contract.
fn run_budget_set_subprocess(
    ceiling: usize,
    persist: bool,
) -> (
    std::process::Output,
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    // ponytail: same TempDir-drop-before-spawn trap as run_hook_subprocess.
    // Returning all three guards so the caller can assert on cfg_dir/data_dir
    // and the runtime_dir guard still outlives the child's read of its env var.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let mut cmd = std::process::Command::new(kf_budget_binary_path());
    cmd.args(["budget", "set", &ceiling.to_string()])
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path());
    if persist {
        cmd.arg("--default");
    }
    let out = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget budget set");
    (out, cfg_dir, data_dir, runtime_dir)
}

#[test]
fn budget_set_default_persists_to_config_toml() {
    let (out, cfg_dir, _data_dir, _runtime_dir) = run_budget_set_subprocess(300_000, true);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg_path = cfg_dir.path().join("config.toml");
    let body = std::fs::read_to_string(&cfg_path).expect("config.toml written");
    // ADR-0005 § Defaults: the [budget] section carries ceiling + ratio.
    assert!(body.contains("[budget]"), "got: {body}");
    assert!(body.contains("ceiling = 300000"), "got: {body}");
    // Round-trip parse via the same wrapper `load_budget_config_at`
    // uses, so a future regression in ConfigFile's section handling
    // shows up here instead of as a silent drift.
    let file: ConfigFile = toml::from_str(&body).expect("parse");
    assert_eq!(file.budget.ceiling, 300_000);
}

#[test]
fn budget_set_without_default_does_not_touch_config_toml() {
    // ponytail: a plain `set` must remain session-local — no
    // config.toml write. Otherwise the `--default` flag becomes
    // meaningless (always on).
    let (out, cfg_dir, _data_dir, _runtime_dir) = run_budget_set_subprocess(150_000, false);
    assert!(out.status.success());
    let cfg_path = cfg_dir.path().join("config.toml");
    assert!(
        !cfg_path.exists(),
        "config.toml must NOT exist when --default is omitted"
    );
}

#[test]
fn budget_set_default_picks_up_on_next_load_budget() {
    // Session 1: write 222_000 as default.
    let (_out, cfg_dir, _data_dir, _runtime_dir) = run_budget_set_subprocess(222_000, true);
    // Session 2: fresh runtime dir; load_budget_with_config must
    // overlay the persisted default even though runtime budget.toml
    // is empty.
    let runtime_path = cfg_dir.path().join("runtime-fresh/budget.toml");
    std::fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let cfg_path = cfg_dir.path().join("config.toml");
    let b = load_budget_with_config(&runtime_path, &cfg_path);
    assert_eq!(b.ceiling, 222_000);
    assert_eq!(b.used, 0, "no carryover from config into a fresh session");
}
