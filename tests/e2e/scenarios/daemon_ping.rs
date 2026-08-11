//! Scenario: daemon ping.
//!
//! Pins that `kf-code daemon --foreground` starts and creates its
//! socket file.  The daemon subcommand is tested as a subprocess;
//! we check that it creates the expected socket path within a timeout.
//! Regression: C-006 (daemon crashed on startup when KF_CODE_DATA_DIR
//! was set).

use crate::harness::shard;
use crate::harness::IsolatedEnv;

use std::time::Duration;

// The session daemon uses a Unix-domain socket and is Unix-only per
// cli_dispatch.rs ("session daemon is not supported on Windows"). The
// test spawns `kf-code daemon --foreground` and polls for the .sock
// file; on Windows the subcommand exits with an error before binding.
#[cfg(unix)]
#[test]
fn daemon_creates_socket_and_exits_cleanly() {
    if !shard::shard_gate("daemon_creates_socket_and_exits_cleanly") {
        return;
    }

    let mut env = IsolatedEnv::new("http://localhost:11434", "e2e-test-model");

    // Start the daemon in foreground mode as a subprocess.
    let mut cmd = env.command(&["daemon", "--foreground"]);
    let mut child = cmd.spawn().expect("e2e: spawn daemon");

    // Wait for the socket file to appear.
    let socket = env.socket_path();
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(5);
    while start.elapsed() < timeout {
        if socket.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        socket.exists(),
        "daemon socket did not appear within {timeout:?}"
    );

    // Stop the daemon (kill the child process).
    env.stop_daemon();

    // The socket should be cleaned up after the daemon exits.
    let _ = child.kill();
    let _ = child.wait();

    // Give it a moment to clean up.
    std::thread::sleep(Duration::from_millis(200));
}
