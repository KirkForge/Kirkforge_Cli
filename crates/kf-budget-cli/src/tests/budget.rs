use super::*;

#[test]
fn budget_status_emits_human_by_default() {
    // ponytail: pin the human format. A contributor who switches
    // the default to JSON breaks every shell alias that greps
    // for "used:". The exact phrase is the contract.
    let (out, _c, _d, _r) = run_cli_subprocess(&["budget", "status"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("used: "), "got: {stdout}");
    assert!(
        !stdout.contains('{'),
        "human output must not contain JSON: {stdout}"
    );
}

#[test]
fn budget_status_human_branch_full_line_shape_and_pascal_case_state() {
    // ponytail: pin the EXACT human-branch line shape on
    // `kf-budget budget status` (no `--json`):
    //   used: <used> / <ceiling> (<State>)
    // where `<State>` is the `Debug`-formatted variant — `Under`,
    // `Approaching`, `Over` — PascalCase. The existing
    // `budget_status_emits_human_by_default` only checks
    // `starts_with("used: ")` and that there's no `{`, which
    // is enough to detect "JSON leaked into the human branch"
    // but doesn't pin:
    //   - the field separator (" / ", not "/" or "of")
    //   - the trailing parenthesised state
    //   - the PascalCase state spelling (Debug, not serde)
    // A contributor who flips the human branch to use the
    // JSON branch's `serde_json::to_string_pretty` builder
    // (or who replaces `{:?}` with `{}` after a serde rename
    // to snake_case) would silently change the wire form
    // from `Under` to `under` on this branch, breaking every
    // `grep "(Approaching)"` wrapper.
    //
    // Three arms cover all three `BudgetState` variants —
    // mirroring the JSON sibling
    // (`budget_status_json_state_approaching_and_over_are_pinned`)
    // which pins the JSON branch's snake_case spellings.
    // Together the two tests pin the LOAD-BEARING divergence:
    // the human branch uses Debug (PascalCase), the JSON
    // branch uses serde (snake_case). The same `BudgetState`
    // value renders as `Under` on stdout and `"under"` in the
    // JSON envelope — intentional, not a bug.
    for (used, ceiling, expected_state) in [
        (0usize, 200_000usize, "Under"),
        (80, 100, "Approaching"),
        (100, 100, "Over"),
    ] {
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        // ponytail: write the runtime budget.toml with the
        // seeded values. `approaching_ratio` defaults to 0.8
        // via `TokenBudget::default()`, and the test harness
        // here doesn't seed config.toml so the default
        // applies. The state ratios come from
        // `BudgetState::state()` in kf-budget-core/src/budget.rs:
        //   ratio >= 1.0                    → Over
        //   ratio >= self.approaching_ratio → Approaching
        //   else                            → Under
        let budget_path = runtime_dir.path().join("budget.toml");
        let seed = TokenBudget {
            ceiling,
            approaching_ratio: 0.8,
            used,
        };
        std::fs::write(&budget_path, toml::to_string(&seed).unwrap()).unwrap();

        let out = std::process::Command::new(kf_budget_binary_path())
            // NOTE: no `--json`. The human branch uses
            // `println!("used: {} / {} ({:?})", ...)`, which
            // Debug-formats the state as PascalCase — distinct
            // from the JSON branch's snake_case serde form.
            .args(["budget", "status"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn kf-budget budget status (human)");
        assert!(
            out.status.success(),
            "budget status must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.trim_end_matches('\n').trim_end();
        assert_eq!(
            line,
            format!("used: {used} / {ceiling} ({expected_state})"),
            "human branch must emit EXACTLY `used: {used} / \
                 {ceiling} ({expected_state})` — the field separator \
                 is ` / ` (slash-space-space), the state is in \
                 parentheses, and the state spelling is PascalCase \
                 (Debug format, not serde snake_case). got: {line:?}"
        );
        // ponytail: negative pin — the JSON-branch snake_case
        // form MUST NOT leak into the human branch. A
        // contributor who replaces the Debug format with the
        // JSON branch's snake_case value (e.g. via a serde
        // rename on `BudgetState` plus a switch to `{}`)
        // surfaces here as `under` instead of `Under`.
        assert!(
            !stdout.contains("(under)"),
            "human branch must NOT emit the snake_case `(under)` \
                 form (that's the JSON branch's spelling); got: {stdout:?}"
        );
        assert!(
            !stdout.contains("(approaching)"),
            "human branch must NOT emit `(approaching)` \
                 (snake_case); got: {stdout:?}"
        );
        assert!(
            !stdout.contains("(over)"),
            "human branch must NOT emit `(over)` (snake_case); \
                 got: {stdout:?}"
        );
    }
}

#[test]
fn budget_status_emits_json_when_json_flag_set() {
    // ponytail: --json is the scriptable path. Pin both the
    // top-level keys AND the snake_case enum spelling — a reader
    // of `report --kind` filters on the same spellings.
    let (out, _c, _d, _r) = run_cli_subprocess(&["--json", "budget", "status"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    let obj = v.as_object().expect("top-level object");
    let keys: std::collections::BTreeSet<&str> =
        obj.keys().map(std::string::String::as_str).collect();
    assert_eq!(
        keys,
        ["ceiling", "state", "used"].into_iter().collect(),
        "field set drifted from ADR-0015",
    );
    assert_eq!(v["state"], "under");
}

#[test]
fn budget_set_emits_json_with_ceiling_and_persisted_default() {
    // ponytail: pin the `kf-budget --json budget set N` wire
    // shape. The CLI builds
    //   `{"ceiling": N, "persisted_default": bool}`
    // — two top-level keys. The boolean distinguishes "session-
    // local change" from "wrote the default to config.toml" so
    // a wrapper script can audit which `set` calls persisted
    // without scraping stderr. A contributor who adds a sibling
    // key (e.g. `"path": "..."`) or renames `persisted_default`
    // → `wrote_default` breaks the audit affordance silently —
    // `jq '.persisted_default'` returns null, no error. Drift
    // catches here.
    //
    // Subprocess invocation is inlined (the existing
    // `run_budget_set_subprocess` helper does not pass `--json`)
    // because the helper's purpose is persistence assertions;
    // this test cares about stdout shape, not config.toml.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "budget", "set", "275000", "--default"])
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
    let obj = v.as_object().expect("top-level object");
    let keys: std::collections::BTreeSet<&str> =
        obj.keys().map(std::string::String::as_str).collect();
    assert_eq!(
        keys,
        ["ceiling", "persisted_default"].into_iter().collect(),
        "budget set --json top-level key set must be exactly \
             {{ceiling, persisted_default}}; a contributor who adds \
             a sibling key (or renames `persisted_default`) breaks \
             downstream `jq` audits. got: {keys:?}",
    );
    assert_eq!(
        v["ceiling"], 275_000,
        "ceiling must echo the argv value verbatim (no formatting)"
    );
    assert_eq!(
        v["persisted_default"], true,
        "persisted_default must be true when --default is passed; \
             `false` here means the --default wiring dropped the \
             persistence call silently"
    );
}

#[test]
fn budget_set_emits_json_with_persisted_default_false_when_flag_omitted() {
    // ponytail: pin the dual-case of the prior test. Without
    // --default, `persisted_default` is false — a wrapper that
    // greps `jq '.persisted_default == false'` for the
    // session-local branch is load-bearing. A contributor who
    // wires the field to always-true (e.g. dropping the
    // `if persist_default` from the JSON build) silently
    // misleads every audit consumer.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(["--json", "budget", "set", "125000"])
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
    assert_eq!(v["ceiling"], 125_000);
    assert_eq!(
        v["persisted_default"], false,
        "persisted_default must be false when --default is omitted; \
             a contributor who flips the boolean (or always writes \
             config.toml) surfaces here"
    );
    // ponytail: also assert no config.toml was written — the
    // JSON field is the audit signal, but a double-check on
    // disk catches a regression that wires the boolean wrong
    // AND writes config.toml anyway.
    assert!(
        !cfg_dir.path().join("config.toml").exists(),
        "config.toml must NOT be written when --default is omitted, \
             regardless of what the JSON reports"
    );
}

#[test]
fn budget_set_emits_human_branch_one_or_two_lines_per_default_flag() {
    // ponytail: subprocess pin for `kf-budget budget set <N>` on
    // the human-readable (non-JSON) branch. The JSON sibling
    // (`budget_set_emits_json_with_ceiling_and_persisted_default`
    // and its dual-case) pin the JSON envelope's
    // `persisted_default` boolean and `ceiling` value. The
    // human branch emits either one or two stdout lines
    // depending on `--default`:
    //   no --default   → `ceiling set to <N>`         (1 line)
    //   --default      → above PLUS `default persisted to <path>`
    //                                                 (2 lines)
    // The line count is the load-bearing contract: a wrapper
    // that runs `kf-budget budget set --default 200000 &&
    // wc -l` to verify persistence emits 2 lines for a
    // persisted write and 1 line for a session-local write.
    // A contributor who always emits 2 lines (or who swaps
    // the conditional to `if !persist_default`) breaks that
    // count silently.
    //
    // The first-line prefix `ceiling set to ` and the
    // second-line prefix `default persisted to ` are also
    // load-bearing — wrapper scripts grep for both. The
    // numeric value `<N>` is rendered via `{ceiling}` (the
    // `usize` Display impl), which is decimal with no
    // thousands separator — a contributor who adds a
    // thousands separator (e.g. `200,000`) breaks the
    // `grep -E 'set to [0-9]+$'` shell pattern.
    let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
    let data_dir = tempfile::tempdir().expect("data tempdir");
    let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
    let run = |extra: &[&str]| -> std::process::Output {
        std::process::Command::new(kf_budget_binary_path())
            // NOTE: no `--json`. Human branch.
            .args(
                ["budget", "set", "150000"]
                    .iter()
                    .chain(extra.iter())
                    .copied()
                    .collect::<Vec<_>>(),
            )
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn kf-budget budget set (human)")
    };

    // Arm 1: no --default → exactly 1 stdout line.
    // The line must start with `ceiling set to ` and carry
    // the ceiling value (150000) decimal with no thousands
    // separator. The forbidden `default persisted` prefix
    // must NOT appear — that's the --default-only line.
    let out = run(&[]);
    assert!(
        out.status.success(),
        "budget set 150000 (no --default) must exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "human branch without --default must emit exactly 1 \
             line (`ceiling set to <N>`); got: {lines:?}"
    );
    let line = lines[0];
    assert!(
        line.starts_with("ceiling set to "),
        "human branch line must lead with `ceiling set to `; \
             got: {line:?}"
    );
    assert!(
        line.contains("150000"),
        "human branch line must carry the ceiling value \
             (150000) decimal, no thousands separator; got: {line:?}"
    );
    assert!(
        !line.contains("150,000"),
        "human branch must NOT add a thousands separator \
             (the value is decimal `Display` on `usize`); got: {line:?}"
    );
    // ponytail: the --default-only second line MUST NOT
    // appear under the no-default arm. A contributor who
    // swaps the conditional to `if !persist_default`
    // surfaces here as 2 lines, the second starting with
    // `default persisted to`.
    assert!(
        !stdout.contains("default persisted"),
        "human branch without --default must NOT emit the \
             `default persisted to ...` line; a leak means the \
             conditional was inverted. stdout: {stdout:?}"
    );
    // ponytail: the JSON sibling's `persisted_default` boolean
    // must not leak into the human branch as a JSON literal.
    assert!(
        !stdout.contains('{'),
        "human branch must NOT emit JSON envelope markers; \
             got: {stdout:?}"
    );

    // Arm 2: --default → exactly 2 stdout lines. The first
    // is the same `ceiling set to 150000` line; the second
    // is `default persisted to <path>` where `<path>` is
    // `cfg_dir/config.toml` (resolved via `config_path()`,
    // which routes through `PLUGIN3_CONFIG_DIR`). The path
    // itself isn't pinned — `config_path()` is a function
    // call and the path includes the tempdir prefix which
    // varies per run — but the prefix and the file name
    // suffix are pinned.
    let out = run(&["--default"]);
    assert!(
        out.status.success(),
        "budget set 150000 --default must exit 0; \
             stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "human branch with --default must emit exactly 2 \
             lines (`ceiling set to <N>` and `default persisted to \
             <path>`); got: {lines:?}"
    );
    // ponytail: pin the first line shape. Same as arm 1.
    assert!(
        lines[0].starts_with("ceiling set to 150000"),
        "first line under --default must lead with `ceiling \
             set to 150000`; got: {:?}",
        lines[0]
    );
    // ponytail: pin the second line shape. The prefix
    // `default persisted to ` is the contract; the path
    // suffix `config.toml` is the file-name contract.
    // A contributor who drops the path display
    // (e.g. shortens to `println!("default persisted")`)
    // surfaces here — a `grep -c persisted to` wrapper
    // would still match, but an audit tool that reads the
    // path off the second line would lose its signal.
    assert!(
        lines[1].starts_with("default persisted to "),
        "second line under --default must lead with \
             `default persisted to ` (note trailing space); \
             got: {:?}",
        lines[1]
    );
    assert!(
        lines[1].ends_with("config.toml"),
        "second line must end with the config file name \
             (`config.toml`); got: {:?}",
        lines[1]
    );
    // ponytail: disk-level double-check. The audit signal
    // on the JSON branch is the boolean; here it's the
    // file existing. A contributor who breaks the
    // conditional so the path message is printed but no
    // write happens surfaces here.
    assert!(
        cfg_dir.path().join("config.toml").exists(),
        "config.toml MUST exist on disk after `budget set \
             --default`; the human branch's `default persisted \
             to ...` line is the user-facing mirror of this \
             file. Missing file means the write was skipped."
    );
}

#[test]
fn unknown_subcommand_exits_64() {
    // ponytail: clap returns 2 by default; ADR-0015 § Exit
    // codes prescribes 64 (EX_USAGE). `main()` routes
    // `Cli::try_parse_from` errors through `exit_usage_err`,
    // so an unknown subcommand surfaces as 64 in the
    // subprocess. A regression that calls `Cli::parse()`
    // directly would flip this back to 2.
    let (out, _c, _d, _r) = run_cli_subprocess(&["nonexistent-cmd"]);
    assert!(
        !out.status.success(),
        "unknown subcommand must exit non-zero, got success; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(64),
        "ADR-0015 prescribes 64 for usage errors; if this fails the exit wiring changed"
    );
}

#[test]
fn budget_status_json_state_approaching_and_over_are_pinned() {
    // ponytail: dual-arm pin for the non-Under BudgetState
    // variants on the JSON path. The existing
    // `budget_status_emits_json_when_json_flag_set` only
    // exercises the `"under"` arm (fresh tempdir, used=0).
    // `BudgetState` is `Under | Approaching | Over` with
    // `#[serde(rename_all = "snake_case")]` — three
    // independent wire spellings. A contributor who flips
    // `"approaching"` → `"warning"` (or `"over"` → `"exceeded"`)
    // breaks every `jq '.state == "approaching"'` filter
    // silently. Drift catches here.
    //
    // budget.toml is `runtime_dir/budget.toml` (ADR-0014 § B2);
    // the runtime loader at `load_budget_with_config` parses it
    // via `toml::from_str::<TokenBudget>`. Two seeded values
    // cover the two interesting ratios:
    //   ceiling=100, used=80  → ratio=0.80 ≥ approaching_ratio
    //                            (default 0.8) → Approaching
    //   ceiling=100, used=100 → ratio=1.00 ≥ 1.0          → Over
    // The `used` value also carries through to the JSON
    // payload — pinning it catches a regression where
    // `state` is computed but `used` is hardcoded to 0 in
    // the wire builder.
    for (used, expected_state) in [(80usize, "approaching"), (100usize, "over")] {
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let runtime_dir = tempfile::tempdir().expect("runtime tempdir");
        // ponytail: write the runtime budget.toml. The
        // default `approaching_ratio` (0.8) is preserved by
        // NOT seeding config.toml — `load_budget_with_config`
        // leaves `b.approaching_ratio` at its Default value
        // when config.toml is absent. The seeded `used`
        // value crosses the threshold for the expected
        // state.
        let budget_path = runtime_dir.path().join("budget.toml");
        let seed = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used,
        };
        std::fs::write(&budget_path, toml::to_string(&seed).unwrap()).unwrap();

        let out = std::process::Command::new(kf_budget_binary_path())
            .args(["--json", "budget", "status"])
            .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
            .env("PLUGIN3_DATA_DIR", data_dir.path())
            .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn kf-budget");
        assert!(
            out.status.success(),
            "budget status must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
        assert_eq!(
            v["state"], expected_state,
            "with ceiling=100, used={used}, expected state \
                 `{expected_state}` (snake_case BudgetState variant); \
                 a different value here means the serde rename \
                 drifted OR the ceiling/used values didn't load from \
                 budget.toml (the runtime file is silently ignored). \
                 got: {:?}",
            v["state"]
        );
        // ponytail: pin that `used` carries the seeded value,
        // not a hardcoded 0. A contributor who rewires the
        // wire builder to emit `used: 0` regardless of state
        // keeps the state transition pin green and loses the
        // diagnostic dashboards care about.
        assert_eq!(
            v["used"], used,
            "used must carry the seeded {used} through to the \
                 JSON payload; `used: 0` here means the wire builder \
                 hardcoded the counter and broke the audit signal. \
                 got: {}",
            v["used"]
        );
        assert_eq!(
            v["ceiling"], 100,
            "ceiling must carry the seeded 100 through; a \
                 different value means the runtime budget.toml was \
                 bypassed (default ceiling is 200_000)"
        );
    }
}

#[test]
fn budget_validate_exits_78_on_corrupt_budget_toml() {
    // ponytail: corrupt budget.toml must also surface as EX_CONFIG
    // (78). ADR-0015 § Exit codes names 78 the catch-all for
    // "config parse or backend init failure"; budget.toml is a
    // sibling surface to config.toml — both flow through
    // `run_path_checks`'s `parse_existing` callback. A contributor
    // who drops `parse_budget_at` from the check list (or
    // changes it to swallow errors) keeps the corrupt-config test
    // green and silently allows a corrupt runtime budget to ship
    // — caught here because the failure count goes to zero and
    // validate would exit 0.
    let (out, _cfg) = run_cli_subprocess_with_corrupt_file(
        &["config", "--validate"],
        b"this is = not [ valid",
        "runtime",
        "budget.toml",
    );
    assert!(
        !out.status.success(),
        "corrupt budget.toml must exit non-zero; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(78),
        "corrupt budget.toml must exit 78 (EX_CONFIG) — same exit code \
             as corrupt config.toml; a contributor who wires a different \
             exit code for budget-parse failures breaks the ADR-0015 \
             contract here. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("FAIL"),
        "stdout must show the failing check; got: {stdout}"
    );
    // ponytail: also pin the label that appears in the FAIL
    // row. The check is registered as `budget_file` in
    // `run_path_checks`; a contributor who renames the
    // label (e.g. `runtime_budget`) breaks dashboard
    // scripts that grep the label by name.
    assert!(
        stdout.contains("budget_file"),
        "FAIL row must label the failing surface as `budget_file`; got: {stdout}"
    );
}

#[test]
fn help_text_includes_subcommand_descriptions() {
    // ponytail: ADR-0015 § Help output conventions requires a
    // one-line description on every subcommand. A contributor
    // who deletes a `///` doc comment from a variant breaks
    // the help output below; this test catches it before a
    // host script greps for the missing phrase. Each phrase
    // matches a `///` line on a HookKind or BudgetSub variant.
    let (out, _c, _d, _r) = run_cli_subprocess(&["hook", "--help"]);
    assert!(
        out.status.success(),
        "--help must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "Slice the tool result",
        "Check the budget",
        "Emit a `CompactHint`",
    ] {
        assert!(
            stdout.contains(needle),
            "hook --help missing {needle:?}; got:\n{stdout}"
        );
    }
    let (out, _c, _d, _r) = run_cli_subprocess(&["budget", "--help"]);
    assert!(out.status.success(), "--help must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Print the current budget state"),
        "budget --help missing Status description; got:\n{stdout}"
    );
}
