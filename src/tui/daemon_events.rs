//! TUI daemon event reader — subscribes to the daemon's instance channel
//! and maps `InstanceEvent`s to `AppState` dirty flags so the TUI can
//! re-list sessions or re-read jobs without polling.
//!
//! The reader task is non-blocking: it sets boolean flags on `AppState`
//! that the draw loop checks on the next tick. This matches vix's
//! approach of pushing a "dirty" signal rather than doing a synchronous
//! RPC per frame.

use crate::daemon::paths;
use crate::daemon::read_line_limited;
use crate::daemon::{InstanceEvent, Request, Response};
use anyhow::Context;
use tokio::io::{AsyncWriteExt, BufStream};
use tokio::net::UnixStream;

/// Flags set by the daemon event reader to signal that the TUI should
/// re-fetch data from the daemon on the next draw tick.
#[derive(Debug, Default)]
pub struct DaemonEventFlags {
    /// When true, the session list has changed and the TUI should
    /// re-list recent sessions from the daemon.
    pub sessions_dirty: bool,
    /// When true, a job has changed and the TUI should re-read the
    /// job list.
    pub jobs_dirty: bool,
}

/// Open a persistent instance channel to the daemon and spawn a background
/// reader task that maps `InstanceEvent`s to flag updates on the provided
/// `Arc<Mutex<DaemonEventFlags>>`.
///
/// Returns the `JoinHandle` of the reader task so the caller can abort it
/// on shutdown. If the daemon is not running, logs a warning and returns
/// `Ok(None)` — the TUI simply won't get push notifications and falls back
/// to polling.
pub async fn spawn_daemon_event_reader(
    flags: std::sync::Arc<std::sync::Mutex<DaemonEventFlags>>,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let socket_path = match paths::socket_path() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "could not resolve daemon socket path; skipping instance channel");
            return Ok(None);
        }
    };

    let stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "daemon not running; skipping instance channel");
            return Ok(None);
        }
    };

    let mut stream = BufStream::new(stream);

    // Send the InstanceRegister request.
    let req = serde_json::to_string(&Request::InstanceRegister {
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        auth_token: crate::daemon::client::read_auth_token(),
    })
    .context("serialise InstanceRegister")?;
    stream
        .write_all(req.as_bytes())
        .await
        .context("write InstanceRegister")?;
    stream.write_all(b"\n").await.context("write newline")?;
    stream.flush().await.context("flush InstanceRegister")?;

    // Read handshake ack.
    let mut line = String::new();
    let n = read_line_limited(&mut stream, &mut line)
        .await
        .context("read instance_register ack")?;
    if n == 0 {
        anyhow::bail!("daemon closed connection during instance register handshake");
    }
    let ack: Response = serde_json::from_str(line.trim()).context("parse instance_register ack")?;
    if !matches!(ack, Response::Ok { .. }) {
        anyhow::bail!("instance_register handshake failed: {ack:?}");
    }

    // Spawn the reader task.
    let join = tokio::spawn(async move {
        let mut stream = stream;
        let mut line = String::new();
        loop {
            line.clear();
            match read_line_limited(&mut stream, &mut line).await {
                Ok(0) => {
                    tracing::debug!("daemon instance channel closed");
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<InstanceEvent>(trimmed) {
                        Ok(InstanceEvent::ThreadsChanged) => {
                            if let Ok(mut f) = flags.lock() {
                                f.sessions_dirty = true;
                            }
                        }
                        Ok(InstanceEvent::JobsChanged) => {
                            if let Ok(mut f) = flags.lock() {
                                f.jobs_dirty = true;
                            }
                        }
                        Ok(InstanceEvent::Quit) => {
                            tracing::info!("daemon sent Quit; instance channel closing");
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to parse InstanceEvent from daemon");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "instance channel read error");
                    break;
                }
            }
        }
    });

    Ok(Some(join))
}
