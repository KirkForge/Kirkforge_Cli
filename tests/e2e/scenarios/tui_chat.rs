//! Scenario: TUI chat turn via TmuxDriver.
//!
//! WO 19.6 Phase 1: prove the TmuxDriver harness works end-to-end by
//! verifying kf-code can run inside tmux and produce output.
//!
//! The full-TUI test (`tui_chat_full_tui_round_trip`) requires a display
//! server and an interactive terminal; it is `#[ignore]` by default because
//! crossterm's alternate-screen-buffer initialisation does not activate
//! when the parent process is the cargo-test runner.  The headless variant
//! uses `--no-tui --non-interactive` to verify the harness wiring.

use std::io::Write;
use std::time::Duration;

use crate::fixtures;
use crate::harness::mock::{MockProvider, Reply};
use crate::harness::ui::{tmux_available, TmuxDriver};
use crate::harness::IsolatedEnv;

/// Headless round-trip: pipe a prompt into `kf-code run --no-tui
/// --non-interactive` and verify the mock response appears in stdout.
/// This proves the mock, config, and adapter wiring all work.
#[tokio::test]
async fn tui_chat_headless_round_trip() {
    let mock = MockProvider::start(vec![Reply::text("Hello from mock!")]).await;
    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");
    fixtures::seed_config_auto_approve(&env.data_dir(), &mock.url(), "e2e-test-model");

    let mut cmd = env.command(&[
        "run",
        "--no-tui",
        "--non-interactive",
        "--max-turns",
        "1",
        "-m",
        "e2e-test-model",
    ]);
    // Pipe the prompt via stdin.
    cmd.stdin(std::process::Stdio::piped());

    let mut child = cmd.spawn().expect("e2e: spawn kf-code run");
    {
        let stdin = child.stdin.as_mut().expect("e2e: stdin");
        writeln!(stdin, "Say hello").expect("e2e: write prompt to stdin");
    }
    // Close stdin to signal EOF.
    drop(child.stdin.take());

    let output = crate::harness::wait_with_timeout(&mut child).expect("e2e: wait for kf-code");

    if !output.status.success() {
        panic!(
            "headless_chat: kf-code exited {:?}\n\
             stdout: {}\n\
             stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("Hello from mock!") || stderr.contains("Hello from mock!"),
        "headless_chat: expected 'Hello from mock!' in output.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );

    let log = mock.request_log();
    assert!(
        !log.is_empty(),
        "headless_chat: expected at least 1 request, got {}",
        log.len()
    );
}

/// Full TUI round-trip: start kf-code in tmux (with TUI), send a
/// message, and verify the response appears on screen.
///
/// **Note:** this test requires a display server and cannot run reliably
/// inside `cargo test` because crossterm's alternate-screen-buffer
/// initialisation does not activate when the parent process is the test
/// runner.  Run it manually from an interactive terminal:
///
/// ```sh
/// tmux new-session -d -s e2e \
///   'cargo test --test e2e -- --include-ignored tui_chat_full_tui'
/// ```
#[tokio::test]
#[ignore = "requires tmux, display server, and interactive terminal"]
async fn tui_chat_full_tui_round_trip() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let mock = MockProvider::start(vec![Reply::text("Hello from mock!")]).await;
    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");
    fixtures::seed_config_auto_approve(&env.data_dir(), &mock.url(), "e2e-test-model");

    let socket = env.data_dir().join("e2e-tui.sock");
    let ui = TmuxDriver::new(socket, "e2e-tui-chat");

    // Start kf-code run (TUI mode) inside tmux.
    ui.start_session(
        &[env.bin().to_str().unwrap(), "run", "-m", "e2e-test-model"],
        &[
            ("KF_CODE_DATA_DIR", env.data_dir().to_str().unwrap()),
            ("HOME", env.root_path().to_str().unwrap()),
            ("TERM", "xterm-256color"),
        ],
    )
    .expect("e2e: start tmux session");

    // Wait for the TUI to render the input box.
    ui.wait_for_contains(
        "Type a message",
        Duration::from_secs(15),
        Duration::from_millis(500),
    )
    .expect("e2e: TUI did not render within 15s");

    // Type the message and press Enter.
    ui.send_keys("Hello").expect("e2e: send_keys");
    ui.send_enter().expect("e2e: send_enter");

    // Wait for the mock's response to appear in the pane.
    ui.wait_for_contains(
        "Hello from mock!",
        Duration::from_secs(30),
        Duration::from_millis(500),
    )
    .expect("e2e: mock response did not appear within 30s");

    // Quit the session cleanly via /quit.
    ui.send_keys("/quit").expect("e2e: send /quit");
    ui.send_enter().expect("e2e: send_enter");
    ui.wait_stable(Duration::from_secs(2), Duration::from_millis(500))
        .expect("e2e: TUI did not exit within 2s");
}
