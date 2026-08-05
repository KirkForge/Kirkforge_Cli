//! On-failure artifact dump: writes the PTY screen, mock request log,
//! and the kf-code log to a per-test artifact directory.

use std::fs;
use std::path::Path;

use super::mock::MockProvider;
use super::ui::TmuxDriver;

/// Dump e2e test artifacts to `artifact_dir`.  Writes:
/// - `screen.txt`  — tmux capture-pane output
/// - `requests.json`  — mock request log (serialized as JSON)
/// - `kf-code.log` — the binary's log file (if it exists)
pub fn dump_artifacts(
    artifact_dir: &Path,
    ui: &TmuxDriver,
    mock: &MockProvider,
    log_path: &Path,
) -> std::io::Result<()> {
    fs::create_dir_all(artifact_dir)?;

    // Screen capture
    if let Ok(pane) = ui.capture_pane() {
        fs::write(artifact_dir.join("screen.txt"), pane)?;
    }

    // Mock request log
    let requests = mock.request_log();
    let requests_json = serde_json::to_string_pretty(&requests)?;
    fs::write(artifact_dir.join("requests.json"), requests_json)?;

    // Binary log
    if log_path.exists() {
        let _ = fs::copy(log_path, artifact_dir.join("kf-code.log"));
    }

    Ok(())
}

/// Dump artifacts for a non-TUI test (no screen capture, just mock
/// log and binary log).
pub fn dump_artifacts_headless(
    artifact_dir: &Path,
    mock: &MockProvider,
    log_path: &Path,
) -> std::io::Result<()> {
    fs::create_dir_all(artifact_dir)?;

    let requests = mock.request_log();
    let requests_json = serde_json::to_string_pretty(&requests)?;
    fs::write(artifact_dir.join("requests.json"), requests_json)?;

    if log_path.exists() {
        let _ = fs::copy(log_path, artifact_dir.join("kf-code.log"));
    }

    Ok(())
}
