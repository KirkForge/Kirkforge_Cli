//! End-to-end smoke test — exercise the release binary as a real
//! subprocess would, the way Claude Code would invoke it. Pins the
//! wire contract at the *integration* level (not unit-mock level):
//! every assertion runs against the actual `plugin3` CLI binary
//! and verifies the bytes the host would observe.
//!
//! ponytail: one test file, one happy path. Three sub-process calls
//! in sequence against a single tempdir — `PostToolUse` slices a
//! large payload, budget set + status round-trips, report sees the
//! slice record. The user only needs to know "did the binary do the
//! right thing end-to-end" — not "did module X call module Y".
//! Each subprocess runs with PLUGIN3_*_DIR pointed at fresh tempdirs
//! so the test is hermetic and parallel-safe across runs.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn binary() -> PathBuf {
    // Cargo sets CARGO_BIN_EXE_plugin3 for integration tests; we
    // honour that path so `cargo test --workspace` picks up the
    // built binary without a separate path lookup.
    PathBuf::from(env!("CARGO_BIN_EXE_plugin3"))
}

struct FreshDirs {
    cfg: tempfile::TempDir,
    data: tempfile::TempDir,
    runtime: tempfile::TempDir,
}

impl FreshDirs {
    fn new() -> Self {
        Self {
            cfg: tempfile::tempdir().expect("cfg tempdir"),
            data: tempfile::tempdir().expect("data tempdir"),
            runtime: tempfile::tempdir().expect("runtime tempdir"),
        }
    }
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(binary());
        c.args(args)
            .env("PLUGIN3_CONFIG_DIR", self.cfg.path())
            .env("PLUGIN3_DATA_DIR", self.data.path())
            .env("PLUGIN3_RUNTIME_DIR", self.runtime.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
}

fn write_stdin(mut child: std::process::Child, payload: &[u8]) -> std::process::Output {
    child.stdin.take().unwrap().write_all(payload).unwrap();
    child.wait_with_output().expect("wait")
}

/// Happy-path end-to-end smoke test. Walks the binary through the
/// three load-bearing subcommands a real host interacts with —
/// `PostToolUse` (slice a 12 KB cargo-test body), budget set (persist
/// a new ceiling), budget status (read it back), report (verify the
/// slice record landed). Each call asserts on the literal output the
/// host would see, so a regression in any module surfaces as a real
/// wire-shape mismatch, not as a unit-test green light that doesn't
/// match the user-facing behaviour.
#[test]
fn end_to_end_post_tool_use_budget_report_round_trip() {
    let dirs = FreshDirs::new();

    // ---- 1. PostToolUse slices a 12 KB cargo-test body. ----
    let mut body = String::from("running 5 tests\ntest foo ... ok\n");
    body.push_str(&"y".repeat(12_000));
    body.push_str("\ntest bar ... FAILED\n");
    let payload = serde_json::json!({
        "tool_name": "cargo test",
        "tool_result_key": "k_smoke_1",
        "session_id": "smoke-session",
        "content": body,
    });
    let out = write_stdin(
        dirs.cmd(&["hook", "post-tool-use"])
            .spawn()
            .expect("spawn post-tool-use"),
        serde_json::to_vec(&payload).unwrap().as_slice(),
    );
    assert!(
        out.status.success(),
        "post-tool-use must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("post-tool-use stdout is JSON");
    // The orchestrator should have sliced (12 KB > 8 KB threshold).
    let content = v["content"].as_str().expect("response.content is a string");
    assert!(
        content.contains("<<plugin3:slice:"),
        "sliced output must carry the plugin3 marker; got: {content:?}"
    );
    assert!(
        content.ends_with("test bar ... FAILED\n"),
        "sliced output must preserve the tail; got: {content:?}"
    );
    let note = v["note"]
        .as_str()
        .expect("note is a non-null string on slice path");
    assert!(
        note.contains("sliced"),
        "note must describe the slice; got: {note:?}"
    );

    // ---- 2. Budget set --default persists the ceiling. ----
    let out = dirs
        .cmd(&["budget", "set", "150000", "--default", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn budget set");
    assert!(
        out.status.success(),
        "budget set must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("budget set --json stdout is JSON");
    assert_eq!(v["ceiling"], 150_000, "ceiling must round-trip; got: {v}");
    assert_eq!(v["persisted_default"], true);

    // ---- 3. Budget status reads back the new ceiling. ----
    let out = dirs
        .cmd(&["budget", "status", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn budget status");
    assert!(
        out.status.success(),
        "budget status must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("budget status --json stdout is JSON");
    assert_eq!(
        v["ceiling"], 150_000,
        "status must reflect the persisted ceiling; got: {v}"
    );
    assert_eq!(
        v["used"], 0,
        "fresh session has no recorded usage; got: {v}"
    );

    // ---- 4. Report sees the slice record from step 1. ----
    // The orchestrator emitted a UsageRecord only on the sliced path
    // (Keep decisions don't inflate report counts), so the slice
    // record should be in the usage.jsonl by the time we query.
    let out = dirs
        .cmd(&["report", "--summary", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn report --summary");
    assert!(
        out.status.success(),
        "report must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("report --summary --json stdout is JSON");
    let smoke = v["smoke-session"]
        .as_object()
        .expect("smoke-session bucket present");
    assert_eq!(
        smoke["records"], 1,
        "smoke-session must have 1 record (the slice); got: {smoke:?}"
    );
    let bytes_saved = smoke["bytes_saved"].as_u64().expect("bytes_saved is u64");
    assert!(
        bytes_saved > 0,
        "slice record must report bytes_saved > 0; got {bytes_saved}"
    );
}

/// Budget guard Slice path — drives the binary through the load-bearing
/// sequence that turns a small budget + a large prompt into a
/// `Intervention::Slice` recommendation. The whole point of the
/// `UserPromptSubmit` hook is to keep a runaway prompt from blowing the
/// ceiling; this test pins that the recommendation actually fires,
/// targets the largest recent output, and lands a slice record in the
/// usage.jsonl that `report --last` can read back.
///
/// ponytail: one file, one happy path per test. The previous
/// `end_to_end_smoke.rs` test pins the `PostToolUse` + report path; this
/// test pins the `UserPromptSubmit` + budget-guard Slice path. They
/// share the same `FreshDirs` helper but never run in the same process,
/// so each gets a clean tempdir.
#[test]
fn end_to_end_budget_guard_forces_slice_on_oversized_prompt() {
    let dirs = FreshDirs::new();

    // ---- 1. Set a tiny ceiling so a single prompt overflows it. ----
    // budget set --default persists into config.toml, so the next
    // user-prompt-submit (which loads budget from disk) picks it up.
    let out = dirs
        .cmd(&["budget", "set", "500", "--default", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn budget set");
    assert!(
        out.status.success(),
        "budget set must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("budget set --json stdout is JSON");
    assert_eq!(v["ceiling"], 500, "tiny ceiling must round-trip; got: {v}");

    // ---- 2. Populate recent_outputs.jsonl with a fat entry. ----
    // PostToolUse is the only hook that calls append_recent, so we
    // use it to seed the budget guard's view of "what's the largest
    // recent output I can auto-slice?". A 20 KB cargo-test body
    // gives a recent entry of size 20_000 — well above the
    // needed + SLICE_OVERHEAD threshold for a 500-token budget.
    //
    // ponytail: 20 KB slices in the orchestrator, so the key appended
    // to recent_outputs.jsonl is the slice marker
    // (`<<plugin3:slice:hash>>`), not `tool_result_key`. The budget
    // guard's `target_key` is whatever key was appended — the slice
    // marker — so step 3 pins the *marker prefix*, not
    // `tool_result_key`. The hash itself is content-derived, so
    // pinning the prefix is the strongest stable contract.
    let body = "y".repeat(20_000);
    let post_payload = serde_json::json!({
        "tool_name": "cargo test",
        "tool_result_key": "k_budget_smoke",
        "session_id": "budget-smoke",
        "content": body,
    });
    let out = write_stdin(
        dirs.cmd(&["hook", "post-tool-use"])
            .spawn()
            .expect("spawn post-tool-use"),
        serde_json::to_vec(&post_payload).unwrap().as_slice(),
    );
    assert!(
        out.status.success(),
        "post-tool-use must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // ---- 3. Send a prompt that overflows the ceiling. ----
    // estimate_tokens does bytes/4 on a non-JSON string, so a 2000-char
    // prompt is ~500 tokens — exactly the ceiling. After record(500),
    // used=500; can_send(500) → 500+500=1000 > 500 → false. The guard
    // looks at recent's max size (20_000) which clears needed + 256
    // (500 + 256 = 756), so it must recommend Slice targeting the
    // recent entry's key.
    let prompt = "x".repeat(2_000);
    let ups_payload = serde_json::json!({
        "session_id": "budget-smoke",
        "prompt": prompt,
    });
    let out = write_stdin(
        dirs.cmd(&["hook", "user-prompt-submit"])
            .spawn()
            .expect("spawn user-prompt-submit"),
        serde_json::to_vec(&ups_payload).unwrap().as_slice(),
    );
    assert!(
        out.status.success(),
        "user-prompt-submit must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("user-prompt-submit stdout is JSON");
    // The decision must be Slice (not Allow / Warn / Compact). The
    // recent entry is fat enough to cover needed + overhead; the
    // Compact path only fires when there is no sliceable recent
    // output. Pin the kind + the target_key prefix + the arithmetic
    // on slice_to so a contributor who flips the logic surfaces
    // here. The target_key is the recent-output key, which for a
    // 20 KB body is the slice marker (see step 2 comment); pin the
    // marker prefix rather than the literal hash.
    assert_eq!(
        v["kind"], "slice",
        "user-prompt-submit must return a slice recommendation; got: {v}"
    );
    let target_key = v["target_key"].as_str().expect("target_key is a string");
    assert!(
        target_key.starts_with("<<plugin3:slice:"),
        "slice target must be the recent-output marker (the post-tool-use \
         key appended a slice marker, not tool_result_key, because the \
         20 KB body crossed the orchestrator's slice threshold); \
         got target_key={target_key:?}"
    );
    assert!(
        target_key.ends_with(">>"),
        "slice marker must be terminated; got target_key={target_key:?}"
    );
    // needed = incoming - remaining = 500 - 0 = 500.
    // slice_to = size - needed = 20_000 - 500 = 19_500.
    // Pin the arithmetic so a contributor who swaps to
    // `size.saturating_sub(needed + SLICE_OVERHEAD)` surfaces here.
    assert_eq!(
        v["slice_to"], 19_500,
        "slice_to must equal recent_size - needed (20_000 - 500); got: {v}"
    );

    // ---- 4. Report --last 1 sees the slice record. ----
    // classify_kind() surfaces Slice from a non-Allow intervention;
    // the hook emits a UsageRecord with kind=Slice before returning,
    // so report --last 1 must surface it. Pinning the session_id +
    // kind at the wire level closes the loop on "did the budget
    // guard's intervention land in the usage stream?" — a regression
    // in either the emit logic or the aggregator surfaces here.
    let out = dirs
        .cmd(&["report", "--last", "1", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn report --last");
    assert!(
        out.status.success(),
        "report must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("report --last --json stdout is JSON");
    // ponytail: `report --json` is a top-level array, not an object
    // with a `records` key (see `report_last_n_truncates_to_n_records_at_subprocess`
    // in the in-crate tests for the wire-shape pin). The first test
    // in this file uses `report --summary --json` which DOES wrap in
    // a per-session object — both shapes exist, both are pinned.
    let records = v
        .as_array()
        .expect("report --last --json must be a top-level array");
    assert_eq!(
        records.len(),
        1,
        "report --last 1 must return 1 record; got: {records:?}"
    );
    let r = &records[0];
    assert_eq!(
        r["kind"], "slice",
        "the last record must be a slice event; got: {r}"
    );
    assert_eq!(
        r["session_id"], "budget-smoke",
        "the slice record must belong to the budget-smoke session; got: {r}"
    );
}

/// `PreCompact` hook round-trip — the third (and least-exercised) host
/// event. The earlier two tests pin `PostToolUse` and `UserPromptSubmit`
/// at the subprocess wire level; this test pins `PreCompact` so a
/// contributor who breaks the `hint`/`summary` envelope (renames a
/// key, drops the null-vs-object distinction on the parse-failure
/// path, swaps `LocalSummaryCompactor` for a stub that returns "") is
/// caught here. The `pre_compact_wire_shape_pins_parse_failure_and_empty_history`
/// drift test in `hooks/mod.rs` pins the *source literal*; this
/// integration test pins the *actual binary output*, which is the
/// contract Claude Code reads.
///
/// ponytail: 3 history turns. index, role, `content_preview` are the
/// three required fields (`canonical::Turn`). Empty preview is fine —
/// `LocalSummaryCompactor` runs over the joined string and produces
/// either a non-empty summary or "" on failure; either is a valid
/// response, so we don't pin the summary's content, only that the
/// key is present and the hint structure is well-formed.
#[test]
fn end_to_end_pre_compact_emits_hint_and_summary() {
    let dirs = FreshDirs::new();
    let payload = serde_json::json!({
        "history_turns": [
            { "index": 0, "role": "user",      "content_preview": "hi" },
            { "index": 1, "role": "assistant", "content_preview": "hello back" },
            { "index": 2, "role": "user",      "content_preview": "more text" },
        ],
    });
    let out = write_stdin(
        dirs.cmd(&["hook", "pre-compact"])
            .spawn()
            .expect("spawn pre-compact"),
        serde_json::to_vec(&payload).unwrap().as_slice(),
    );
    assert!(
        out.status.success(),
        "pre-compact must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("pre-compact stdout is JSON");
    // ponytail: pin BOTH keys exist (no rename). A contributor who
    // drops `summary` (or renames it to `compacted`) breaks Claude
    // Code's envelope reader; dropping `hint` (or renaming it to
    // `adv`) breaks the compactor path. The literal-key pin is the
    // strongest contract — type changes (object → null) are caught
    // by the hint type assertion below.
    assert!(
        v.get("hint").is_some(),
        "pre-compact response must carry `hint` key; got: {v}"
    );
    assert!(
        v.get("summary").is_some(),
        "pre-compact response must carry `summary` key; got: {v}"
    );
    // The hint is either a CompactHint object OR null (parse-failure
    // path is unreachable here — we sent a parseable payload). On
    // the happy path, `hint` is an object with the field set the
    // canonical pin (`tokens_used`, `tokens_ceiling`, etc.). We
    // pin `kind` is "ok" only if non-null; a null here means the
    // budget hasn't crossed the threshold, which is also fine. The
    // load-bearing pin: `hint` is either null OR an object — never
    // a string, number, or array.
    let hint = &v["hint"];
    assert!(
        hint.is_null() || hint.is_object(),
        "hint must be either null (under threshold) or a CompactHint object; \
         got type {} with value: {hint}",
        hint_type_name(hint)
    );
    // ponytail: summary is always a string (the LocalSummaryCompactor
    // returns "" on failure). Pin the type so a contributor who
    // serialises it as an object / array breaks here.
    assert!(
        v["summary"].is_string(),
        "summary must be a string (LocalSummaryCompactor output or empty); \
         got type {} with value: {}",
        hint_type_name(&v["summary"]),
        v["summary"]
    );
}

fn hint_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// `plugin3 config --validate` end-to-end — pins the exit-code
/// contract. The earlier in-crate test (`run_path_checks_end_to_end…`)
/// covers the helper wiring; this integration test drives the actual
/// binary so a contributor who breaks the exit-code path
/// (`exit_config_err` import, the magic-number argument, or the
/// `eprintln` formatting) is caught at the user-visible boundary.
/// Per ADR-0015 § Exit codes, config failures map to 78 (`EX_CONFIG`).
///
/// ponytail: hermetic fresh tempdir — every check must pass on a
/// brand-new install. A future contributor who adds a check that
/// fails by default (e.g. requires a sentinel file) breaks this
/// test before the first user runs.
#[test]
fn end_to_end_config_validate_passes_on_fresh_install() {
    let dirs = FreshDirs::new();
    let out = dirs
        .cmd(&["config", "--validate"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn config --validate");
    assert!(
        out.status.success(),
        "config --validate on fresh tempdir must exit 0; \
         exit={:?} stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    // ponytail: pin the human-readable Ok marker. The validate
    // subcommand prints `all N checks passed` (see config.rs:238) —
    // a contributor who drops the trailing summary line, or who
    // rewrites it to a different phrase, breaks any wrapper that
    // greps for `passed`. The exact wording is part of the
    // contract because it surfaces in shell pipes and CI logs.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("passed"),
        "config --validate stdout must report 'passed' on success; got: {stdout:?}"
    );
}

/// `plugin3 config --validate --json` end-to-end — pins the
/// JSON envelope. The unit tests in `commands/config.rs` cover the
/// shape against the helper, but only this subprocess test exercises
/// `serde_json::to_string_pretty` on the wire shape a `jq`-based
/// dashboard would actually parse.
#[test]
fn end_to_end_config_validate_json_envelope_shape() {
    let dirs = FreshDirs::new();
    let out = dirs
        .cmd(&["config", "--validate", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn config --validate --json");
    assert!(
        out.status.success(),
        "config --validate --json must exit 0 on fresh install; \
         exit={:?} stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("config --validate --json stdout is JSON");
    // ponytail: pin the three top-level keys: `ok`, `failures`,
    // `checks`. A contributor who renames `ok` → `success` or
    // `failures` → `errors` breaks the dashboard parser. `checks`
    // is the array of per-path results.
    assert_eq!(
        v["ok"], true,
        "fresh-install validate must report ok=true; got: {v}"
    );
    assert_eq!(
        v["failures"], 0,
        "fresh-install validate must report zero failures; got: {v}"
    );
    let checks = v["checks"].as_array().expect("checks must be a JSON array");
    // ADR-0014/0015 pins exactly 8 paths (3 dirs + 5 derived).
    // The in-crate unit test pins the same number; this subprocess
    // test catches a regression where the count is right in the
    // helper but the JSON serialisation drops an entry (e.g. via
    // a filter that drops the empty-string label).
    assert_eq!(
        checks.len(),
        8,
        "validate --json must report 8 path checks (3 dirs + 5 derived); got {}",
        checks.len()
    );
    // Every entry must have the four pinned fields (label/path/
    // status/detail). Pin the shape so a contributor who renames
    // `detail` → `message` (a tempting rename) breaks here.
    for (i, c) in checks.iter().enumerate() {
        for key in ["label", "path", "status", "detail"] {
            assert!(
                c.get(key).is_some(),
                "check[{i}] must carry `{key}` field; got: {c}"
            );
        }
        assert_eq!(
            c["status"], "ok",
            "fresh-install check[{i}] must be Ok; got: {c}"
        );
    }
}
