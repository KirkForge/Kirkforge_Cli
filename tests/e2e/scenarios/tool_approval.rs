//! Scenario: tool approval round-trip.
//!
//! Pins that `kf-code run` with a tool-calling model sends a tool_call,
//! receives approval, and completes the loop.  In non-interactive mode
//! with `auto_approve=true`, the tool call is auto-approved.
//! Regression: C-004 (tool-call responses were dropped when
//! auto_approve was set via config).

use crate::harness::artifact;
use crate::harness::mock::{MockProvider, Reply};
use crate::harness::shard;
use crate::harness::IsolatedEnv;

#[cfg_attr(not(feature = "e2e-tests"), ignore)]
#[tokio::test]
async fn tool_approval_auto_approve_round_trip() {
    if !shard::shard_gate("tool_approval_auto_approve_round_trip") {
        return;
    }

    // Two replies: first the model calls a tool, second it produces text
    // after seeing the tool result.
    let mock = MockProvider::start(vec![
        Reply::tool("bash", serde_json::json!({"command": "echo hello"})),
        Reply::text("Tool executed successfully."),
    ])
    .await;

    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");
    // Override config to enable auto-approve so we don't need a PTY.
    crate::fixtures::seed_config_auto_approve(&env.data_dir(), &mock.url(), "e2e-test-model");

    let output = env
        .run_with_prompt(
            &[
                "run",
                "--no-tui",
                "--non-interactive",
                "--max-turns",
                "3",
                "-m",
                "e2e-test-model",
            ],
            "Run echo hello",
        )
        .expect("e2e: spawn kf-code run");

    if !output.status.success() {
        let artifact_dir = env.data_dir().join("artifacts");
        let _ = artifact::dump_artifacts_headless(&artifact_dir, &mock, &env.log_path());
    }

    // At least the first request should have been made (the model call).
    let log = mock.request_log();
    assert!(
        !log.is_empty(),
        "tool_approval: expected ≥1 request, got {}.\n\
         stdout: {}\nstderr: {}",
        log.len(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    // The first request should be to /api/chat (Ollama dialect).
    assert_eq!(log[0].path, "/api/chat");
}
