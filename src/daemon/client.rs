//! Daemon client — connect to the session daemon over its transport.
//!
//! On Unix the daemon communicates over a domain socket; on Windows the
//! daemon is not implemented yet, so the client provides no-op stubs that
//! gracefully degrade to file-based session discovery.

#[cfg(unix)]
mod unix_imp {

    /// Read the auth token from the `KF_CODE_DAEMON_TOKEN_FILE` env var.
    /// Returns `None` if the env var is not set or the file cannot be read.
    pub fn read_auth_token() -> Option<String> {
        std::env::var("KF_CODE_DAEMON_TOKEN_FILE")
            .ok()
            .and_then(|path| std::fs::read_to_string(&path).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    use crate::daemon::{paths, read_line_limited, InstanceEvent, Request, Response};
    use crate::session::session_index::SessionEntry;
    use anyhow::Context;
    use std::path::PathBuf;
    use tokio::io::{AsyncWriteExt, BufStream};
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
        /// Sends a version-gated Ping handshake and bails if the daemon
        /// version does not match the client version.
        pub async fn connect_at(path: PathBuf) -> anyhow::Result<Self> {
            let stream = UnixStream::connect(&path).await.with_context(|| {
                format!("failed to connect to daemon socket at {}", path.display())
            })?;
            let mut client = Self {
                stream: BufStream::new(stream),
            };
            // Version-gated handshake: ping with our version, bail on mismatch.
            client.ping_with_version().await?;
            Ok(client)
        }

        /// Ping the daemon, sending our version. Bails on version mismatch.
        async fn ping_with_version(&mut self) -> anyhow::Result<()> {
            let client_version = env!("CARGO_PKG_VERSION").to_string();
            match self
                .call(Request::Ping {
                    version: Some(client_version),
                    auth_token: read_auth_token(),
                })
                .await?
            {
                Response::Ok {
                    data: Some(serde_json::Value::Object(map)),
                } => {
                    if let Some(serde_json::Value::String(daemon_ver)) = map.get("version") {
                        if daemon_ver != env!("CARGO_PKG_VERSION") {
                            anyhow::bail!(
                                "version mismatch: client {}, daemon {} — restart both halves",
                                env!("CARGO_PKG_VERSION"),
                                daemon_ver
                            );
                        }
                    }
                    Ok(())
                }
                Response::Ok { .. } => Ok(()),
                Response::Error { message } => {
                    anyhow::bail!("daemon handshake failed: {message}")
                }
                Response::Busy { message } => {
                    anyhow::bail!("daemon handshake failed: {message}")
                }
            }
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
            let n = read_line_limited(&mut self.stream, &mut line)
                .await
                .context("read daemon response")?;
            if n == 0 {
                anyhow::bail!("daemon closed connection before responding");
            }
            let trimmed = line.trim();
            let resp: Response = serde_json::from_str(trimmed).context("parse daemon response")?;
            Ok(resp)
        }

        /// Health check (no version gating — use `connect_at` for the full
        /// version-gated handshake).
        #[cfg(test)]
        pub async fn ping(&mut self) -> anyhow::Result<()> {
            match self
                .call(Request::Ping {
                    version: None,
                    auth_token: read_auth_token(),
                })
                .await?
            {
                Response::Ok { .. } => Ok(()),
                Response::Error { message } => anyhow::bail!("daemon ping failed: {message}"),
                Response::Busy { message } => anyhow::bail!("daemon ping failed: {message}"),
            }
        }

        /// Return the daemon's recent sessions list.
        pub async fn list_recent(&mut self) -> anyhow::Result<Vec<SessionEntry>> {
            match self
                .call(Request::List {
                    auth_token: read_auth_token(),
                })
                .await?
            {
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
                Response::Busy { message } => anyhow::bail!("daemon list failed: {message}"),
            }
        }

        /// Resolve a session id or prefix to a log path.
        pub async fn resolve(&mut self, id_or_prefix: &str) -> anyhow::Result<Option<PathBuf>> {
            match self
                .call(Request::Resolve {
                    id: id_or_prefix.to_string(),
                    auth_token: read_auth_token(),
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
                Response::Busy { message } => anyhow::bail!("daemon resolve failed: {message}"),
            }
        }

        /// Tell the daemon that a session was just opened.
        pub async fn touch(&mut self, id: &str, path: PathBuf) -> anyhow::Result<()> {
            match self
                .call(Request::Touch {
                    id: id.to_string(),
                    path: path.into(),
                    auth_token: read_auth_token(),
                })
                .await?
            {
                Response::Ok { .. } => Ok(()),
                Response::Error { message } => anyhow::bail!("daemon touch failed: {message}"),
                Response::Busy { message } => anyhow::bail!("daemon touch failed: {message}"),
            }
        }

        /// Ask the daemon to shut down.
        pub async fn shutdown(&mut self) -> anyhow::Result<()> {
            match self
                .call(Request::Shutdown {
                    auth_token: read_auth_token(),
                })
                .await?
            {
                Response::Ok { .. } => Ok(()),
                Response::Error { message } => {
                    anyhow::bail!("daemon shutdown failed: {message}")
                }
                Response::Busy { message } => anyhow::bail!("daemon shutdown failed: {message}"),
            }
        }

        /// Claim a session. Returns `Ok(true)` if the claim was granted,
        /// `Ok(false)` if the session was already claimed by another connection.
        pub async fn claim(&mut self, id: &str) -> anyhow::Result<bool> {
            match self
                .call(Request::Claim {
                    id: id.to_string(),
                    auth_token: read_auth_token(),
                })
                .await?
            {
                Response::Ok { .. } => Ok(true),
                Response::Busy { .. } => Ok(false),
                Response::Error { message } => anyhow::bail!("daemon claim failed: {message}"),
            }
        }

        /// Ask the daemon to broadcast Quit to all connected TUIs and shut down.
        pub async fn quit_all(&mut self) -> anyhow::Result<()> {
            match self
                .call(Request::QuitAll {
                    auth_token: read_auth_token(),
                })
                .await?
            {
                Response::Ok { .. } => Ok(()),
                Response::Error { message } => {
                    anyhow::bail!("daemon quit_all failed: {message}")
                }
                Response::Busy { message } => anyhow::bail!("daemon quit_all failed: {message}"),
            }
        }

        /// Notify the daemon that scheduled jobs changed, so it can push
        /// `JobsChanged` to all registered TUI instances.
        pub async fn notify_jobs_changed(&mut self) -> anyhow::Result<()> {
            match self
                .call(Request::NotifyJobsChanged {
                    auth_token: read_auth_token(),
                })
                .await?
            {
                Response::Ok { .. } => Ok(()),
                Response::Error { message } => {
                    anyhow::bail!("daemon notify_jobs_changed failed: {message}")
                }
                Response::Busy { message } => {
                    anyhow::bail!("daemon notify_jobs_changed failed: {message}")
                }
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

    /// Convenience: notify the session daemon that scheduled jobs changed.
    /// The daemon will push `JobsChanged` to all connected TUI instances.
    /// Best-effort; logs a debug message if the daemon is not running.
    pub async fn try_notify_jobs_changed() {
        match DaemonClient::connect().await {
            Ok(mut c) => {
                if let Err(e) = c.notify_jobs_changed().await {
                    tracing::warn!(error = %e, "failed to notify daemon of jobs change");
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "daemon not running; skipping jobs change notification");
            }
        }
    }

    /// Open a persistent instance channel to the session daemon and return
    /// a reader that yields `InstanceEvent`s. The daemon pushes events
    /// (session list changes, job updates, quit) to this channel for the
    /// lifetime of the connection.
    ///
    /// Returns `(join_handle, receiver)` where the join handle drives the
    /// background reader task. Drop the receiver to signal shutdown;
    /// abort the join handle to force-close.
    pub async fn connect_instance_channel() -> anyhow::Result<(
        tokio::task::JoinHandle<()>,
        tokio::sync::mpsc::UnboundedReceiver<InstanceEvent>,
    )> {
        use crate::daemon::InstanceEvent;

        let socket_path = paths::socket_path()?;
        let stream = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("connect to instance channel at {}", socket_path.display()))?;
        let mut stream = BufStream::new(stream);

        // Send registration request.
        let req = serde_json::to_string(&Request::InstanceRegister {
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            auth_token: read_auth_token(),
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
        let ack: Response =
            serde_json::from_str(line.trim()).context("parse instance_register ack")?;
        if !matches!(ack, Response::Ok { .. }) {
            anyhow::bail!("instance_register handshake failed: {ack:?}");
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<InstanceEvent>();

        // Background reader: read NDJSON InstanceEvent lines and forward
        // them to the channel. Runs until the stream closes or errors.
        let join = tokio::spawn(async move {
            let mut stream = stream;
            let mut line = String::new();
            loop {
                line.clear();
                match read_line_limited(&mut stream, &mut line).await {
                    Ok(0) => break, // stream closed
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<InstanceEvent>(trimmed) {
                            Ok(ev) => {
                                if tx.send(ev).is_err() {
                                    break; // receiver dropped
                                }
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

        Ok((join, rx))
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
            Err(anyhow::anyhow!(
                "session daemon is not supported on Windows"
            ))
        }

        pub async fn shutdown(&mut self) -> anyhow::Result<()> {
            Self::connect().await.map(|_| ())
        }

        pub async fn claim(&mut self, _id: &str) -> anyhow::Result<bool> {
            Ok(true)
        }

        pub async fn quit_all(&mut self) -> anyhow::Result<()> {
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
