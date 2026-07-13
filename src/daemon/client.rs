//! Daemon client — connect to the session daemon over its transport.
//!
//! On Unix the daemon communicates over a domain socket; on Windows the
//! daemon is not implemented yet, so the client provides no-op stubs that
//! gracefully degrade to file-based session discovery.

#[cfg(unix)]
mod unix_imp {
    use crate::daemon::{paths, Request, Response};
    use crate::session::session_index::SessionEntry;
    use anyhow::Context;
    use std::path::PathBuf;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufStream};
    use tokio::net::UnixStream;

    /// Client handle to the session daemon.
    pub struct DaemonClient {
        stream: BufStream<UnixStream>,
    }

    impl DaemonClient {
        /// Try to connect to the running daemon at the canonical socket path.
        pub async fn connect() -> anyhow::Result<Self> {
            Self::connect_at(paths::socket_path()?).await
        }

        /// Try to connect to a daemon at an explicit socket path.
        pub async fn connect_at(path: PathBuf) -> anyhow::Result<Self> {
            let stream = UnixStream::connect(&path).await.with_context(|| {
                format!("failed to connect to daemon socket at {}", path.display())
            })?;
            Ok(Self {
                stream: BufStream::new(stream),
            })
        }

        /// Send a request and wait for one response line.
        async fn call(&mut self, req: Request) -> anyhow::Result<Response> {
            let line = serde_json::to_string(&req).context("serialize daemon request")?;
            self.stream
                .write_all(line.as_bytes())
                .await
                .context("write daemon request")?;
            self.stream
                .write_all(b"\n")
                .await
                .context("write newline")?;
            self.stream.flush().await.context("flush daemon request")?;

            let mut line = String::new();
            let n = self
                .stream
                .read_line(&mut line)
                .await
                .context("read daemon response")?;
            if n == 0 {
                anyhow::bail!("daemon closed connection before responding");
            }
            let trimmed = line.trim();
            let resp: Response = serde_json::from_str(trimmed).context("parse daemon response")?;
            Ok(resp)
        }

        /// Health check.
        #[cfg(test)]
        pub async fn ping(&mut self) -> anyhow::Result<()> {
            match self.call(Request::Ping).await? {
                Response::Ok { .. } => Ok(()),
                Response::Error { message } => anyhow::bail!("daemon ping failed: {message}"),
            }
        }

        /// Return the daemon's recent sessions list.
        pub async fn list_recent(&mut self) -> anyhow::Result<Vec<SessionEntry>> {
            match self.call(Request::List).await? {
                Response::Ok {
                    data: Some(serde_json::Value::Object(mut map)),
                } => {
                    let arr = match map.remove("sessions") {
                        Some(serde_json::Value::Array(a)) => a,
                        _ => return Ok(Vec::new()),
                    };
                    let mut out = Vec::with_capacity(arr.len());
                    for v in arr {
                        out.push(
                            serde_json::from_value::<SessionEntry>(v)
                                .context("parse session entry")?,
                        );
                    }
                    Ok(out)
                }
                Response::Ok { .. } => Ok(Vec::new()),
                Response::Error { message } => anyhow::bail!("daemon list failed: {message}"),
            }
        }

        /// Resolve a session id or prefix to a log path.
        pub async fn resolve(&mut self, id_or_prefix: &str) -> anyhow::Result<Option<PathBuf>> {
            match self
                .call(Request::Resolve {
                    id: id_or_prefix.to_string(),
                })
                .await?
            {
                Response::Ok {
                    data: Some(serde_json::Value::Object(mut map)),
                } => {
                    if let Some(serde_json::Value::String(p)) = map.remove("path") {
                        Ok(Some(PathBuf::from(p)))
                    } else {
                        Ok(None)
                    }
                }
                Response::Ok { .. } => Ok(None),
                Response::Error { message } => anyhow::bail!("daemon resolve failed: {message}"),
            }
        }

        /// Tell the daemon that a session was just opened.
        pub async fn touch(&mut self, id: &str, path: PathBuf) -> anyhow::Result<()> {
            match self
                .call(Request::Touch {
                    id: id.to_string(),
                    path: path.into(),
                })
                .await?
            {
                Response::Ok { .. } => Ok(()),
                Response::Error { message } => anyhow::bail!("daemon touch failed: {message}"),
            }
        }

        /// Ask the daemon to shut down.
        pub async fn shutdown(&mut self) -> anyhow::Result<()> {
            match self.call(Request::Shutdown).await? {
                Response::Ok { .. } => Ok(()),
                Response::Error { message } => anyhow::bail!("daemon shutdown failed: {message}"),
            }
        }
    }

    /// Try to start a daemon process in the background.
    fn start_daemon() -> anyhow::Result<()> {
        let current_exe = std::env::current_exe().context("get current executable")?;
        let mut cmd = std::process::Command::new(current_exe);
        cmd.arg("daemon");
        cmd.spawn().context("spawn daemon process")?;
        Ok(())
    }

    /// Wait for the daemon socket to become connectable.
    async fn wait_for_daemon(timeout: std::time::Duration) -> anyhow::Result<()> {
        let socket_path = paths::socket_path()?;
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if socket_path.exists() && DaemonClient::connect().await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        anyhow::bail!("daemon did not become reachable within {timeout:?}")
    }

    /// Ensure the daemon is running, starting it if necessary.
    async fn ensure_daemon_running() -> anyhow::Result<()> {
        if DaemonClient::connect().await.is_ok() {
            return Ok(());
        }
        tracing::info!("starting session daemon in the background");
        start_daemon()?;
        wait_for_daemon(std::time::Duration::from_secs(2)).await
    }

    /// Convenience: list recent sessions via the daemon, starting it if needed.
    /// Returns `Ok(None)` only if the daemon could not be reached even after an
    /// auto-start attempt.
    pub async fn try_list_recent() -> anyhow::Result<Option<Vec<SessionEntry>>> {
        if DaemonClient::connect().await.is_err() {
            if let Err(e) = ensure_daemon_running().await {
                tracing::debug!(error = %e, "daemon not running and could not be started");
                return Ok(None);
            }
        }
        let mut c = DaemonClient::connect().await?;
        Ok(Some(c.list_recent().await?))
    }

    /// Convenience: resolve the most recent session via the daemon.
    pub async fn try_resolve_recent() -> anyhow::Result<Option<PathBuf>> {
        let sessions = match try_list_recent().await? {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(None),
        };
        let first = sessions
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("daemon returned empty session list"))?;
        Ok(Some(first.path))
    }

    /// Convenience: resolve a specific id/prefix via the daemon, starting it if
    /// needed.
    pub async fn try_resolve_id(id_or_prefix: &str) -> anyhow::Result<Option<PathBuf>> {
        if DaemonClient::connect().await.is_err() {
            if let Err(e) = ensure_daemon_running().await {
                tracing::debug!(error = %e, "daemon not running and could not be started; skipping resolve");
                return Ok(None);
            }
        }
        let mut c = DaemonClient::connect().await?;
        c.resolve(id_or_prefix).await
    }

    /// Convenience: touch a session if the daemon is reachable. This helper does
    /// *not* auto-start the daemon so that a run that never requested the daemon
    /// stays self-contained.
    pub async fn try_touch(id: &str, path: PathBuf) {
        match DaemonClient::connect().await {
            Ok(mut c) => {
                if let Err(e) = c.touch(id, path).await {
                    tracing::warn!(error = %e, "failed to touch session in daemon");
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "daemon not running; skipping touch");
            }
        }
    }
}

#[cfg(windows)]
mod windows_imp {
    use crate::session::session_index::SessionEntry;
    use std::path::PathBuf;

    /// Placeholder client for platforms that do not implement the session daemon.
    pub struct DaemonClient;

    impl DaemonClient {
        pub async fn connect() -> anyhow::Result<Self> {
            Err(anyhow::anyhow!(
                "session daemon is not supported on Windows; use file-based session commands"
            ))
        }

        pub async fn connect_at(_path: PathBuf) -> anyhow::Result<Self> {
            Self::connect().await
        }

        pub async fn list_recent(&mut self) -> anyhow::Result<Vec<SessionEntry>> {
            Ok(Vec::new())
        }

        pub async fn resolve(&mut self, _id_or_prefix: &str) -> anyhow::Result<Option<PathBuf>> {
            Ok(None)
        }

        pub async fn touch(&mut self, _id: &str, _path: PathBuf) -> anyhow::Result<()> {
            Ok(())
        }

        pub async fn shutdown(&mut self) -> anyhow::Result<()> {
            Self::connect().await.map(|_| ())
        }
    }

    pub async fn try_list_recent() -> anyhow::Result<Option<Vec<SessionEntry>>> {
        Ok(None)
    }

    pub async fn try_resolve_recent() -> anyhow::Result<Option<PathBuf>> {
        Ok(None)
    }

    pub async fn try_resolve_id(_id_or_prefix: &str) -> anyhow::Result<Option<PathBuf>> {
        Ok(None)
    }

    pub async fn try_touch(_id: &str, _path: PathBuf) {
        // no-op: session index file is the source of truth on Windows
    }
}

#[cfg(unix)]
pub use unix_imp::*;
#[cfg(windows)]
pub use windows_imp::*;
