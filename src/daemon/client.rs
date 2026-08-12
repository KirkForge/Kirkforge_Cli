//! Daemon client — connect to the session daemon over its transport.
//!
//! On Unix the daemon communicates over a domain socket; on Windows the
//! daemon is not implemented yet, so the client provides no-op stubs that
//! gracefully degrade to file-based session discovery.

#[cfg(unix)]
mod unix_imp {

    use crate::daemon::{
        paths, read_auth_token, read_line_limited, InstanceEvent, Request, Response,
    };
    use crate::session::session_index::SessionEntry;
    use anyhow::Context;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::io::{AsyncWriteExt, BufStream};
    use tokio::net::UnixStream;

    /// How long a single connect attempt to the daemon socket may take.
    /// ponytail: 5s is plenty for a local socket; raise if a slow filesystem
    /// flakes. Tune when measurements show it's wrong.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    /// How long the client waits for one daemon response line. The server
    /// already caps its own handler at 30s; this matches so a hung daemon
    /// surfaces to the caller rather than hanging the client forever.
    const READ_TIMEOUT: Duration = Duration::from_secs(30);

    // ponytail: test-only override hooks. Production callers never set
    // these env vars — when unset, the constants above are used. They
    // exist so the timeout tests can pin the timeout firing without
    // waiting the full 5s/30s (which would blow the test-fast.sh 60s
    // budget). Done-condition for R2: dropping the
    // `tokio::time::timeout(read_timeout(), ...)` wrap in `call()`
    // makes the test hang past its own harness timeout.
    fn connect_timeout() -> Duration {
        std::env::var("KF_TEST_DAEMON_CONNECT_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(CONNECT_TIMEOUT)
    }
    fn read_timeout() -> Duration {
        std::env::var("KF_TEST_DAEMON_READ_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(READ_TIMEOUT)
    }

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
            let timeout = connect_timeout();
            let stream = tokio::time::timeout(timeout, UnixStream::connect(&path))
                .await
                .with_context(|| {
                    format!(
                        "timed out connecting to daemon socket at {} after {timeout:?}",
                        path.display()
                    )
                })?
                .with_context(|| {
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

            let timeout = read_timeout();
            let mut line = String::new();
            let n = tokio::time::timeout(timeout, read_line_limited(&mut self.stream, &mut line))
                .await
                .with_context(|| format!("daemon response timed out after {timeout:?}"))?
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
        // Detach the daemon from the parent's stdio so it cannot hold the
        // parent's piped stdout/stderr open. Without this, any caller that
        // pipes kf-code's output (CI, test harnesses, shell pipelines) hangs
        // forever on read_to_end after the parent exits, because the daemon
        // grandchild inherits — and keeps open — the pipe write end. The
        // daemon writes its own log via tracing, not via inherited stdio.
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
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
                tracing::info!(error = %e, "daemon not running and could not be started");
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
                tracing::trace!(error = %e, "daemon not running and could not be started; skipping resolve");
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
                tracing::trace!(error = %e, "daemon not running; skipping touch");
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
                tracing::trace!(error = %e, "daemon not running; skipping jobs change notification");
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
        tokio::sync::mpsc::Receiver<InstanceEvent>,
    )> {
        use crate::daemon::InstanceEvent;

        let socket_path = paths::socket_path()?;
        let connect_to = connect_timeout();
        let stream = tokio::time::timeout(connect_to, UnixStream::connect(&socket_path))
            .await
            .with_context(|| {
                format!(
                    "timed out connecting to instance channel at {} after {connect_to:?}",
                    socket_path.display()
                )
            })?
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
        let read_to = read_timeout();
        let mut line = String::new();
        let n = tokio::time::timeout(read_to, read_line_limited(&mut stream, &mut line))
            .await
            .with_context(|| format!("instance_register ack timed out after {read_to:?}"))?
            .context("read instance_register ack")?;
        if n == 0 {
            anyhow::bail!("daemon closed connection during instance register handshake");
        }
        let ack: Response =
            serde_json::from_str(line.trim()).context("parse instance_register ack")?;
        if !matches!(ack, Response::Ok { .. }) {
            anyhow::bail!("instance_register handshake failed: {ack:?}");
        }

        let (tx, rx) =
            tokio::sync::mpsc::channel::<InstanceEvent>(crate::daemon::INSTANCE_CHANNEL_CAPACITY);

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
                                match tx.try_send(ev) {
                                    Ok(()) => {}
                                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                        tracing::warn!("instance channel full; dropping event");
                                    }
                                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                        break; // receiver dropped
                                    }
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

#[cfg(all(test, unix))]
mod tests {
    use super::unix_imp::DaemonClient;
    use crate::daemon::{read_line_limited, Response, MAX_FRAME_SIZE};
    use tokio::io::{AsyncWriteExt, BufStream};
    use tokio::net::UnixListener;
    use tokio::time::{sleep, Duration};

    /// Poll until `socket` appears on disk (the daemon bound it).
    async fn wait_for_socket(socket: &std::path::Path) {
        for _ in 0..50 {
            if socket.exists() {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
        panic!("daemon did not bind socket at {}", socket.display());
    }

    /// R1: `connect_at` against a path with no listener returns Err.
    /// ponytail: a Unix-domain connect to a nonexistent socket returns
    /// ENOENT immediately (it does not block for CONNECT_TIMEOUT), so
    /// this pins the connect-failure-surfaces-to-caller contract. The
    /// WO done-condition "dropping the 5s timeout breaks the test" is
    /// not exactly enforceable for a fast-failing Unix-domain connect;
    /// R2 below pins the timeout path itself.
    #[tokio::test]
    async fn connect_at_returns_err_for_nonexistent_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("never-bound.sock");
        let result = DaemonClient::connect_at(socket).await;
        assert!(
            result.is_err(),
            "expected Err for nonexistent socket, got Ok"
        );
    }

    /// R2: `call()` returns a timeout error when the daemon accepts but
    /// never writes a response. The test sets `KF_TEST_DAEMON_READ_TIMEOUT_MS`
    /// so the inner `tokio::time::timeout` in `call()` fires in tens of
    /// milliseconds rather than the production 30s. Done-condition: drop
    /// the `tokio::time::timeout(read_timeout(), ...)` wrap in `call()`
    /// and this test hangs past its own outer timeout.
    #[tokio::test]
    async fn call_times_out_when_daemon_never_responds() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("silent.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let accept_handle = tokio::spawn(async move {
            // Accept the connection and park forever — no response.
            let (_stream, _addr) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });

        // Shrink the inner READ_TIMEOUT so the test does not wait 30s.
        let prior = std::env::var("KF_TEST_DAEMON_READ_TIMEOUT_MS").ok();
        // SAFETY: env var is read only by client.rs test helpers; setting
        // it here only affects this test's connect_at call.
        std::env::set_var("KF_TEST_DAEMON_READ_TIMEOUT_MS", "100");

        // Outer bound so a regression (timeout wrap removed in call())
        // fails the test instead of hanging the suite.
        let connect = DaemonClient::connect_at(socket);
        let result = tokio::time::timeout(Duration::from_secs(2), connect).await;

        // Restore env before asserting so a panic does not leak.
        match prior {
            Some(v) => std::env::set_var("KF_TEST_DAEMON_READ_TIMEOUT_MS", v),
            None => std::env::remove_var("KF_TEST_DAEMON_READ_TIMEOUT_MS"),
        }
        accept_handle.abort();

        // Outer timeout should NOT have fired — connect_at should have
        // returned Err with the timeout message well within 2s.
        let connect_result = match result {
            Err(_) => panic!("connect_at did not return within 2s — timeout wrap missing?"),
            Ok(r) => r,
        };
        assert!(connect_result.is_err(), "expected timeout Err, got Ok");
        let msg = match connect_result {
            Err(e) => format!("{e}"),
            Ok(_) => unreachable!(),
        };
        assert!(
            msg.contains("timed out"),
            "error should name the timeout, got: {msg}"
        );
    }

    /// R3: a request carrying the wrong auth token is rejected. The
    /// daemon is started with one token file; the file's contents are
    /// then swapped so the client's next `read_auth_token()` returns a
    /// different value than the server cached at startup.
    #[tokio::test]
    async fn request_with_wrong_auth_token_is_rejected() {
        let _guard = crate::session::test_data_dir_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();

        let token_path = dir.path().join("token");
        std::fs::write(&token_path, "server-secret").unwrap();
        let prior_token = std::env::var("KF_CODE_DAEMON_TOKEN_FILE").ok();
        // SAFETY: test holds test_data_dir_lock; no neighbour test sets
        // this var (server.rs's auth tests remove it on a different code
        // path and don't run concurrently with this one).
        std::env::set_var(
            "KF_CODE_DAEMON_TOKEN_FILE",
            token_path.to_string_lossy().as_ref(),
        );

        let socket = dir.path().join("auth.sock");
        let pid = dir.path().join("auth.pid");
        let server_handle = tokio::spawn(crate::daemon::server::run_daemon_at(
            socket.clone(),
            pid.clone(),
        ));
        wait_for_socket(&socket).await;

        // Handshake passes — client reads the same token file.
        let mut client = DaemonClient::connect_at(socket.clone()).await.unwrap();

        // Swap the file contents so the client now reads a wrong token.
        std::fs::write(&token_path, "wrong-secret").unwrap();
        let result = client.list_recent().await;

        // Restore env before asserting so a panic does not leak.
        match &prior_token {
            Some(v) => std::env::set_var("KF_CODE_DAEMON_TOKEN_FILE", v),
            None => std::env::remove_var("KF_CODE_DAEMON_TOKEN_FILE"),
        }

        assert!(
            result.is_err(),
            "wrong auth token should be rejected, got Ok"
        );
        let msg = match result {
            Err(e) => format!("{e}"),
            Ok(_) => unreachable!(),
        };
        assert!(
            msg.contains("authentication failed"),
            "error should mention authentication, got: {msg}"
        );

        let _ = client.shutdown().await;
        drop(client);
        server_handle.abort();
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(&pid);
    }

    /// R4: a response line longer than `MAX_FRAME_SIZE` is rejected by
    /// `read_line_limited` (DoS guard on the wire format). A stub
    /// listener drains the ping request then writes the oversized line.
    #[tokio::test]
    async fn oversized_response_line_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("oversized.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut buf = BufStream::new(stream);
            // Drain the request line.
            let mut line = String::new();
            let _ = read_line_limited(&mut buf, &mut line).await;
            // Write a response line larger than MAX_FRAME_SIZE.
            let oversized = vec![b'x'; MAX_FRAME_SIZE + 1];
            buf.write_all(&oversized).await.unwrap();
            buf.write_all(b"\n").await.unwrap();
            buf.flush().await.unwrap();
            // Hold the stream open until the client reads + errors.
            std::future::pending::<()>().await;
        });

        let result = DaemonClient::connect_at(socket).await;
        assert!(
            result.is_err(),
            "oversized response should produce Err, got Ok"
        );
        server_handle.abort();
    }

    /// R5: `connect_at` bails when the daemon's `version` field does not
    /// match `CARGO_PKG_VERSION`. A stub listener echoes a synthetic
    /// `0.0.0-mismatch` version in its `Response::Ok` payload.
    #[tokio::test]
    async fn connect_at_bails_on_version_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("version.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut buf = BufStream::new(stream);
            // Drain the ping request.
            let mut line = String::new();
            let _ = read_line_limited(&mut buf, &mut line).await;
            // Respond with a version that cannot match CARGO_PKG_VERSION.
            let resp = Response::Ok {
                data: Some(serde_json::json!({ "version": "0.0.0-mismatch" })),
            };
            let out = serde_json::to_string(&resp).unwrap();
            buf.write_all(out.as_bytes()).await.unwrap();
            buf.write_all(b"\n").await.unwrap();
            buf.flush().await.unwrap();
            std::future::pending::<()>().await;
        });

        let result = DaemonClient::connect_at(socket).await;
        assert!(result.is_err(), "version mismatch should produce Err");
        let msg = match result {
            Err(e) => format!("{e}"),
            Ok(_) => unreachable!(),
        };
        assert!(
            msg.contains("version mismatch"),
            "error should mention version mismatch, got: {msg}"
        );
        server_handle.abort();
    }

    /// R6 (happy-path round-trip): pin the client surface in client.rs's
    /// own test module. `server.rs::client_server_round_trip` already
    /// exercises the same path; this anchors coverage to the file the
    /// WO targets and serves as a smoke test that connect/handshake
    /// still work end-to-end.
    #[tokio::test]
    async fn client_round_trips_ping_touch_list_resolve() {
        let _guard = crate::session::test_data_dir_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var("KF_CODE_DATA_DIR").ok();
        std::env::set_var("KF_CODE_DATA_DIR", dir.path());

        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let touch_path = sessions_dir.join("rt-session.conv.ndjson");
        std::fs::write(&touch_path, "").unwrap();

        let socket = dir.path().join("rt.sock");
        let pid = dir.path().join("rt.pid");
        let server_handle = tokio::spawn(crate::daemon::server::run_daemon_at(
            socket.clone(),
            pid.clone(),
        ));
        wait_for_socket(&socket).await;

        let mut client = DaemonClient::connect_at(socket.clone()).await.unwrap();
        client.ping().await.unwrap();
        client
            .touch("rt-session", touch_path.clone())
            .await
            .unwrap();
        let list = client.list_recent().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "rt-session");
        let resolved = client.resolve("rt-session").await.unwrap();
        assert_eq!(resolved, Some(touch_path.clone()));
        let resolved_prefix = client.resolve("rt").await.unwrap();
        assert_eq!(resolved_prefix, Some(touch_path));
        client.shutdown().await.unwrap();
        server_handle.await.unwrap().unwrap();

        assert!(!socket.exists(), "daemon left stale socket");
        assert!(!pid.exists(), "daemon left stale pid file");

        match previous {
            Some(v) => std::env::set_var("KF_CODE_DATA_DIR", v),
            None => std::env::remove_var("KF_CODE_DATA_DIR"),
        }
    }
}
