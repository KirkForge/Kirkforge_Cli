//! Approval modal driver for e2e tests.
//!
//! Detects the TUI approval panel by looking for the rendered strings
//! from `src/tui/components/approval.rs` and sends the right key to
//! accept or reject.

use super::ui::TmuxDriver;

/// Strings rendered in the approval dialog that we can search for
/// in the tmux pane.  These come from `render_approval_dialog` in
/// `src/tui/components/approval.rs`.
const APPROVAL_TITLE: &str = "Approval Required";
const APPROVAL_YES: &str = "[Y]es";

/// Detect an approval modal in the tmux pane and send 'y' to approve.
/// Returns the pane content after the approval key was sent.
pub fn approve(ui: &TmuxDriver, timeout: std::time::Duration) -> std::io::Result<String> {
    let pane = ui.wait_for_contains(
        APPROVAL_TITLE,
        timeout,
        std::time::Duration::from_millis(200),
    )?;
    ui.send_keys("y")?;
    // Wait for the approval to be processed and the modal to clear.
    std::thread::sleep(std::time::Duration::from_millis(500));
    Ok(pane)
}

/// Detect an approval modal and send 'n' to reject.
pub fn reject(ui: &TmuxDriver, timeout: std::time::Duration) -> std::io::Result<String> {
    let pane = ui.wait_for_contains(
        APPROVAL_TITLE,
        timeout,
        std::time::Duration::from_millis(200),
    )?;
    ui.send_keys("n")?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    Ok(pane)
}

/// Check whether the tmux pane currently shows the approval modal.
#[allow(dead_code)] // wired when a visibility-assert scenario lands
pub fn is_approval_visible(ui: &TmuxDriver) -> bool {
    ui.capture_pane()
        .map(|p| p.contains(APPROVAL_TITLE) || p.contains(APPROVAL_YES))
        .unwrap_or(false)
}
