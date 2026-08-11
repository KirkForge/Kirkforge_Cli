//! E2E test harness: binary driver, mock provider, PTY driver, and
//! regression catalog.  See docs/workorders/17.8-e2e-test-harness.md
//! for the design rationale.

#[allow(dead_code)]
pub mod artifact;
pub mod confirm;
pub mod mock;
pub mod shard;
pub mod ui;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

/// Isolated environment for one e2e test.  Creates a temp HOME + XDG
/// dirs, a temp socket dir, and a seed config.toml pointing at the
/// given mock URL.  On drop the daemon (if running) is stopped and
/// the temp dirs are cleaned up.
pub struct IsolatedEnv {
    /// Tempdir that holds HOME, .local/share/kf-code (data), and the
    /// daemon socket.  Kept alive so the dirs survive for the test
    /// lifetime and are cleaned on drop.
    root: tempfile::TempDir,
    /// Path to the kf-code binary resolved via CARGO_BIN_EXE.
    bin: PathBuf,
    /// Daemon child handle (set after `start_daemon`).
    daemon: Option<Child>,
}

impl IsolatedEnv {
    /// Build an isolated env with a fresh tempdir and a config.toml that
    /// points the binary at the mock provider URL.
    pub fn new(mock_url: &str, model: &str) -> Self {
        let root = tempfile::tempdir().expect("e2e: create tempdir");
        let bin = PathBuf::from(env!("CARGO_BIN_EXE_kf-code"));

        // Write config.toml into the data dir so the binary picks up
        // the mock URL and model.
        let data_dir = root.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("e2e: create data dir");
        let config_content = format!(
            "default_model = \"{model}\"\n\
             ollama_host = \"{mock_url}\"\n\
             auto_approve = false\n"
        );
        std::fs::write(data_dir.join("config.toml"), config_content)
            .expect("e2e: write config.toml");

        // Create sessions dir so the daemon doesn't complain.
        let sessions_dir = data_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("e2e: create sessions dir");

        Self {
            root,
            bin,
            daemon: None,
        }
    }

    /// Path to the isolated data directory (HOME/.local/share/kf-code).
    pub fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    /// Path to the daemon socket inside the isolated data dir.
    pub fn socket_path(&self) -> PathBuf {
        self.data_dir().join("daemon.sock")
    }

    /// Path to the kf-code log file.
    pub fn log_path(&self) -> PathBuf {
        self.data_dir().join("kf-code.log")
    }

    /// Build a `Command` for `kf-code` with the isolated env vars set.
    /// The caller adds subcommand args and launches.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .env("KF_CODE_DATA_DIR", self.data_dir())
            .env("HOME", self.root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Run `kf-code` with the given args, piping `prompt` to stdin as a
    /// single line (then EOF). `kf-code run --non-interactive` reads each
    /// non-empty stdin line as one turn, so this is how a headless prompt
    /// is delivered. Returns the captured output.
    pub fn run_with_prompt(
        &self,
        args: &[&str],
        prompt: &str,
    ) -> std::io::Result<std::process::Output> {
        use std::io::Write;
        let mut cmd = self.command(args);
        cmd.stdin(Stdio::piped());
        let mut child = cmd.spawn()?;
        {
            let stdin = child.stdin.as_mut().expect("e2e: stdin");
            writeln!(stdin, "{prompt}").expect("e2e: write prompt to stdin");
        }
        drop(child.stdin.take());
        child.wait_with_output()
    }

    /// Start `kf-code daemon --foreground` in the isolated env.
    /// Returns the daemon's PID.  The daemon is stopped on drop.
    #[allow(dead_code)]
    pub fn start_daemon(&mut self) -> u32 {
        let mut cmd = self.command(&["daemon", "--foreground"]);
        let child = cmd.spawn().expect("e2e: spawn daemon");
        let pid = child.id();
        self.daemon = Some(child);
        pid
    }

    /// Stop the daemon (SIGTERM then wait).  Called automatically on drop.
    pub fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Return the kf-code binary path.
    #[allow(dead_code)]
    pub fn bin(&self) -> &PathBuf {
        &self.bin
    }

    /// Root tempdir path (useful for artifact collection).
    #[allow(dead_code)]
    pub fn root_path(&self) -> &std::path::Path {
        self.root.path()
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        self.stop_daemon();
    }
}

/// A shared `IsolatedEnv` behind an `Arc` so the harness can be passed
/// to scenario closures while the `Drop` guard stays alive for the
/// whole test.
#[allow(dead_code)]
pub type SharedEnv = Arc<std::sync::Mutex<IsolatedEnv>>;
