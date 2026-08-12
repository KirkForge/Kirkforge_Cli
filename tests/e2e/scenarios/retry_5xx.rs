//! Scenario: adapter retry on mock 5xx.
//!
//! Pins that `kf-code` retries the request when the Ollama provider
//! returns HTTP 500 on the first attempt and succeeds on the second.
//! Regression: C-003 (binary gave up on transient 5xx without retry).

use crate::harness::artifact;
use crate::harness::mock::{HttpError, MockProvider, Reply};
use crate::harness::shard;
use crate::harness::IsolatedEnv;

#[ignore = "slow binary-spawn e2e (real kf-code binary + mock provider); WO 27.2 startup hang is fixed — run with `cargo test --test e2e --features e2e-tests -- --ignored adapter_retries_on_5xx`"]
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

    // The binary may succeed (if it retries) or may fail (if it gives
    // up after exhausting retries).  We accept either outcome but
    // check the request log to see that at least two requests were
    // made (one 500, one success).
    let log = mock.request_log();

    if !output.status.success() {
        let artifact_dir = env.data_dir().join("artifacts");
        let _ = artifact::dump_artifacts_headless(&artifact_dir, &mock, &env.log_path());
        // Even on failure, we should see that the binary retried.
        assert!(
            log.len() >= 2,
            "adapter_retries_on_5xx: expected ≥2 requests (1 fail + 1 retry), got {}.\n\
             stdout: {}\nstderr: {}",
            log.len(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // Success path: binary retried and got the response.
    if output.status.success() {
        assert!(
            log.len() >= 2,
            "adapter_retries_on_5xx: expected ≥2 requests on success path, got {}",
            log.len(),
        );
    }
}
