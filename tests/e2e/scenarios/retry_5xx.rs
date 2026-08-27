//! Scenario: adapter retry on mock 5xx.
//!
//! Pins that `kf-code` retries the request when the Ollama provider
//! returns HTTP 500 on the first attempt and succeeds on the second.
//! Regression: C-003 (binary gave up on transient 5xx without retry).

use crate::harness::artifact;
use crate::harness::mock::{HttpError, MockProvider, Reply};
use crate::harness::shard;
use crate::harness::IsolatedEnv;

#[cfg_attr(not(feature = "e2e-tests"), ignore)]
#[tokio::test]
async fn adapter_retries_on_5xx() {
    if !shard::shard_gate("adapter_retries_on_5xx") {
        return;
    }

    let mock = MockProvider::start(vec![
        Reply::text("retry-success"), // second attempt succeeds
    ])
    .await;

    // Inject a 500 error on the first request.
    mock.inject_error(0, HttpError { status_code: 500 });

    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");

    let output = env
        .run_with_prompt(
            &[
                "run",
                "--no-tui",
                "--non-interactive",
                "--max-turns",
                "1",
                "-m",
                "e2e-test-model",
            ],
            "Test retry",
        )
        .expect("e2e: spawn kf-code run");

    if !output.status.success() {
        let artifact_dir = env.data_dir().join("artifacts");
        let _ = artifact::dump_artifacts_headless(&artifact_dir, &mock, &env.log_path());
        panic!(
            "adapter_retries_on_5xx: kf-code exited {:?} — the mock succeeds on \
             attempt 2, so a non-zero exit means the adapter did not retry.\n\
             stdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The retried attempt's reply must be relayed — proof the run
    // surfaced the post-retry response, not just an error.
    assert!(
        stdout.contains("retry-success") || stderr.contains("retry-success"),
        "adapter_retries_on_5xx: expected 'retry-success' in output.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );

    // Attempt evidence: exactly two requests — the 500 and the retry —
    // both to the Ollama chat endpoint. One means no retry happened;
    // more means unscripted exchanges.
    let log = mock.request_log();
    assert_eq!(
        log.len(),
        2,
        "adapter_retries_on_5xx: expected exactly 2 requests (1 × HTTP 500 + 1 × retry), got {}.\n\
         stdout: {stdout}\nstderr: {stderr}",
        log.len(),
    );
    assert_eq!(log[0].path, "/api/chat");
    assert_eq!(log[1].path, "/api/chat");
}
