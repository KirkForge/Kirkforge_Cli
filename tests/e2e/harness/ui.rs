//! PTY/TMUX driver for e2e tests.
//!
//! Launches `kf-code run` inside a tmux server so we can send keystrokes
//! and capture screen output.  Uses `tmux -S <sock>` for socket isolation
//! (each test gets its own tmux server so tests don't collide).

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// A tmux-based PTY driver for interacting with the `kf-code` TUI.
pub struct TmuxDriver {
    /// Path to the tmux socket file (unique per test).
    socket_path: PathBuf,
    /// Name of the tmux session.
    session_name: String,
}

impl TmuxDriver {
    /// Create a new tmux driver.  Does NOT start tmux yet.
    pub fn new(socket_path: PathBuf, session_name: &str) -> Self {
        Self {
            socket_path,
            session_name: session_name.to_string(),
        }
    }

    /// Start a tmux session running the given command.
    pub fn start_session(
        &self,
        command: &[&str],
        env_vars: &[(&str, &str)],
    ) -> std::io::Result<()> {
        let mut cmd = Command::new("tmux");
        cmd.arg("-S").arg(&self.socket_path);
        cmd.args(["new-session", "-d", "-s", &self.session_name]);
        // Set a reasonable terminal size for detached sessions that may
        // not inherit a controlling terminal (e.g. from cargo test).
        cmd.args(["-x", "120", "-y", "40"]);

        // Set env vars before the command
        for (key, val) in env_vars {
            cmd.arg("-e");
            cmd.arg(format!("{}={}", key, val));
        }

        // The command to run inside tmux
        cmd.arg("--");
        cmd.args(command);

        let status = cmd.status()?;
        if !status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("tmux new-session failed: {:?}", status),
            ));
        }

        // Give the TUI a moment to render.
        std::thread::sleep(Duration::from_millis(500));
        Ok(())
    }

    /// Send keystrokes to the tmux session.
    pub fn send_keys(&self, keys: &str) -> std::io::Result<()> {
        let status = Command::new("tmux")
            .args([
                "-S",
                &self.socket_path.to_string_lossy(),
                "send-keys",
                "-t",
                &self.session_name,
                keys,
            ])
            .status()?;
        if !status.success() {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("tmux send-keys failed: {:?}", status),
            ))
        } else {
            Ok(())
        }
    }

    /// Send a literal Enter key.
    pub fn send_enter(&self) -> std::io::Result<()> {
        self.send_keys("Enter")
    }

    /// Capture the visible pane content as a string.
    /// Tries the alternate screen first (the TUI renders on the alternate
    /// screen buffer via `EnterAlternateScreen`), falls back to the
    /// primary screen if no alternate screen exists yet.
    pub fn capture_pane(&self) -> std::io::Result<String> {
        // Try alternate screen first (TUI content is here).
        let alt = Command::new("tmux")
            .args([
                "-S",
                &self.socket_path.to_string_lossy(),
                "capture-pane",
                "-a", // alternate screen
                "-p",
                "-t",
                &self.session_name,
            ])
            .output()?;
        if alt.status.success() {
            return Ok(String::from_utf8_lossy(&alt.stdout).to_string());
        }
        // Fall back to primary screen (before TUI switches to alternate).
        let output = Command::new("tmux")
            .args([
                "-S",
                &self.socket_path.to_string_lossy(),
                "capture-pane",
                "-p",
                "-t",
                &self.session_name,
            ])
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "tmux capture-pane failed: {:?}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Wait until the pane content contains the expected substring,
    /// polling every `interval` up to `timeout`.
    pub fn wait_for_contains(
        &self,
        expected: &str,
        timeout: Duration,
        interval: Duration,
    ) -> std::io::Result<String> {
        let start = std::time::Instant::now();
        loop {
            let pane = self.capture_pane()?;
            if pane.contains(expected) {
                return Ok(pane);
            }
            if start.elapsed() > timeout {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "timed out waiting for {:?} in tmux pane. Last pane content:\n{}",
                        expected, pane
                    ),
                ));
            }
            std::thread::sleep(interval);
        }
    }

    /// Wait until the pane content stabilizes (no change for `stable`
    /// duration), polling every `interval`.
    pub fn wait_stable(&self, stable: Duration, interval: Duration) -> std::io::Result<String> {
        let mut last = self.capture_pane()?;
        let mut stable_since = std::time::Instant::now();
        loop {
            std::thread::sleep(interval);
            let current = self.capture_pane()?;
            if current == last {
                if stable_since.elapsed() >= stable {
                    return Ok(current);
                }
            } else {
                last = current;
                stable_since = std::time::Instant::now();
            }
        }
    }

    /// Kill the tmux session.  Called automatically on drop.
    pub fn kill_session(&self) -> std::io::Result<()> {
        let _ = Command::new("tmux")
            .args([
                "-S",
                &self.socket_path.to_string_lossy(),
                "kill-session",
                "-t",
                &self.session_name,
            ])
            .status();
        Ok(())
    }
}

impl Drop for TmuxDriver {
    fn drop(&mut self) {
        let _ = self.kill_session();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Check whether tmux is available on this system.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
