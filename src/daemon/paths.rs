//! Paths used by the session daemon.
//!
//! Everything lives under `~/.local/share/kirkforge/`:
//!   - `daemon.sock` — Unix domain socket for client/daemon RPC
//!   - `daemon.pid`  — PID file so `--stop` and health checks can find it

use std::path::PathBuf;

/// Base data directory for kirkforge.
pub fn data_dir() -> anyhow::Result<PathBuf> {
    crate::session::data_dir()
}

/// Path to the daemon's Unix domain socket.
pub fn socket_path() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("daemon.sock"))
}

/// Path to the daemon's PID file.
pub fn pid_file_path() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("daemon.pid"))
}
