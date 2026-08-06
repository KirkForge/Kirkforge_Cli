//! Scenario: TUI tool-approval round-trip.
//!
//! WO 19.6 Phase 2: prove the approval dialog renders, accepts 'y' to
//! approve, and the tool result flows back to the model.
//!
//! **Note:** like `tui_chat_full_tui_round_trip`, this test requires a
//! display server and interactive terminal and is `#[ignore]` by default.
//! It cannot run reliably inside `cargo test` because crossterm does not
//! initialise the alternate screen buffer when the parent is the test
//! runner.

use std::time::Duration;

use crate::harness::confirm;
use crate::harness::mock::{MockProvider, Reply};
use crate::harness::ui::{tmux_available, TmuxDriver};
use crate::harness::IsolatedEnv;

/// Send a prompt that triggers a tool call, approve it in the TUI,
/// and verify the model's second response appears. Exercises the full
/// approval dialog → keypress → executor → second-turn loop.
#[tokio::test]
#[ignore = "requires tmux, display server, and interactive terminal"]
async fn tui_tool_approval_approve_flow() {
    if !tmux_available() {
        eprintln!("skipping: tmux not available");
        return;
    }

    // Two replies: first the model calls a tool, second it produces text
    // after seeing the tool result.
    let mock = MockProvider::start(vec![
        Reply::tool("bash", serde_json::json!({"command": "echo hello"})),
        Reply::text("Tool executed successfully."),
    ])
    .await;

    // auto_approve = false (IsolatedEnv default) so the TUI shows the dialog.
    let env = IsolatedEnv::new(&mock.url(), "e2e-test-model");

    let socket = env.data_dir().join("e2e-approval.sock");
    let ui = TmuxDriver::new(socket, "e2e-tui-approval");

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

    // Type a prompt that triggers the tool call.
    ui.send_keys("Run echo hello").expect("e2e: send_keys");
    ui.send_enter().expect("e2e: send_enter");

    // The model responds with a tool call; the TUI shows the approval dialog.
    confirm::approve(&ui, Duration::from_secs(30))
        .expect("e2e: approval dialog did not appear within 30s");

    // After approval, the tool runs and the model's second response appears.
    ui.wait_for_contains(
        "Tool executed successfully",
        Duration::from_secs(30),
        Duration::from_millis(500),
    )
    .expect("e2e: second model response did not appear within 30s");

    // Quit cleanly.
    ui.send_keys("/quit").expect("e2e: send /quit");
    ui.send_enter().expect("e2e: send_enter");
    ui.wait_stable(Duration::from_secs(2), Duration::from_millis(500))
        .expect("e2e: TUI did not exit within 2s");
}
