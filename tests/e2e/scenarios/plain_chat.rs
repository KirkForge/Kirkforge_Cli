//! Scenario: plain chat turn.
//!
//! Pins that `kf-code run --no-tui` can complete a single chat turn
//! against the mock Ollama provider and exit cleanly.  This is the
//! most basic e2e smoke test — it exercises the adapter wire format,
//! the executor loop, and the session file write, all in-process through
//! the real binary.

use crate::harness::artifact;
use crate::harness::mock::{MockProvider, Reply};
use crate::harness::shard;
use crate::harness::IsolatedEnv;

/// Send a single prompt to the mock and verify the response appears in
/// stdout.  Uses `--no-tui --non-interactive` mode so we don't need a
/// PTY.  Regression: C-001 (binary crashes when Ollama returns a
/// single text chunk and closes).
#[tokio::test]
async fn plain_chat_turn_completes() {
    if !shard::shard_gate("plain_chat_turn_completes") {
        return;
    }

    let mock = MockProvider::start(vec![Reply::text("Hello from mock!")]).await;
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
            "Say hello",
        )
        .expect("e2e: spawn kf-code run");

    if !output.status.success() {
        let artifact_dir = env.data_dir().join("artifacts");
        let _ = artifact::dump_artifacts_headless(&artifact_dir, &mock, &env.log_path());
        panic!(
            "plain_chat_turn: kf-code exited {:?}\n\
             stdout: {}\n\
             stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The mock sends "Hello from mock!" — the binary should relay it
    // (in non-interactive mode it writes to stdout).
    assert!(
        stdout.contains("Hello from mock!") || stderr.contains("Hello from mock!"),
        "plain_chat_turn: expected 'Hello from mock!' in output.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );

    // The mock should have received exactly one request.
    let log = mock.request_log();
    assert_eq!(
        log.len(),
        1,
        "plain_chat_turn: expected 1 request, got {}",
        log.len()
    );
    assert_eq!(log[0].path, "/api/chat");
}
