//! Deterministic smoke tests that run without Ollama.
//!
//! These exercise the CLI surface and public APIs to catch regressions in
//! the agent loop, permission gating, and operational metrics without
//! relying on a live model server.

use std::process::Command;

/// Return the path to the built `kf-code` binary.
fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_kf-code").into()
}

/// Build a `Command` for `kf-code` with an isolated `KF_CODE_DATA_DIR`
/// pointing at a fresh temp dir, so first-run behaviour is deterministic
/// and the test never touches the operator's real data dir. Returns the
/// temp dir (kept alive for the command's lifetime) and the command.
fn isolated_command() -> (tempfile::TempDir, Command) {
    let dir = tempfile::tempdir().expect("smoke: create temp data dir");
    let mut cmd = Command::new(bin());
    cmd.env("KF_CODE_DATA_DIR", dir.path());
    (dir, cmd)
}

#[test]
fn metrics_command_prints_summary() {
    let output = Command::new(bin())
        .arg("metrics")
        .output()
        .expect("failed to run kf-code metrics");

    assert!(
        output.status.success(),
        "kf-code metrics failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Metrics summary"), "summary header missing");
    assert!(stdout.contains("turns:"), "turns line missing");
    assert!(stdout.contains("tool calls:"), "tool-calls line missing");
    assert!(stdout.contains("verifiers:"), "verifiers line missing");
    assert!(stdout.contains("approvals:"), "approvals line missing");
}

#[test]
fn completions_command_outputs_script() {
    let output = Command::new(bin())
        .args(["completions", "bash"])
        .output()
        .expect("failed to run kf-code completions bash");

    assert!(
        output.status.success(),
        "kf-code completions bash failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("kf-code"),
        "completion script should mention kf-code"
    );
}

/// `kf-code <subcommand> --help` must exit 0 and mention the subcommand
/// name. Covers the subcommands not exercised by the tests above
/// (metrics + completions). `--help` short-circuits before any required
/// positional args or side effects, so it is safe for daemon/replay/etc.
#[test]
fn help_flag_for_remaining_subcommands() {
    let subcommands = [
        "run", "verify", "sessions", "daemon", "jobd", "replay", "bench", "plugin", "doctor",
    ];
    for sub in subcommands {
        let output = Command::new(bin())
            .args([sub, "--help"])
            .output()
            .unwrap_or_else(|e| panic!("failed to run kf-code {sub} --help: {e}"));
        assert!(
            output.status.success(),
            "kf-code {sub} --help failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(sub),
            "kf-code {sub} --help output should mention '{sub}':\n{stdout}"
        );
    }
}

/// WO 38.10 P0: a first run with no config and `--output stream-json`
/// must keep stdout byte-clean. The first-run banner now goes to stderr,
/// and an empty default_model exits 3 (ModelUnreachable) before any
/// model call — so stdout must contain no non-JSON lines. With the
/// empty-model guard, the binary bails before the line loop emits any
/// stream-json, so stdout should be empty.
#[test]
fn first_run_stream_json_stdout_is_clean() {
    let (_dir, mut cmd) = isolated_command();
    cmd.args([
        "run",
        "--non-interactive",
        "--output",
        "stream-json",
        "--max-turns",
        "1",
    ])
    .stdin(std::process::Stdio::null());
    let output = cmd.output().expect("smoke: spawn kf-code run");

    // Empty model → exit 3 (ModelUnreachable). Non-zero is expected.
    let code = output.status.code().unwrap_or(-1);
    assert_ne!(
        code,
        0,
        "expected non-zero exit for empty model, got {code}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Every non-empty stdout line must parse as JSON. With the empty-model
    // guard the line loop never runs, so stdout should be empty — but the
    // assertion is stronger: if any line is present, it must be valid JSON
    // (no banner pollution).
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "first-run stream-json stdout has a non-JSON line (banner pollution?): {line:?}\n\
             full stdout: {stdout}"
        );
    }

    // The banner must go to stderr, not stdout.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no model configured") || stderr.contains("model unreachable"),
        "expected empty-model guidance on stderr, got: {stderr}"
    );
}

/// WO 38.10 P0: an empty default_model must exit 3 (ModelUnreachable
/// class) and print actionable guidance, instead of the cryptic 400 from
/// the OpenAI-compat fallback. Fresh data dir → no config → empty model.
#[test]
fn empty_model_exits_3_with_hint() {
    let (_dir, mut cmd) = isolated_command();
    cmd.args(["run", "--non-interactive", "--max-turns", "1"])
        .stdin(std::process::Stdio::null());
    let output = cmd.output().expect("smoke: spawn kf-code run");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        3,
        "empty model must exit 3 (ModelUnreachable), got {code}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no model configured"),
        "empty-model stderr must mention 'no model configured', got: {stderr}"
    );
    assert!(
        stderr.contains("kf-code run -m"),
        "empty-model stderr must suggest `kf-code run -m`, got: {stderr}"
    );
}

/// WO 38.10 P1: `run -p "<prompt>"` parses and, with no model, still
/// hits the empty-model guard (so `-p` doesn't bypass the model check).
/// The round-trip to a live model is covered by the e2e suite; this
/// smoke test pins that `-p` is accepted and routed through the same
/// session setup.
#[test]
fn run_with_prompt_flag_parses_and_routes() {
    let (_dir, mut cmd) = isolated_command();
    cmd.args([
        "run",
        "-p",
        "hello world",
        "--non-interactive",
        "--max-turns",
        "1",
    ])
    .stdin(std::process::Stdio::null());
    let output = cmd.output().expect("smoke: spawn kf-code run -p");

    // No model → exit 3 (the prompt is accepted; the guard fires after).
    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        3,
        "run -p with no model must exit 3, got {code}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // clap must NOT reject -p (it would exit 2).
    assert_ne!(code, 2, "-p must be a valid flag (exit 2 = bad args)");
}
