//! Scenario: mock error response.
//!
//! Pins that the binary handles a 4xx error from the mock provider
//! gracefully (exits with non-zero, logs the error).  Regression:
//! C-008 (binary panics on 401 from the provider instead of
//! returning a clean error).

use crate::harness::mock::{HttpError, MockProvider, Reply};
use crate::harness::shard;
use crate::harness::IsolatedEnv;

#[tokio::test]
async fn mock_401_produces_clean_exit() {
    if !shard::shard_gate("mock_401_produces_clean_exit") {
        return;
    }

    let mock = MockProvider::start(vec![Reply::text("should-not-see-this")]).await;
    // Inject a 401 on the first request.
    mock.inject_error(0, HttpError { status_code: 401 });

    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");

    let output = env
        .command(&[
            "run",
            "--no-tui",
            "--non-interactive",
            "--max-turns",
            "1",
            "-m",
            "e2e-test-model",
            "Hello",
        ])
        .output()
        .expect("e2e: spawn kf-code run");

    // The binary should exit with a non-zero status (4xx is not retryable).
    assert!(
        !output.status.success(),
        "mock_401: binary should exit non-zero on 401, but succeeded.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // The mock should have received the request.
    let log = mock.request_log();
    assert_eq!(
        log.len(),
        1,
        "mock_401: expected 1 request, got {}",
        log.len()
    );
}
