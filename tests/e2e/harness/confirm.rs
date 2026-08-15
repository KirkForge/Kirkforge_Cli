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
#[allow(dead_code)]
const APPROVAL_YES: &str = "[Y]es";

/// Detect an approval modal in the tmux pane and send 'y' to approve.
/// Returns the pane content after the approval key was sent.
pub fn approve(ui: &TmuxDriver, timeout: std::time::Duration) -> std::io::Result<String> {
    let pane = ui.wait_for_contains(
        APPROVAL_TITLE,
        timeout,
        std::time::Duration::from_millis(50),
    )?;
    ui.send_keys("y")?;
    // Wait for the modal to clear instead of a blind sleep: poll until
    // the approval title is no longer in the pane.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(p) = ui.capture_pane() {
            if !p.contains(APPROVAL_TITLE) {
                return Ok(pane);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Ok(pane);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Detect an approval modal and send 'n' to reject.
#[allow(dead_code)]
pub fn reject(ui: &TmuxDriver, timeout: std::time::Duration) -> std::io::Result<String> {
    let pane = ui.wait_for_contains(
        APPROVAL_TITLE,
        timeout,
        std::time::Duration::from_millis(50),
    )?;
    ui.send_keys("n")?;
    // Wait for the modal to clear instead of a blind sleep.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(p) = ui.capture_pane() {
            if !p.contains(APPROVAL_TITLE) {
                return Ok(pane);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Ok(pane);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Check whether the tmux pane currently shows the approval modal.
#[allow(dead_code)]
pub fn is_approval_visible(ui: &TmuxDriver) -> bool {
    ui.capture_pane()
        .map(|p| p.contains(APPROVAL_TITLE) || p.contains(APPROVAL_YES))
        .unwrap_or(false)
}
