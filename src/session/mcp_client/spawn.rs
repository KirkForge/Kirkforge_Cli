//! Child-process spawn helpers for the MCP client: a stderr drain that
//! keeps a verbose server from deadlocking on a full error pipe, and an
//! async child reaper used on disconnect/drop.

use crate::session::process_group::reap_child;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, ChildStderr};
use tokio::sync::oneshot;

/// Spawn a task that drains a child's stderr into tracing logs.
pub(super) fn spawn_stderr_drain(
    stderr: Option<ChildStderr>,
    mut shutdown: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(stderr) = stderr else { return };
        let mut reader = tokio::io::BufReader::new(stderr);
        let mut buf = String::new();
        loop {
            buf.clear();
            tokio::select! {
                biased;
                _ = &mut shutdown => break,
                result = reader.read_line(&mut buf) => {
                    match result {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if !buf.is_empty() {
                                let line = buf.trim_end_matches('\n').trim_end_matches('\r');
                                if !line.is_empty() {
                                    tracing::debug!(target: "mcp_stderr", "{}", line);
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// Kill a child and reap it asynchronously, bounded by a short timeout.
pub(super) fn spawn_child_reap(mut child: Child) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        reap_child(&mut child, Duration::from_secs(2)).await;
    })
}
