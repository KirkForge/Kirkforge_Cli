//! E2E test harness: binary driver, mock provider, PTY driver, and
//! regression catalog.  See docs/archive/workorders/17.8-e2e-test-harness.md
//! for the design rationale.

#[allow(dead_code)]
pub mod artifact;
pub mod confirm;
pub mod mock;
pub mod shard;
pub mod ui;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Ceiling for how long a spawned `kf-code` binary may run in an e2e test
/// before the harness kills it. nextest's `timeout-period` is 60s in CI, so
/// this fires first and the test gets a real error result (with stdout/stderr)
/// instead of a nextest hard-kill with no output. Real e2e turns complete in
/// 2-5s against the mock; 30s is a generous ceiling that catches hangs.
const E2E_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling for draining the child's stdout/stderr pipes AFTER the direct
/// child has exited. Normally EOF arrives instantly on exit; the only way
/// this ceiling is reached is a grandchild holding the pipe write-end open
/// (the historical daemon pipe-inheritance bug). 5s is a generous bound
/// that turns that regression into a named `TimedOut` error instead of the
/// infinite hang a bare `read_to_end` would produce.
const PIPE_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Wait for a child to exit, collecting stdout/stderr into an `Output`.
/// If the child does not exit within `E2E_TIMEOUT`, it is killed and an
/// `io::TimedOut` error is returned. This is the e2e-hang defense: the
/// prior `child.wait_with_output()` blocked forever if `kf-code` wedged,
/// and nextest's 60s kill orphaned the spawned binary (no kill_on_drop).
pub fn wait_with_timeout(child: &mut Child) -> std::io::Result<Output> {
    let deadline = Instant::now() + E2E_TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "e2e: kf-code did not exit within {}s; killed",
                            E2E_TIMEOUT.as_secs()
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let stdout = read_with_deadline(child.stdout.take(), PIPE_READ_TIMEOUT)?;
    let stderr = read_with_deadline(child.stderr.take(), PIPE_READ_TIMEOUT)?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Drain a child pipe into a `Vec<u8>` with a hard deadline. `read_to_end`
/// only EOFs when every writer holding the pipe's write-end has closed it —
/// including inherited copies in spawned grandchildren. If a grandchild
/// keeps the pipe open (the daemon pipe-inheritance regression), a direct
/// `read_to_end` would block forever; this bounds it so the test surfaces a
/// named `TimedOut` error. The reader thread is detached on timeout (it exits
/// naturally when the pipe is eventually closed or the process tree reaped).
fn read_with_deadline<R: Read + Send + 'static>(
    pipe: Option<R>,
    timeout: Duration,
) -> std::io::Result<Vec<u8>> {
    let mut s = match pipe {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    let (tx, rx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = tx.send(s.read_to_end(&mut buf).map(|_| buf));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(buf)) => Ok(buf),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "e2e: child pipe did not EOF within {}s after exit — a grandchild likely holds the pipe open",
                timeout.as_secs()
            ),
        )),
    }
}

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
             auto_approve = false\n\
             \n\
             [adapter_routing]\n\
             \"e2e-\" = \"Ollama\"\n"
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
    /// The caller adds subcommand args and launches. NOTE: std
    /// `process::Command` has no `kill_on_drop` (that's tokio-only), so
    /// callers MUST pass the spawned child to `wait_with_timeout`, which
    /// kills on the 30s ceiling. A panic between spawn and wait could
    /// orphan the binary; the adapter's own STREAM_IDLE_TIMEOUT (90s)
    /// bounds that residual case.
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
    /// is delivered. Returns the captured output. Bounded by
    /// `E2E_TIMEOUT` — see `wait_with_timeout`.
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
            writeln!(stdin, "{prompt}")?;
        }
        drop(child.stdin.take());
        wait_with_timeout(&mut child)
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
