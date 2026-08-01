//! Deterministic smoke tests that run without Ollama.
//!
//! These exercise the CLI surface and public APIs to catch regressions in
//! the agent loop, permission gating, and operational metrics without
//! relying on a live model server.

use std::process::Command;

/// Return the path to the built `kirkforge` binary.
fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_kirkforge").into()
}

#[test]
fn metrics_command_prints_summary() {
    let output = Command::new(bin())
        .arg("metrics")
        .output()
        .expect("failed to run kirkforge metrics");

    assert!(
        output.status.success(),
        "kirkforge metrics failed: {}",
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
        .expect("failed to run kirkforge completions bash");

    assert!(
        output.status.success(),
        "kirkforge completions bash failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("kirkforge"),
        "completion script should mention kirkforge"
    );
}

/// `kirkforge <subcommand> --help` must exit 0 and mention the subcommand
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
            .unwrap_or_else(|e| panic!("failed to run kirkforge {sub} --help: {e}"));
        assert!(
            output.status.success(),
            "kirkforge {sub} --help failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(sub),
            "kirkforge {sub} --help output should mention '{sub}':\n{stdout}"
        );
    }
}
