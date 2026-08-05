// ponytail: ADR-0015 § Exit codes — exercises the binary as a subprocess
// so the real clap + std::process::exit paths are taken. Unit tests on
// the inner functions would not catch a regression where someone moves
// the exit(78) call behind a flag.
use super::*;

#[test]
fn config_validate_exits_78_on_corrupt_config() {
    // ponytail: corrupt config.toml must surface as EX_CONFIG (78).
    // ADR-0015 § Exit codes lists 78 for "config parse or backend
    // init failure". Writing a non-TOML file forces the parse
    // failure path inside run_path_checks.
    let (out, _cfg) = run_cli_subprocess_with_corrupt_file(
        &["config", "--validate"],
        b"this is = not [ valid",
        "config",
        "config.toml",
    );
    assert!(!out.status.success(), "corrupt config must exit non-zero");
    assert_eq!(
        out.status.code(),
        Some(78),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Sanity: the failure mention lives in stdout (the check table)
    // so an ops-tool that greps stderr-only still sees the non-zero
    // exit and a host that reads stdout sees the table.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FAIL"),
        "stdout must show the failing check: {stdout}"
    );
}

#[test]
fn config_validate_json_envelope_shape_is_pinned() {
    // ponytail: pin the `kf-budget --json config --validate` wire
    // shape. The CLI builds
    //   `{"ok": bool, "failures": usize, "checks": [...]}` where
    // each item in `checks` is `{label, path, status, detail}` and
    // `status` is the snake_case string `"ok"` or `"fail"` (NOT
    // the human format's `"OK  "` / `"FAIL"` — those are padded
    // for terminal alignment and would silently fail a downstream
    // `jq '.checks[].status == "ok"'` filter).
    //
    // The top-level `ok` is the boolean summary; `failures` is
    // the count. They must agree — a contributor who flips one
    // without the other (e.g. reports `ok: failures == 0` but
    // forgets to update `failures` itself) surfaces here.
    //
    // On a fresh tempdir all 8 `run_path_checks` rows pass:
    //   config_dir, data_dir, runtime_dir (directories)
    //   config_file, budget_file, slices_dir, usage_log,
    //   recent_outputs (file surfaces)
    // The exact count is the boundary; a contributor who adds a
    // 9th check (or drops one) without updating this test catches
    // here. The 8 count is the same number the
    // `run_path_checks_passes_on_fresh_tempdir` unit test asserts
    // — both must move together.
    let (out, _c, _d, _r) = run_cli_subprocess(&["--json", "config", "--validate"]);
    assert!(
        out.status.success(),
        "fresh tempdir must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level object");
    let top_keys: std::collections::BTreeSet<&str> =
        obj.keys().map(std::string::String::as_str).collect();
    assert_eq!(
        top_keys,
        ["checks", "failures", "ok"].into_iter().collect(),
        "config --validate --json top-level key set must be exactly \
             {{ok, failures, checks}}; a contributor who renames `failures` \
             → `failure_count` (or `ok` → `success`) breaks every \
             `jq '.ok'` audit silently. got: {top_keys:?}",
    );

    assert_eq!(
        v["ok"], true,
        "ok must be true on a fresh tempdir; false here means the \
             boolean summary drifted from the per-check status"
    );
    assert_eq!(
        v["failures"], 0,
        "failures count must be 0 on a fresh tempdir; non-zero here \
             means a check flipped to Fail without a reason (env? tempdir \
             quirk?)"
    );
    let checks = v["checks"].as_array().expect("checks is an array");
    assert_eq!(
        checks.len(),
        8,
        "fresh tempdir must produce 8 path checks (3 dirs + 5 file \
             surfaces); a contributor who adds or removes a check without \
             updating the wire pin surfaces here"
    );

    let expected_keys: std::collections::BTreeSet<&str> =
        ["detail", "label", "path", "status"].into_iter().collect();
    for (i, c) in checks.iter().enumerate() {
        let cobj = c
            .as_object()
            .unwrap_or_else(|| panic!("check[{i}] must be an object, got: {c}"));
        let keys: std::collections::BTreeSet<&str> =
            cobj.keys().map(std::string::String::as_str).collect();
        assert_eq!(
            keys, expected_keys,
            "check[{i}] field set drifted from ADR-0015; got: {keys:?}"
        );
        assert_eq!(
            c["status"], "ok",
            "check[{i}] (label={}) must report status `\"ok\"` (snake_case) \
                 on a fresh tempdir; `\"OK\"` or `\"OK  \"` would break a \
                 downstream `jq '.checks[].status == \"ok\"'` filter",
            c["label"]
        );
    }
}

#[test]
fn config_validate_json_status_fail_snake_case_is_pinned() {
    // ponytail: dual-arm pin for the `"fail"` status spelling
    // on the JSON path. Round 38's
    // `config_validate_json_envelope_shape_is_pinned` only
    // exercises the `"ok"` row (fresh tempdir, all checks pass).
    // A contributor who flips the JSON match arm to
    //   `CheckStatus::Fail => "failed"` (or `"FAIL"`)
    // breaks every `jq '.checks[] | select(.status == "fail")'`
    // filter — that filter would silently return an empty set
    // and the failing check would be invisible to dashboards.
    //
    // The human format (`"FAIL"`, padded for terminal alignment)
    // is a different surface and lives in a different match arm
    // (commands/config.rs:217). The JSON arm uses `"fail"` and
    // `"ok"` — both snake_case, both unpadded. This test pins
    // the JSON arm's `Fail` spelling.
    //
    // Also pins `ok: false` + `failures: 1` agreement with the
    // per-check row. A contributor who wires `ok` to always-true
    // (or `failures` to `0`) keeps the row's `"fail"` string
    // intact and breaks the summary signal — caught here.
    let (out, _cfg) = run_cli_subprocess_with_corrupt_file(
        &["--json", "config", "--validate"],
        b"this is = not [ valid",
        "config",
        "config.toml",
    );
    assert!(
        !out.status.success(),
        "corrupt config must exit non-zero (78); \
             a contributor who swallows the exit code surfaces here. \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(78),
        "ADR-0015 prescribes 78 for config parse failure"
    );

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is valid JSON even on the failure path");
    let obj = v.as_object().expect("top-level object");

    // ponytail: assert the summary fields agree with each
    // other. `ok: false` AND `failures >= 1` are both required;
    // a contributor who wires only one surfaces here.
    assert_eq!(
        v["ok"], false,
        "ok must be false when any check fails; `true` here means \
             the boolean summary drifted from `failures == 0` to a \
             constant"
    );
    let failures = v["failures"]
        .as_u64()
        .expect("failures must be a non-negative integer");
    assert!(
        failures >= 1,
        "failures must be ≥ 1 with a corrupt config.toml; \
             zero here means the corrupt-file test lost its bite"
    );
    assert_eq!(
        failures as usize,
        obj["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["status"] == "fail")
            .count(),
        "top-level `failures` count must equal the number of rows \
             with `status == \"fail\"`; a contributor who hardcodes \
             `failures: 1` (or computes it wrong) surfaces here"
    );

    // ponytail: locate the `config_file` row and assert its
    // `status` is the snake_case `"fail"`. The label is what
    // `run_path_checks` registers; a rename of the label would
    // also drift this test, but that's a separate concern
    // covered by `budget_validate_exits_78_on_corrupt_budget_toml`.
    let checks = obj["checks"].as_array().expect("checks is array");
    let config_file_row = checks
        .iter()
        .find(|c| c["label"] == "config_file")
        .expect("checks must include a row labelled `config_file`");
    assert_eq!(
        config_file_row["status"], "fail",
        "config_file row must report status `\"fail\"` (snake_case) \
             on a corrupt config.toml; `\"failed\"`, `\"FAIL\"`, or \
             `\"fail \"` (with whitespace) would break a downstream \
             `jq '.checks[] | select(.status == \"fail\")'` filter \
             and silently hide the failing check from dashboards. \
             got: {:?}",
        config_file_row["status"]
    );
    // ponytail: detail must be a non-empty string carrying
    // the parse error. A contributor who drops the error
    // message (sets `detail: ""`) keeps the row's `"fail"`
    // status and loses the diagnostic — the human format
    // shows it but the JSON surface would not.
    let detail = config_file_row["detail"]
        .as_str()
        .expect("detail must be a string, not null/array");
    assert!(
        !detail.is_empty(),
        "detail must carry the parse error message; an empty \
             string here means a contributor lost the diagnostic \
             the user needs to fix the file"
    );
}

#[test]
fn config_show_json_envelope_includes_sources_when_show_sources_passed() {
    // ponytail: pin the fix for the `--show-sources --json`
    // flag-drop bug. Pre-fix, the JSON branch returned early
    // before the env-source block, so `kf-budget --json config
    // --show-sources` produced identical output to `kf-budget
    // --json config` — the flag was silently swallowed on the
    // JSON path. The fix adds a `sources` key to the JSON
    // envelope when --show-sources is passed; without it, the
    // envelope stays at 8 keys (the existing shape).
    //
    // Two arms pin the dual state:
    //   1. `kf-budget --json config`                     → 8 keys, no `sources`
    //   2. `kf-budget --json config --show-sources`      → 9 keys, with `sources`
    // A contributor who always emits `sources` (or who restores
    // the silent drop) breaks one arm or the other.
    //
    // The `sources` value is a 3-key object (config_dir,
    // data_dir, runtime_dir), each value either `"XDG default"`
    // (no env var set) or `"env PLUGIN3_*=<value>"`. Setting
    // PLUGIN3_CONFIG_DIR in the second arm pins both value
    // shapes — a contributor who drops the env-prefix
    // formatting (returns the bare value without the `env
    // VAR=` prefix) breaks the audit affordance.
    let (out, _c, _d, _r) = run_cli_subprocess(&["--json", "config"]);
    assert!(
        out.status.success(),
        "config --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level object");
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert!(
        !keys.contains("sources"),
        "without --show-sources the envelope must NOT include a \
             `sources` key; a contributor who always emits `sources` \
             breaks consumers that grep the 8-key envelope. got: \
             {keys:?}"
    );
    assert_eq!(
        keys.len(),
        8,
        "without --show-sources the envelope must be exactly the \
             8 path keys (config_dir, data_dir, runtime_dir, \
             config_file, budget_file, slices_dir, usage_log, \
             recent_outputs). got: {keys:?}"
    );

    // ponytail: spawn with --show-sources AND a custom
    // PLUGIN3_CONFIG_DIR so we exercise both the `XDG default`
    // arm (data_dir, runtime_dir) AND the `env VAR=<value>`
    // arm (config_dir) in a single subprocess. `run_cli_subprocess`
    // sets PLUGIN3_CONFIG_DIR to the cfg tempdir, which would
    // hide the env-prefix formatting; use a one-off spawn
    // pattern with a distinct custom cfg tempdir. PLUGIN3_DATA_DIR
    // and PLUGIN3_RUNTIME_DIR are explicitly REMOVED (via
    // `env_remove`) so the XDG-default branch is exercised —
    // the parent test process may have these set, and inherited
    // env vars would silently turn the "XDG default" branch
    // into "env VAR=" here.
    let custom_cfg = tempfile::tempdir().expect("custom cfg tempdir");
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "config", "--show-sources"])
        .env("PLUGIN3_CONFIG_DIR", custom_cfg.path())
        .env_remove("PLUGIN3_DATA_DIR")
        .env_remove("PLUGIN3_RUNTIME_DIR")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget");
    assert!(
        out.status.success(),
        "config --json --show-sources must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level object");
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert!(
        keys.contains("sources"),
        "with --show-sources the envelope MUST include a \
             `sources` key — the flag-drop fix. got: {keys:?}"
    );
    assert_eq!(
        keys.len(),
        9,
        "with --show-sources the envelope must be 8 path keys + \
             1 sources key. got: {keys:?}"
    );

    // ponytail: pin the sources sub-object. config_dir is the
    // env-set one (custom_cfg.path()); data_dir and runtime_dir
    // are XDG default (no env var set for those in the spawn
    // above). A contributor who drops the `env VAR=` prefix
    // surfaces here as the config_dir value being a bare path.
    let sources = obj["sources"]
        .as_object()
        .expect("sources must be a nested object");
    let source_keys: std::collections::BTreeSet<&str> =
        sources.keys().map(String::as_str).collect();
    assert_eq!(
        source_keys,
        ["config_dir", "data_dir", "runtime_dir"]
            .into_iter()
            .collect(),
        "sources sub-object must have exactly the 3 env-var \
             keys; a contributor who adds a 4th (e.g. \
             `PLUGIN3_CONFIG_FILE`) without updating this pin \
             surfaces here. got: {source_keys:?}"
    );
    let config_src = sources["config_dir"]
        .as_str()
        .expect("config_dir source must be a string");
    assert!(
        config_src.starts_with("env PLUGIN3_CONFIG_DIR="),
        "env-set var must use the `env VAR=` prefix; bare path \
             or `env:` (no space) breaks the audit affordance. got: \
             {config_src:?}"
    );
    assert!(
        config_src.contains(&custom_cfg.path().to_string_lossy().to_string()),
        "config_dir source must include the custom tempdir path \
             passed via PLUGIN3_CONFIG_DIR; the env var isn't being \
             read on the JSON path. got: {config_src:?}"
    );
    assert_eq!(
        sources["data_dir"], "XDG default",
        "data_dir with no PLUGIN3_DATA_DIR set must report \
             `XDG default`; a contributor who flips the default \
             string (e.g. to `default`) breaks the audit signal. \
             got: {:?}",
        sources["data_dir"]
    );
    assert_eq!(
        sources["runtime_dir"], "XDG default",
        "runtime_dir with no PLUGIN3_RUNTIME_DIR set must report \
             `XDG default`. got: {:?}",
        sources["runtime_dir"]
    );
}

#[test]
fn config_validate_human_branch_emits_check_table_then_summary() {
    // ponytail: subprocess pin for `kf-budget config --validate`
    // on the human-readable (non-JSON) branch. The JSON
    // sibling family
    // (`config_validate_json_envelope_shape_is_pinned`,
    // `config_validate_json_status_fail_snake_case_is_pinned`,
    // `config_validate_exits_78_on_corrupt_config`) pins the
    // JSON envelope, the snake_case `"fail"` status string,
    // and the EX_CONFIG (78) exit code. The human branch goes
    // through `commands::config::validate(...)` directly,
    // emitting N check-table lines (`{status}  {label:<22}  \
    // {path}  ({detail})`), then a `---` separator, then a
    // summary line (`all N checks passed` or `F of N path \
    // checks failed`).
    //
    // The format strings are the contract: a contributor who
    // narrows the label column from 22 to 12 clips
    // `recent_outputs` (14 chars) into the path; a
    // contributor who changes the status prefix from `OK  `
    // (4 chars, 2 trailing spaces) to `OK ` (3 chars) breaks
    // the column alignment of every line. Both drift modes
    // are caught here at the subprocess boundary.
    //
    // The default `run_path_checks` returns 8 checks (3
    // directories + 5 file/parent paths; see
    // `commands/config.rs::run_path_checks`). A contributor
    // who adds a 9th path surface to the checks Vec
    // surfaces here as 11 lines.
    let (out, _c, _d, _r) = run_cli_subprocess(&["config", "--validate"]);
    assert!(
        out.status.success(),
        "config --validate must exit 0 on a clean tempdir; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    // ponytail: count the check-table lines. The default
    // `run_path_checks` walks 8 entries (3 dirs +
    // `config_file`, `budget_file`, `slices_dir`,
    // `usage_log`, `recent_outputs`). Then there's a `---`
    // separator and a summary line. So expect 8 + 1 + 1 = 10
    // lines on a clean tempdir. A contributor who adds a
    // 9th entry surfaces here as 11 lines; one who drops an
    // entry surfaces as 9.
    assert_eq!(
        lines.len(),
        10,
        "human branch on a clean tempdir must emit 8 check \
             lines + `---` + summary = 10 lines; got: {lines:?}"
    );

    // ponytail: pin the separator position. After the 8
    // check lines (lines 0..7), line 8 is the `---`
    // separator, line 9 is the summary.
    assert_eq!(
        lines[8], "---",
        "line[8] must be the literal `---` separator; \
             got: {:?}",
        lines[8]
    );
    assert_eq!(
        lines[9], "all 8 checks passed",
        "line[9] must be `all 8 checks passed` (the \
             clean-tempdir summary); got: {:?}",
        lines[9]
    );

    // ponytail: pin the per-check line shape. Each of the 8
    // check lines starts with the status prefix (`OK  ` or
    // `FAIL`), then two spaces (the literal between the
    // status and the label), then the label left-aligned to
    // 22 chars, then two spaces (the literal between the
    // label and the path), then the path, then two spaces,
    // then `({detail})`. The clean-tempdir arm has all
    // `OK  ` rows (every check passes on a freshly created
    // tempdir).
    //
    // The labels in order (from `run_path_checks`):
    //   config_dir, data_dir, runtime_dir, config_file,
    //   budget_file, slices_dir, usage_log, recent_outputs
    // (note: `slices_dir`, `usage_log`, `recent_outputs` go
    // through `check_file_parent` rather than `check_file`
    // because the files don't exist yet).
    let expected_labels = [
        "config_dir",
        "data_dir",
        "runtime_dir",
        "config_file",
        "budget_file",
        "slices_dir",
        "usage_log",
        "recent_outputs",
    ];
    assert_eq!(
        expected_labels.len(),
        8,
        "internal: 8 labels expected, matching the line \
             count of 8 check rows; if this fails, the line \
             count and labels pin have drifted apart"
    );
    for (i, label) in expected_labels.iter().enumerate() {
        let line = lines[i];
        // ponytail: status prefix. The status is `OK  ` (4
        // chars: O, K, space, space) on a clean tempdir.
        // The 2-space gap is the literal in the format
        // string after `{status}`. So the prefix is
        // `OK    ` (4 + 2 = 6 chars).
        assert!(
            line.starts_with("OK    "),
            "line[{i}] must lead with `OK    ` (status `OK  ` \
                 + 2-space gap to the label); got: {line:?}"
        );
        // ponytail: the label must appear at column 6 with
        // 22-char padding. The substring `{label:<22}` is
        // left-aligned to 22 chars, so the next character
        // after the label is a space (if the label is < 22
        // chars) or the path (if it's exactly 22).
        // `recent_outputs` (14 chars) gets 8 spaces of pad;
        // `config_dir` (10 chars) gets 12 spaces of pad.
        let after_status = &line[6..];
        assert!(
            after_status.starts_with(&format!("{label:<22}")),
            "line[{i}] after the status prefix must lead with \
                 `{label:<22}` (22-char pad); got: {line:?}"
        );
    }

    // ponytail: pin the path suffix on each check line.
    // The path value is a tempdir prefix (randomised) so
    // we can't pin the full path; we pin the file-name
    // suffix that the wrapper cares about (the file path
    // is `tempdir/<filename>` and we know the filename).
    // For directories (`config_dir`, `data_dir`,
    // `runtime_dir`) we assert the label substring appears
    // (the path itself is just the tempdir).
    assert!(
        lines[0].contains('(') && lines[0].ends_with(')'),
        "line[0] must end with `(<detail>)`; got: {:?}",
        lines[0]
    );
    assert!(
        lines[3].contains("config.toml"),
        "line[3] (config_file) must include `config.toml` \
             in the path; got: {:?}",
        lines[3]
    );
    assert!(
        lines[4].contains("budget.toml"),
        "line[4] (budget_file) must include `budget.toml` \
             in the path; got: {:?}",
        lines[4]
    );
    assert!(
        lines[5].contains("slices"),
        "line[5] (slices_dir) must include `slices`; \
             got: {:?}",
        lines[5]
    );
    assert!(
        lines[6].contains("usage.jsonl"),
        "line[6] (usage_log) must include `usage.jsonl`; \
             got: {:?}",
        lines[6]
    );
    assert!(
        lines[7].contains("recent_outputs.jsonl"),
        "line[7] (recent_outputs) must include \
             `recent_outputs.jsonl`; got: {:?}",
        lines[7]
    );

    // ponytail: negative pin. The JSON sibling's status
    // string (`"fail"`, snake_case) MUST NOT appear on
    // the human branch — the human branch uses Debug
    // format for `CheckStatus` (`OK` / `Fail`, PascalCase).
    // A contributor who unifies the two branches to share
    // the JSON path's `serde_json::to_string` builder
    // surfaces here as `"fail"` leaking into stdout.
    assert!(
        !stdout.contains("\"fail\""),
        "human branch must NOT emit the JSON sibling's \
             `\"fail\"` snake_case status string; got: {stdout:?}"
    );
    assert!(
        !stdout.contains("\"ok\""),
        "human branch must NOT emit the JSON sibling's \
             `\"ok\"` snake_case status string; got: {stdout:?}"
    );
    assert!(
        !stdout.contains('{'),
        "human branch must NOT emit JSON envelope markers; \
             got: {stdout:?}"
    );

    // ponytail: arm 2 — corrupt config triggers FAIL rows
    // and the failure summary. This exercises the FAIL
    // status prefix (`FAIL` with no trailing spaces,
    // 4 chars), the `F of N path checks failed` summary,
    // and the EX_CONFIG (78) exit code from the
    // `exit_config_err` route. The exit-code pin is
    // already covered by `config_validate_exits_78_on_corrupt_config`;
    // we focus on the human branch's render here.
    let (out, _cfg) = run_cli_subprocess_with_corrupt_file(
        &["config", "--validate"],
        b"this is = not [ valid",
        "config",
        "config.toml",
    );
    assert!(
        !out.status.success(),
        "corrupt config must exit non-zero (78)"
    );
    assert_eq!(
        out.status.code(),
        Some(78),
        "EX_CONFIG (78) is the documented exit code for \
             config parse failures; got: {:?}",
        out.status.code()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    // ponytail: the summary line on the failure arm is
    // `1 of 8 checks failed` (the corrupt config
    // produces one FAIL row; the literal word is
    // `checks`, not `path checks` — that's a
    // contrib-out-of-sync hazard worth pinning
    // explicitly).
    let summary = lines.last().expect("at least the summary");
    assert!(
        summary.starts_with("1 of 8 checks failed"),
        "failure summary must be `1 of 8 checks failed` \
             (one FAIL row for the corrupt config.toml); \
             got: {summary:?}"
    );
    // ponytail: the FAIL prefix is `FAIL` (4 chars, no
    // trailing spaces in the literal). The 2-space gap
    // between status and label comes from the format
    // string (`{status}  `), so the full prefix to the
    // label is `FAIL  ` (4 + 2 = 6 chars). Note this
    // differs from the OK prefix in length-by-coincidence:
    // `OK  ` is 4 chars (OK + 2 trailing spaces inside
    // the literal), so the OK+gap prefix is also 6 chars
    // (`OK    `). Both share the same total width —
    // a contributor who switches to `format!("{status:<4}")`
    // to right-pad would surface here as either `FAIL  `
    // (OK) or `FAIL    ` (right-padded, broken).
    let corrupt_row = lines
        .iter()
        .find(|l| l.starts_with("FAIL"))
        .expect("must have at least one FAIL row");
    assert!(
        corrupt_row.starts_with("FAIL  "),
        "FAIL row must lead with `FAIL  ` (status `FAIL` \
             + 2-space gap to label); got: {corrupt_row:?}"
    );
    assert!(
        corrupt_row.contains("config_file"),
        "the FAIL row should be the config_file check \
             (the corrupt config.toml); got: {corrupt_row:?}"
    );
    assert!(
        corrupt_row.contains("parse failed"),
        "the FAIL row's detail should mention `parse failed`; \
             got: {corrupt_row:?}"
    );
}

#[test]
fn config_show_human_branch_emits_8_padded_label_lines_without_sources() {
    // ponytail: subprocess pin for `kf-budget config` on the
    // human-readable (non-JSON) branch. The JSON sibling
    // (`config_show_json_envelope_includes_sources_when_show_sources_passed`)
    // pins the 8-key envelope (or 9 with `--show-sources`).
    // The human branch goes through
    // `commands::config::show(...)` directly, emitting 8 lines
    // with `{k:<16} {path}` padding, plus an optional `---`
    // separator + 3 env-source lines when `--show-sources` is
    // passed. The format is the contract: a contributor who
    // changes the column width from 16 to 8 (or 24) breaks
    // every wrapper that does `awk '{print $1}'` on the
    // rendered output.
    //
    // Two arms:
    //   no --show-sources → 8 lines, no `---` separator,
    //                        no JSON envelope markers
    //   --show-sources    → 8 + 1 (---) + 3 = 12 lines,
    //                        with `config_dir:`/`data_dir:`/
    //                        `runtime_dir:` env-source lines
    //
    // The 16-char pad matters: `recent_outputs` is the
    // longest label at 14 chars; `{:<16}` pads it to 16.
    // A contributor who narrows to `{:<12}` would clip the
    // `recent_outputs` label (last 4 chars land on the value
    // side, contaminating path parsing). The label-content
    // pin below catches that.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let out = std::process::Command::new(kf_budget_binary_path())
        // NOTE: no `--json`. Human branch.
        .args(["config"])
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget config (human)");
    assert!(
        out.status.success(),
        "config (human) must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    // ponytail: pin the exact line count. The human branch
    // emits 8 lines (one per path in the `pairs` Vec) — no
    // `---` separator, no env-source block (those are
    // gated on `--show-sources`). A contributor who adds
    // a 9th path to the Vec without updating this pin
    // surfaces here as 9 lines.
    assert_eq!(
        lines.len(),
        8,
        "human branch without --show-sources must emit \
             exactly 8 lines (one per path in `pairs`); got: {lines:?}"
    );

    // ponytail: pin the label-order and `{:<16}` padding. The
    // labels in order are: config_dir, data_dir, runtime_dir,
    // config_file, budget_file, slices_dir, usage_log,
    // recent_outputs. Each label is left-aligned to 16 chars
    // (the longest label `recent_outputs` is 14 chars, padded
    // with 2 spaces). The pinned substrings check that the
    // label appears with at least one trailing space before
    // the path (the `{:<16}` formatter pads with spaces, then
    // `format!` adds one more space before the value).
    let expected_labels = [
        "config_dir",
        "data_dir",
        "runtime_dir",
        "config_file",
        "budget_file",
        "slices_dir",
        "usage_log",
        "recent_outputs",
    ];
    for (i, label) in expected_labels.iter().enumerate() {
        let line = lines[i];
        // ponytail: pin the column-pad-and-space pattern.
        // `{:<16}` left-aligns to 16 chars, then `format!`
        // adds one more space (the literal between the two
        // `{}` placeholders). So the gap between label and
        // path is `16 - label.len() + 1` chars. For
        // `recent_outputs` (14 chars): 16 - 14 + 1 = 3
        // spaces. For `data_dir` (8 chars): 16 - 8 + 1 = 9
        // spaces. We check for at least one space after
        // the label and that the label prefix is followed by
        // a separator before the path.
        assert!(
            line.starts_with(&format!("{label:<16}")),
            "line[{i}] must lead with `{label:<16}` (left-aligned \
                 to 16 chars); got: {line:?}"
        );
    }

    // ponytail: pin the path values. Each line carries the
    // path value (a tempdir prefix that varies per run). We
    // verify each line has a non-empty path after the label
    // (the path is `{label:<16} {path}` — at least one char
    // of path after the trailing space). The tempdir prefix
    // is randomised per run; we don't pin it.
    for (i, line) in lines.iter().enumerate() {
        let label_part = &line[..16.min(line.len())];
        let rest = line[16.min(line.len())..].trim_start();
        assert!(
            !rest.is_empty(),
            "line[{i}] must carry a non-empty path after the \
                 `{label_part}` label; got: {line:?}"
        );
    }

    // ponytail: negative pin. Without --show-sources the
    // human branch MUST NOT emit:
    //   - the `---` separator that gates the env-source block
    //   - the JSON envelope markers (`{`, `}`)
    //   - the env-source labels (`config_dir:`, `data_dir:`,
    //     `runtime_dir:` — note these would overlap with the
    //     path labels but use a different format: `: ` vs
    //     `:<16} `)
    assert!(
        !stdout.contains("---"),
        "human branch without --show-sources must NOT emit the \
             `---` separator (that's gated on --show-sources); \
             got: {stdout:?}"
    );
    assert!(
        !stdout.contains('{'),
        "human branch must NOT emit JSON envelope markers; \
             got: {stdout:?}"
    );
    // The env-source block uses `config_dir:    ` (12-char pad)
    // rather than `{:<16}` (16-char pad). A contributor who
    // makes the separator block unconditional surfaces here
    // as the 12-char-padded labels appearing.
    for env_label in ["config_dir:    ", "data_dir:      ", "runtime_dir:   "] {
        assert!(
            !stdout.contains(env_label),
            "human branch without --show-sources must NOT emit \
                 the env-source label `{env_label}` (12-char pad); \
                 got: {stdout:?}"
        );
    }

    // ponytail: arm 2 — with --show-sources. 8 path lines +
    // 1 separator + 3 env-source lines = 12 lines. The
    // env-source lines use a different padding scheme
    // (`config_dir:    ` is 12-char-padded, vs the path
    // lines' `{:<16}` 16-char padding). A contributor who
    // re-aligns both to 16 surfaces here as the env-source
    // labels gaining more spaces.
    let cfg2 = tempfile::tempdir().expect("cfg tempdir 2");
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["config", "--show-sources"])
        .env("PLUGIN3_CONFIG_DIR", cfg2.path())
        // Remove PLUGIN3_DATA_DIR / PLUGIN3_RUNTIME_DIR so
        // those arms report "XDG default" (matches the
        // JSON sibling's pattern).
        .env_remove("PLUGIN3_DATA_DIR")
        .env_remove("PLUGIN3_RUNTIME_DIR")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget config --show-sources (human)");
    assert!(
        out.status.success(),
        "config --show-sources (human) must exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        12,
        "human branch with --show-sources must emit \
             8 + 1 (`---`) + 3 = 12 lines; got: {lines:?}"
    );
    // ponytail: pin the separator position. After the 8 path
    // lines (lines 0..7), line 8 is `---`, then lines 9..11
    // are the env-source block.
    assert_eq!(
        lines[8], "---",
        "line[8] must be the literal `---` separator; \
             got: {:?}",
        lines[8]
    );
    // ponytail: pin the env-source label padding. The labels
    // here are 12-char-padded (NOT 16) — `config_dir:` is 11
    // chars, padded to 12 with 1 space; `data_dir:` is 9
    // chars, padded to 12 with 3 spaces; `runtime_dir:` is
    // 11 chars, padded to 12 with 1 space. A contributor
    // who switches these to 16-char padding (matching the
    // path lines) surfaces here.
    assert!(
        lines[9].starts_with("config_dir:    "),
        "line[9] (env-source config_dir) must lead with \
             `config_dir:    ` (12-char pad); got: {:?}",
        lines[9]
    );
    assert!(
        lines[10].starts_with("data_dir:      "),
        "line[10] (env-source data_dir) must lead with \
             `data_dir:      ` (12-char pad); got: {:?}",
        lines[10]
    );
    assert!(
        lines[11].starts_with("runtime_dir:   "),
        "line[11] (env-source runtime_dir) must lead with \
             `runtime_dir:   ` (12-char pad); got: {:?}",
        lines[11]
    );
    // ponytail: pin the env-source values. config_dir is
    // env-set (custom tempdir path); data_dir and runtime_dir
    // are XDG default (no env var set).
    assert!(
        lines[9].contains(&cfg2.path().to_string_lossy().to_string()),
        "env-source config_dir line must include the custom \
             tempdir path passed via PLUGIN3_CONFIG_DIR; \
             got: {:?}",
        lines[9]
    );
    assert!(
        lines[9].starts_with("config_dir:    env PLUGIN3_CONFIG_DIR="),
        "env-source config_dir must use the `env VAR=` prefix; \
             a bare path here breaks the audit affordance. \
             got: {:?}",
        lines[9]
    );
    assert_eq!(
        lines[10], "data_dir:      XDG default",
        "env-source data_dir with no PLUGIN3_DATA_DIR must be \
             exactly `data_dir:      XDG default` (12-char pad); \
             got: {:?}",
        lines[10]
    );
    assert_eq!(
        lines[11], "runtime_dir:   XDG default",
        "env-source runtime_dir with no PLUGIN3_RUNTIME_DIR \
             must be exactly `runtime_dir:   XDG default` \
             (12-char pad); got: {:?}",
        lines[11]
    );
}
