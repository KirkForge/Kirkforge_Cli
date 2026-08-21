//! Scenario: `run -p "<prompt>"` one-shot (WO 38.10).
//!
//! Pins that the `--prompt`/`-p` flag delivers the arg as the first turn
//! to the model without piping, and that a multi-paragraph `-p` value
//! arrives whole (the heredoc-terminator that truncates piped stdin on a
//! blank line must not split a `-p` arg).

use crate::harness::artifact;
use crate::harness::mock::{MockProvider, Reply};
use crate::harness::shard;
use crate::harness::IsolatedEnv;

/// `run -p "Say hello"` reaches the model with the prompt intact and
/// exits cleanly. The mock echoes the prompt it received.
#[cfg_attr(not(feature = "e2e-tests"), ignore)]
#[tokio::test]
async fn run_prompt_flag_round_trips() {
    if !shard::shard_gate("run_prompt_flag_round_trips") {
        return;
    }

    let mock = MockProvider::start(vec![Reply::text("ack")]).await;
    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");

    let mut cmd = env.command(&[
        "run",
        "-p",
        "Say hello",
        "--no-tui",
        "--non-interactive",
        "--max-turns",
        "1",
        "-m",
        "e2e-test-model",
    ]);
    cmd.stdin(std::process::Stdio::null());
    let mut child = cmd.spawn().expect("e2e: spawn kf-code run -p");
    let output = crate::harness::wait_with_timeout(&mut child).expect("e2e: wait for kf-code");

    if !output.status.success() {
        let artifact_dir = env.data_dir().join("artifacts");
        let _ = artifact::dump_artifacts_headless(&artifact_dir, &mock, &env.log_path());
        panic!(
            "run_prompt_flag_round_trips: kf-code exited {:?}\n\
             stdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // The mock should have received exactly one request carrying the
    // `-p` prompt as the user message.
    let log = mock.request_log();
    assert_eq!(
        log.len(),
        1,
        "run_prompt_flag_round_trips: expected 1 request, got {}",
        log.len()
    );
    let body = &log[0].body;
    assert!(
        body.to_string().contains("Say hello"),
        "run_prompt_flag_round_trips: model request body must contain the -p prompt, got: {body}"
    );
}

/// A multi-paragraph `-p` value (with an internal blank line) must arrive
/// as a single turn — the blank-line heredoc terminator only applies to
/// piped stdin, not the arg form (WO 38.10).
#[cfg_attr(not(feature = "e2e-tests"), ignore)]
#[tokio::test]
async fn run_prompt_flag_multi_paragraph_is_one_turn() {
    if !shard::shard_gate("run_prompt_flag_multi_paragraph_is_one_turn") {
        return;
    }

    let mock = MockProvider::start(vec![Reply::text("ack")]).await;
    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");

    let mut cmd = env.command(&[
        "run",
        "-p",
        "first paragraph\n\nsecond paragraph",
        "--no-tui",
        "--non-interactive",
        "--max-turns",
        "1",
        "-m",
        "e2e-test-model",
    ]);
    cmd.stdin(std::process::Stdio::null());
    let mut child = cmd.spawn().expect("e2e: spawn kf-code run -p");
    let output = crate::harness::wait_with_timeout(&mut child).expect("e2e: wait for kf-code");

    if !output.status.success() {
        let artifact_dir = env.data_dir().join("artifacts");
        let _ = artifact::dump_artifacts_headless(&artifact_dir, &mock, &env.log_path());
        panic!(
            "run_prompt_flag_multi_paragraph: kf-code exited {:?}\n\
             stdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // Both paragraphs must be present in the single model request.
    let log = mock.request_log();
    assert_eq!(
        log.len(),
        1,
        "run_prompt_flag_multi_paragraph: expected exactly 1 request, got {}",
        log.len()
    );
    let body = &log[0].body;
    assert!(
        body.to_string().contains("first paragraph")
            && body.to_string().contains("second paragraph"),
        "run_prompt_flag_multi_paragraph: both paragraphs must be in the request body, got: {body}"
    );
}
