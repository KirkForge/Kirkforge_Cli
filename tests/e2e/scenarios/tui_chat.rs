//! Scenario: TUI chat turn via TmuxDriver.
//!
//! WO 19.6 Phase 1: prove the TmuxDriver harness works end-to-end by
//! sending a message and verifying the mock response appears on screen.
//! Marked `#[ignore]` because it requires tmux and a display server.

use std::time::Duration;

use crate::fixtures;
use crate::harness::mock::{MockProvider, Reply};
use crate::harness::ui::{tmux_available, TmuxDriver};
use crate::harness::IsolatedEnv;

/// Send a single prompt to the TUI and verify the response appears
/// in the tmux pane. Exercises the full TUI render loop, input
/// handling, and executor round-trip through the mock provider.
#[tokio::test]
#[ignore = "requires tmux and display server"]
async fn tui_chat_sends_message_and_sees_response() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }

    let mock = MockProvider::start(vec![Reply::text("Hello from mock!")]).await;
    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");
    fixtures::seed_config_auto_approve(&env.data_dir(), &mock.url(), "e2e-test-model");

    let socket = env.data_dir().join("e2e-tui.sock");
    let ui = TmuxDriver::new(socket, "e2e-tui-chat");

    // Start kf-code run (no --no-tui) inside tmux.
    ui.start_session(
        &[env.bin().to_str().unwrap(), "run", "-m", "e2e-test-model"],
        &[
            ("KF_CODE_DATA_DIR", env.data_dir().to_str().unwrap()),
            ("HOME", env.root_path().to_str().unwrap()),
        ],
    )
    .expect("e2e: start tmux session");

    // Wait for the TUI to render a prompt indicator.
    ui.wait_for_contains(">", Duration::from_secs(10), Duration::from_millis(500))
        .expect("e2e: TUI did not render within 10s");

    // The mock sends "Hello from mock!" when it receives any chat message.
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
