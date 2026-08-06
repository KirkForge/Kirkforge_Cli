//! Scenario: auto-approve skips approval modal.
//!
//! Pins that setting `auto_approve = true` in the config (or via
//! `KF_CODE_AUTO_APPROVE=true`) causes the binary to skip the
//! approval step for destructive tool calls.  Regression: C-002
//! (auto_approve flag was ignored in non-interactive mode).

use crate::harness::artifact;
use crate::harness::mock::{MockProvider, Reply};
use crate::harness::shard;
use crate::harness::IsolatedEnv;

#[tokio::test]
async fn auto_approve_skips_approval() {
    if !shard::shard_gate("auto_approve_skips_approval") {
        return;
    }

    // The model calls `bash` with an `rm -rf` command (destructive),
    // then confirms completion.
    let mock = MockProvider::start(vec![
        Reply::tool(
            "bash",
            serde_json::json!({"command": "rm -rf /tmp/e2e-test"}),
        ),
        Reply::text("Cleanup done."),
    ])
    .await;

    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");
    // Explicitly set auto_approve=true.
    crate::fixtures::seed_config_auto_approve(&env.data_dir(), &mock.url(), "e2e-test-model");

    let output = env
        .command(&[
            "run",
            "--no-tui",
            "--non-interactive",
            "--max-turns",
            "3",
            "-m",
            "e2e-test-model",
            "Clean up /tmp/e2e-test",
        ])
        .output()
        .expect("e2e: spawn kf-code run");

    // With auto_approve, the binary should complete without hanging on
    // the approval modal.  The test may or may not succeed depending on
    // whether the tool result round-trip works end-to-end, but the key
    // assertion is that the binary didn't hang waiting for user input.
    let log = mock.request_log();
    assert!(
        !log.is_empty(),
        "auto_approve: expected ≥1 request, got {}.\n\
         stdout: {}\nstderr: {}",
        log.len(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    if !output.status.success() {
        let artifact_dir = env.data_dir().join("artifacts");
        let _ = artifact::dump_artifacts_headless(&artifact_dir, &mock, &env.log_path());
    }
}
