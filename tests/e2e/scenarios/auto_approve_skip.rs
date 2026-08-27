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

#[cfg_attr(not(feature = "e2e-tests"), ignore)]
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
            "Clean up /tmp/e2e-test",
        )
        .expect("e2e: spawn kf-code run");

    if !output.status.success() {
        let artifact_dir = env.data_dir().join("artifacts");
        let _ = artifact::dump_artifacts_headless(&artifact_dir, &mock, &env.log_path());
        panic!(
            "auto_approve: kf-code exited {:?}\n\
             stdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The final scripted reply must be relayed — proof the turn completed
    // after the destructive tool call instead of wedging on approval.
    assert!(
        stdout.contains("Cleanup done.") || stderr.contains("Cleanup done."),
        "auto_approve: expected 'Cleanup done.' in output.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );

    // Exactly two model requests: the tool-call turn + the follow-up
    // carrying the tool result. Fewer means the loop stalled; more means
    // an unscripted exchange.
    let log = mock.request_log();
    assert_eq!(
        log.len(),
        2,
        "auto_approve: expected 2 requests (tool call + follow-up), got {}.\n\
         stdout: {stdout}\nstderr: {stderr}",
        log.len(),
    );

    // The follow-up must carry the tool result, and that result must NOT
    // be the denial reason — `auto_approve = true` means the approval
    // prompt was skipped (approved), not denied by the non-interactive
    // handler ("❌ Approval denied: non-interactive mode cannot approve
    // destructive tools; …" — pre_run.rs Skip path).
    let follow_up = log
        .last()
        .expect("auto_approve: follow-up request recorded");
    let tool_msg = follow_up.body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["role"] == "tool")
        .unwrap_or_else(|| {
            panic!(
                "auto_approve: expected a tool-role message in the follow-up request, got {}",
                follow_up.body
            )
        });
    let content = tool_msg["content"].as_str().unwrap_or("");
    assert!(
        !content.contains("Approval denied"),
        "auto_approve: tool result is an approval denial (content: {content:?}) — \
         the approve flow was not skipped"
    );
}
