//! Daemon server — listens on a Unix domain socket and serves JSON-RPC.

use crate::daemon::paths;
use crate::daemon::{DaemonState, InstanceEvent, Request, Response};
use anyhow::Context;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufStream};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Semaphore};

/// Run the daemon until a shutdown request is received.
///
/// This is the production entry point. It resolves the canonical socket/pid
/// paths, optionally backgrounds itself, and then runs the event loop in
/// [`run_daemon_at`].
pub async fn run_daemon(foreground: bool, stop: bool) -> anyhow::Result<()> {
    let socket_path = paths::socket_path()?;
    let pid_path = paths::pid_file_path()?;

    if stop {
        return stop_daemon(&socket_path, &pid_path).await;
    }

    if !foreground {
        crate::daemon::daemonize(["daemon", "--foreground"])?;
    }

    run_daemon_at(socket_path, pid_path).await
}

/// Run the daemon event loop on the supplied socket and pid paths.
///
/// This is public so tests can spin up an isolated daemon in a temporary
/// directory without touching the production socket or environment.
pub async fn run_daemon_at(socket_path: PathBuf, pid_path: PathBuf) -> anyhow::Result<()> {
    // Make sure the data directory exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).context("create data directory")?;
    }

    // Socket guard: before removing and binding, try to connect. If a
    // connection succeeds, another daemon is live — refuse to hijack it.
    if socket_path.exists() {
        match tokio::net::UnixStream::connect(&socket_path).await {
            Ok(_) => {
                anyhow::bail!(
                    "daemon already running on {} — stop it first",
                    socket_path.display()
                );
            }
            Err(_) => {
                // Connection failed — the socket is stale. Safe to remove.
                if let Err(e) = std::fs::remove_file(&socket_path) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(error = %e, path = %socket_path.display(), "Failed to remove stale daemon socket");
                    }
                }
            }
        }
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind daemon socket at {}", socket_path.display()))?;

    // Write PID file only after the socket is bound. If bind fails, no
    // stale PID is left behind for a daemon that never started.
    let pid = std::process::id();
    if let Err(e) = std::fs::write(&pid_path, format!("{pid}\n")) {
        tracing::warn!(error = %e, path = %pid_path.display(), "Failed to write daemon PID file");
    }

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let concurrency = Arc::new(Semaphore::new(16));

    // Clean up orphaned undo snapshots from deleted/pruned sessions (F16).
    crate::session::undo::cleanup_orphan_snaps();

    // Initial refresh.
    {
        let mut s = state.lock().await;
        s.refresh();
    }

    // Signal handlers: shared SIGINT/SIGHUP/SIGTERM → shutdown Notify.
    crate::daemon::spawn_shutdown_signal_handlers(shutdown.clone());

    tracing::info!(
        socket = %socket_path.display(),
        "session daemon listening"
    );

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                tracing::info!("daemon shutting down gracefully");
                break;
            }
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let shutdown = shutdown.clone();
                        let concurrency = concurrency.clone();
                        tokio::spawn(async move {
                            let Ok(permit) = concurrency.acquire_owned().await else {
                                tracing::warn!("daemon concurrency semaphore closed; dropping connection");
                                return;
                            };
                            handle_client(stream, state, shutdown, permit).await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "daemon accept failed");
                    }
                }
            }
        }
    }

    if let Err(e) = std::fs::remove_file(&socket_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                error = %e,
                path = %socket_path.display(),
                "Failed to remove daemon socket at shutdown"
            );
        }
    }
    if let Err(e) = std::fs::remove_file(&pid_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                error = %e,
                path = %pid_path.display(),
                "Failed to remove daemon PID file at shutdown"
            );
        }
    }
    Ok(())
}

/// Ask a running daemon to shut down.
async fn stop_daemon(socket_path: &PathBuf, pid_path: &PathBuf) -> anyhow::Result<()> {
    use crate::daemon::client::DaemonClient;
    match DaemonClient::connect().await {
        Ok(mut c) => {
            c.shutdown().await?;
            tracing::info!("daemon shutdown requested");
        }
        Err(e) => {
            tracing::warn!(error = %e, "no daemon reachable; cleaning up stale files");
            if let Err(e) = std::fs::remove_file(socket_path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        error = %e,
                        path = %socket_path.display(),
                        "Failed to remove stale daemon socket"
                    );
                }
            }
        }
    }
    if let Err(e) = std::fs::remove_file(pid_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                error = %e,
                path = %pid_path.display(),
                "Failed to remove daemon PID file"
            );
        }
    }
    Ok(())
}

async fn handle_client(
    stream: UnixStream,
    state: Arc<Mutex<DaemonState>>,
    shutdown: Arc<tokio::sync::Notify>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let mut stream = BufStream::new(stream);
    let mut line = String::new();
    let mut claimed_sessions: Vec<String> = Vec::new();

    loop {
        line.clear();
        match crate::daemon::read_line_limited(&mut stream, &mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "daemon read from client failed");
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Request = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::error(format!("invalid request: {e}"));
                if write_response(&mut stream, resp).await.is_err() {
                    break;
                }
                continue;
            }
        };

        // InstanceRegister is a long-lived control channel, not a
        // request/response pair. Hand off to a dedicated handler that
        // holds the stream open and pushes events.
        if let Request::InstanceRegister {
            version: client_version,
            auth_token,
        } = req
        {
            {
                let s = state.lock().await;
                if let Err(e) = s.check_auth(auth_token.as_deref()) {
                    if write_response(&mut stream, e).await.is_err() {
                        break;
                    }
                    continue;
                }
                if !kf_rbac::has_permission(
                    &s.actor_for_token(auth_token.as_deref()),
                    kf_rbac::Permission::ViewerStatus,
                ) {
                    let resp = Response::error(format!(
                        "forbidden: requires {}",
                        kf_rbac::Permission::ViewerStatus.as_str()
                    ));
                    if write_response(&mut stream, resp).await.is_err() {
                        break;
                    }
                    continue;
                }
                if let Some(ref cv) = client_version {
                    let daemon_version = env!("CARGO_PKG_VERSION");
                    if cv != daemon_version {
                        let resp = Response::error(format!(
                            "version mismatch: client {cv}, daemon {daemon_version} — restart both halves"
                        ));
                        if write_response(&mut stream, resp).await.is_err() {
                            break;
                        }
                        continue;
                    }
                }
            }
            // InstanceRegister is a long-lived control channel, not a
            // request/response pair. Hand off to a dedicated handler that
            // holds the stream open and pushes events. Release the
            // concurrency permit first: the handshake above (auth + version
            // check) is done, and the push loop parks indefinitely — holding
            // the permit for the connection lifetime would starve all 16
            // slots once 16 TUIs attach, blocking new `kf-code run --attach`.
            drop(permit);
            handle_instance_register(stream, state).await;
            return;
        }

        let is_shutdown = matches!(req, Request::Shutdown { .. } | Request::QuitAll { .. });
        let resp = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            handle_request(req, state.clone(), &mut claimed_sessions),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!("daemon request handler timed out; closing connection");
                if let Err(e) = write_response(
                    &mut stream,
                    Response::error("request timed out".to_string()),
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to write timeout response to daemon client");
                }
                break;
            }
        };

        // If this was a Claim that succeeded, track it for cleanup on disconnect.
        if let Response::Ok {
            data: Some(serde_json::Value::Object(ref map)),
        } = resp
        {
            if let Some(serde_json::Value::String(id)) = map.get("claimed") {
                claimed_sessions.push(id.clone());
            }
        }

        if write_response(&mut stream, resp).await.is_err() {
            break;
        }
        if is_shutdown {
            shutdown.notify_one();
            break;
        }
    }

    // Release all sessions claimed by this connection.
    if !claimed_sessions.is_empty() {
        let mut s = state.lock().await;
        for id in &claimed_sessions {
            s.open_sessions.remove(id);
        }
        s.broadcast(InstanceEvent::ThreadsChanged);
    }
}

/// Execute one request and produce a response.
async fn handle_request(
    req: Request,
    state: Arc<Mutex<DaemonState>>,
    claimed: &mut Vec<String>,
) -> Response {
    let version = env!("CARGO_PKG_VERSION");
    // WO 43.6: permission tier for this op. Computed once before the match
    // consumes `req`; checked after `check_auth` succeeds in each arm.
    let perm = DaemonState::required_permission(&req);
    match req {
        Request::Ping {
            version: client_version,
            auth_token,
        } => {
            let s = state.lock().await;
            if let Err(e) = s.check_auth(auth_token.as_deref()) {
                return e;
            }
            if !kf_rbac::has_permission(&s.actor_for_token(auth_token.as_deref()), perm) {
                return Response::error(format!("forbidden: requires {}", perm.as_str()));
            }
            if let Some(ref cv) = client_version {
                if cv != version {
                    return Response::error(format!(
                        "version mismatch: client {cv}, daemon {version} — restart both halves"
                    ));
                }
            }
            Response::ok_json(serde_json::json!({ "version": version }))
        }

        Request::List { auth_token } => {
            let mut s = state.lock().await;
            if let Err(e) = s.check_auth(auth_token.as_deref()) {
                return e;
            }
            if !kf_rbac::has_permission(&s.actor_for_token(auth_token.as_deref()), perm) {
                return Response::error(format!("forbidden: requires {}", perm.as_str()));
            }
            s.refresh();
            let sessions: Vec<_> = s.recent.iter().cloned().collect();
            let arr: Vec<serde_json::Value> = sessions
                .into_iter()
                .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
                .collect();
            Response::ok_json(serde_json::json!({ "sessions": arr }))
        }

        Request::Resolve { id, auth_token } => {
            let s = state.lock().await;
            if let Err(e) = s.check_auth(auth_token.as_deref()) {
                return e;
            }
            if !kf_rbac::has_permission(&s.actor_for_token(auth_token.as_deref()), perm) {
                return Response::error(format!("forbidden: requires {}", perm.as_str()));
            }
            match s.resolve(&id) {
                Some(entry) => Response::ok_json(serde_json::json!({
                    "id": entry.id,
                    "path": entry.path.to_string_lossy().to_string(),
                })),
                None => Response::error(format!("session '{id}' not found")),
            }
        }

        Request::Touch {
            id,
            path,
            auth_token,
        } => {
            let mut s = state.lock().await;
            if let Err(e) = s.check_auth(auth_token.as_deref()) {
                return e;
            }
            if !kf_rbac::has_permission(&s.actor_for_token(auth_token.as_deref()), perm) {
                return Response::error(format!("forbidden: requires {}", perm.as_str()));
            }
            s.touch(&id, path.path);
            s.broadcast(InstanceEvent::ThreadsChanged);
            Response::ok_empty()
        }

        Request::Shutdown { auth_token } => {
            let s = state.lock().await;
            if let Err(e) = s.check_auth(auth_token.as_deref()) {
                return e;
            }
            if !kf_rbac::has_permission(&s.actor_for_token(auth_token.as_deref()), perm) {
                return Response::error(format!("forbidden: requires {}", perm.as_str()));
            }
            tracing::info!("daemon received shutdown request");
            s.broadcast(InstanceEvent::Quit);
            Response::ok_empty()
        }

        Request::Claim { id, auth_token } => {
            let mut s = state.lock().await;
            if let Err(e) = s.check_auth(auth_token.as_deref()) {
                return e;
            }
            if !kf_rbac::has_permission(&s.actor_for_token(auth_token.as_deref()), perm) {
                return Response::error(format!("forbidden: requires {}", perm.as_str()));
            }
            if s.open_sessions.contains(&id) {
                return Response::Busy {
                    message: format!("session '{id}' is already claimed by another connection"),
                };
            }
            s.open_sessions.insert(id.clone());
            claimed.push(id.clone());
            Response::ok_json(serde_json::json!({ "claimed": id }))
        }

        Request::QuitAll { auth_token } => {
            let s = state.lock().await;
            if let Err(e) = s.check_auth(auth_token.as_deref()) {
                return e;
            }
            if !kf_rbac::has_permission(&s.actor_for_token(auth_token.as_deref()), perm) {
                return Response::error(format!("forbidden: requires {}", perm.as_str()));
            }
            tracing::info!("daemon received quit_all request");
            s.broadcast(InstanceEvent::Quit);
            Response::ok_empty()
        }

        Request::NotifyJobsChanged { auth_token } => {
            let s = state.lock().await;
            if let Err(e) = s.check_auth(auth_token.as_deref()) {
                return e;
            }
            if !kf_rbac::has_permission(&s.actor_for_token(auth_token.as_deref()), perm) {
                return Response::error(format!("forbidden: requires {}", perm.as_str()));
            }
            s.broadcast(InstanceEvent::JobsChanged);
            Response::ok_empty()
        }

        // InstanceRegister is consumed in handle_client before reaching
        // handle_request (it opens a long-lived push channel, not a
        // request/response pair). This arm is unreachable but required by
        // the exhaustive match.
        Request::InstanceRegister { .. } => Response::error(
            "instance_register should be handled by the connection loop".to_string(),
        ),
    }
}

/// Handle an `InstanceRegister` connection.
///
/// Sends an `ok` handshake response, then spawns a single writer task
/// that drains an `UnboundedReceiver<InstanceEvent>` and writes NDJSON
/// frames to the stream. The sender is registered in `DaemonState` so
/// `broadcast` can push events. When the writer task finishes (stream
/// closed or error), the sender is automatically pruned on the next
/// broadcast because `try_send` will fail on a disconnected receiver.
async fn handle_instance_register(
    mut stream: BufStream<UnixStream>,
    state: Arc<Mutex<DaemonState>>,
) {
    // Send handshake ack.
    let ack = Response::ok_json(serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }));
    if write_response(&mut stream, ack).await.is_err() {
        tracing::warn!("failed to write instance_register ack");
        return;
    }

    let (tx, rx) =
        tokio::sync::mpsc::channel::<InstanceEvent>(crate::daemon::INSTANCE_CHANNEL_CAPACITY);

    // Register the sender in the instance registry.
    {
        let s = state.lock().await;
        let mut instances = s.instances.lock().unwrap();
        instances.push(tx);
    }

    // Notify the newly-registered instance of the current session state
    // so it can do an initial refresh without polling.
    {
        let s = state.lock().await;
        s.broadcast(InstanceEvent::ThreadsChanged);
    }

    // Writer loop: drain the channel and write NDJSON frames.
    // This is the single writer per connection — no concurrent
    // writes to the stream, so NDJSON frames never interleave.
    let mut rx = rx;
    while let Some(ev) = rx.recv().await {
        let line = match serde_json::to_string(&ev) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialise InstanceEvent");
                continue;
            }
        };
        if stream.write_all(line.as_bytes()).await.is_err()
            || stream.write_all(b"\n").await.is_err()
            || stream.flush().await.is_err()
        {
            break; // stream closed
        }
    }

    // Prune the sender from the registry. Since the receiver is
    // dropped (rx moved into write_events and now finished), the
    // next broadcast call will prune it via try_send failure, but
    // we remove it eagerly here.
    {
        let s = state.lock().await;
        let mut instances = s.instances.lock().unwrap();
        instances.retain(|sender| !sender.is_closed());
    }
}
async fn write_response(stream: &mut BufStream<UnixStream>, resp: Response) -> anyhow::Result<()> {
    crate::daemon::write_ndjson_response(stream, &resp).await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::daemon::client::DaemonClient;
    use crate::shared::test_util::EnvGuard;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn read_line_limited_reads_normal_line() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let mut reader = BufStream::new(server);
        let mut line = String::new();

        let write_handle = tokio::spawn(async move {
            client.write_all(b"{\"status\":\"ok\"}\n").await.unwrap();
            client.shutdown().await.unwrap();
        });

        let n = crate::daemon::read_line_limited(&mut reader, &mut line)
            .await
            .expect("normal line should read successfully");
        assert_eq!(n, "{\"status\":\"ok\"}\n".len());
        assert_eq!(line.trim(), "{\"status\":\"ok\"}");

        write_handle.await.unwrap();
    }

    #[tokio::test]
    async fn read_line_limited_rejects_oversized_frame() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let mut reader = BufStream::new(server);
        let mut line = String::new();

        let oversized = vec![b'x'; crate::daemon::MAX_FRAME_SIZE + 1];
        let write_handle = tokio::spawn(async move {
            client.write_all(&oversized).await.unwrap();
            client.shutdown().await.unwrap();
        });

        let result = crate::daemon::read_line_limited(&mut reader, &mut line).await;
        assert!(
            result.is_err(),
            "expected oversized frame to be rejected, got {result:?}"
        );

        write_handle.await.unwrap();
    }

    #[tokio::test]
    async fn client_server_round_trip() {
        let _guard = crate::session::test_data_dir_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set("KF_CODE_DATA_DIR", dir.path());

        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let touch_path = sessions_dir.join("test-session.conv.ndjson");
        std::fs::write(&touch_path, "").unwrap();

        let socket = dir.path().join("daemon.sock");
        let pid = dir.path().join("daemon.pid");

        let server_handle = tokio::spawn(run_daemon_at(socket.clone(), pid.clone()));

        // Wait for the daemon to bind its socket. 1s budget, 5ms interval.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if socket.exists() {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
        assert!(socket.exists(), "daemon did not bind socket in time");

        let mut client = DaemonClient::connect_at(socket.clone()).await.unwrap();
        client.ping().await.unwrap();

        client
            .touch("test-session", touch_path.clone())
            .await
            .unwrap();

        let list = client.list_recent().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "test-session");
        assert_eq!(list[0].path, touch_path);

        let resolved = client.resolve("test-session").await.unwrap();
        assert_eq!(resolved, Some(touch_path.clone()));

        let resolved_prefix = client.resolve("test").await.unwrap();
        assert_eq!(resolved_prefix, Some(touch_path));

        let unknown = client.resolve("nope").await;
        assert!(unknown.is_err());

        client.shutdown().await.unwrap();

        server_handle.await.unwrap().unwrap();

        assert!(!socket.exists(), "daemon left stale socket");
        assert!(!pid.exists(), "daemon left stale pid file");
    }

    #[tokio::test]
    async fn pid_file_not_written_when_bind_fails() {
        // Regression for C24: the PID file used to be written before the
        // Unix socket was bound, leaving a stale PID if bind failed.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let pid = dir.path().join("daemon.pid");

        // Make the directory read-only so bind cannot create the socket file.
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        let original_readonly = perms.readonly();
        perms.set_readonly(true);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        let result = run_daemon_at(socket, pid.clone()).await;

        // Restore write permissions so the temp dir can be cleaned up.
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_readonly(original_readonly);
        std::fs::set_permissions(dir.path(), perms).unwrap();

        // If we could not actually prevent bind (e.g. running as root), the
        // test is inconclusive for this environment.
        if result.is_ok() {
            return;
        }

        assert!(
            !pid.exists(),
            "PID file must not be written when socket bind fails"
        );
    }

    /// Register two fake instances, broadcast JobsChanged, assert both receive.
    /// Drop one receiver, broadcast again, assert only the live one receives
    /// and the dead sender was pruned.
    #[tokio::test]
    async fn instance_broadcast_reaches_all_and_prunes_dead() {
        use crate::daemon::InstanceEvent;

        let state = DaemonState::new();

        // Two channels simulating two registered instances.
        let (tx1, mut rx1) =
            tokio::sync::mpsc::channel::<InstanceEvent>(crate::daemon::INSTANCE_CHANNEL_CAPACITY);
        let (tx2, mut rx2) =
            tokio::sync::mpsc::channel::<InstanceEvent>(crate::daemon::INSTANCE_CHANNEL_CAPACITY);

        {
            let mut instances = state.instances.lock().unwrap();
            instances.push(tx1);
            instances.push(tx2);
        }

        // Broadcast JobsChanged — both should receive.
        state.broadcast(InstanceEvent::JobsChanged);

        let ev1 = rx1
            .try_recv()
            .expect("instance 1 should receive JobsChanged");
        assert!(matches!(ev1, InstanceEvent::JobsChanged));

        let ev2 = rx2
            .try_recv()
            .expect("instance 2 should receive JobsChanged");
        assert!(matches!(ev2, InstanceEvent::JobsChanged));

        // Drop instance 2's receiver to simulate a disconnect.
        drop(rx2);

        // Broadcast again — the dead sender should be pruned.
        state.broadcast(InstanceEvent::ThreadsChanged);

        let ev1 = rx1.try_recv().expect("instance 1 should still receive");
        assert!(matches!(ev1, InstanceEvent::ThreadsChanged));

        // The dead sender was pruned from the registry.
        let instances = state.instances.lock().unwrap();
        assert_eq!(instances.len(), 1, "dead sender should be pruned");
    }

    /// Verify that rapid broadcasts produce well-formed NDJSON lines
    /// (no interleaving) on a single instance channel.
    #[tokio::test]
    async fn instance_broadcast_rapid_no_interleaving() {
        use crate::daemon::InstanceEvent;

        let state = DaemonState::new();
        let (tx, mut rx) =
            tokio::sync::mpsc::channel::<InstanceEvent>(crate::daemon::INSTANCE_CHANNEL_CAPACITY);

        {
            let mut instances = state.instances.lock().unwrap();
            instances.push(tx);
        }

        // Fire many rapid broadcasts.
        for _ in 0..100 {
            state.broadcast(InstanceEvent::JobsChanged);
            state.broadcast(InstanceEvent::ThreadsChanged);
        }

        // All 200 events should arrive in order, with no drops.
        let mut count = 0usize;
        while let Ok(ev) = rx.try_recv() {
            match count % 2 {
                0 => assert!(matches!(ev, InstanceEvent::JobsChanged)),
                1 => assert!(matches!(ev, InstanceEvent::ThreadsChanged)),
                _ => unreachable!(),
            }
            count += 1;
        }
        assert_eq!(count, 200, "all events should be delivered in order");
    }

    /// Instance register handshake over a real Unix stream produces the
    /// ack and then receives pushed events.
    #[tokio::test]
    async fn instance_register_receives_pushed_events() {
        use crate::daemon::{read_line_limited, InstanceEvent};

        let (client, server) = UnixStream::pair().unwrap();
        let state = Arc::new(Mutex::new(DaemonState::new()));

        // Server side: handle the instance register request.
        let state_clone = state.clone();
        let server_handle = tokio::spawn(async move {
            handle_instance_register(BufStream::new(server), state_clone).await;
        });

        // Client side: send the registration request.
        let mut client_buf = BufStream::new(client);
        let req = serde_json::to_string(&Request::InstanceRegister {
            version: None,
            auth_token: None,
        })
        .unwrap();
        client_buf.write_all(req.as_bytes()).await.unwrap();
        client_buf.write_all(b"\n").await.unwrap();
        client_buf.flush().await.unwrap();

        // Read the handshake ack.
        let mut line = String::new();
        let n = read_line_limited(&mut client_buf, &mut line)
            .await
            .expect("should read ack");
        assert!(n > 0, "should receive an ack response");
        let ack: Response = serde_json::from_str(line.trim()).expect("ack should parse");
        assert!(
            matches!(ack, Response::Ok { .. }),
            "expected ok ack, got {ack:?}"
        );

        // The daemon should have pushed a ThreadsChanged event
        // during registration. Read it.
        let mut event_line = String::new();
        let n = read_line_limited(&mut client_buf, &mut event_line)
            .await
            .expect("should read event");
        assert!(n > 0, "should receive an event after registration");
        let ev: InstanceEvent =
            serde_json::from_str(event_line.trim()).expect("event should parse");
        assert!(matches!(ev, InstanceEvent::ThreadsChanged));

        // Now broadcast a JobsChanged from the daemon side.
        {
            let s = state.lock().await;
            s.broadcast(InstanceEvent::JobsChanged);
        }

        // The client should receive it.
        let mut event_line2 = String::new();
        let n = read_line_limited(&mut client_buf, &mut event_line2)
            .await
            .expect("should read JobsChanged");
        assert!(n > 0);
        let ev2: InstanceEvent =
            serde_json::from_str(event_line2.trim()).expect("event should parse");
        assert!(matches!(ev2, InstanceEvent::JobsChanged));

        // Drop the client so the server's next write fails on the closed
        // stream. The writer loop parks on rx.recv() between events, so
        // trigger one more broadcast to unblock it — handle_instance_register
        // prunes the sender on the next broadcast after a disconnect
        // (lazy-prune design, documented at the function).
        drop(client_buf);
        {
            let s = state.lock().await;
            s.broadcast(InstanceEvent::JobsChanged);
        }
        let _ = server_handle.await;
    }

    // WO 19.5: daemon auth token enforcement.
    // Shutdown without an auth token must be rejected when auth is configured.
    #[tokio::test]
    async fn shutdown_requires_auth_token_when_configured() {
        use crate::daemon::DaemonState;

        // Configure a daemon state that expects an auth token.
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("token");
        std::fs::write(&token_path, "secret-token-value").unwrap();
        let _env = EnvGuard::set(
            "KF_CODE_DAEMON_TOKEN_FILE",
            token_path.to_string_lossy().as_ref(),
        );

        let state = DaemonState::new();
        // Verify auth is required.
        assert!(
            state.check_auth(None).is_err(),
            "auth should be required when token file exists"
        );
        assert!(
            state.check_auth(Some("wrong-token")).is_err(),
            "wrong token should be rejected"
        );
        assert!(
            state.check_auth(Some("secret-token-value")).is_ok(),
            "correct token should be accepted"
        );
    }

    // WO 19.5: daemon allows all ops when no auth token is configured.
    #[tokio::test]
    async fn daemon_allows_all_when_no_auth_configured() {
        use crate::daemon::DaemonState;

        let _env = EnvGuard::remove("KF_CODE_DAEMON_TOKEN_FILE");
        let state = DaemonState::new();
        assert!(
            state.check_auth(None).is_ok(),
            "no auth should be required when no token configured"
        );
    }

    // WO 43.6: RBAC permission tiers. A viewer-role token can List
    // (ViewerResults) but cannot Shutdown (requires OperatorRestart).
    #[tokio::test]
    async fn viewer_role_can_list_but_not_shutdown() {
        let _guard = crate::session::test_data_dir_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();

        let token_path = dir.path().join("token");
        std::fs::write(&token_path, "viewer-secret").unwrap();
        let _env_token = EnvGuard::set(
            "KF_CODE_DAEMON_TOKEN_FILE",
            token_path.to_string_lossy().as_ref(),
        );
        let _env_role = EnvGuard::set("KF_CODE_DAEMON_ROLE", "viewer");

        let socket = dir.path().join("rbac.sock");
        let pid = dir.path().join("rbac.pid");
        let server_handle = tokio::spawn(run_daemon_at(socket.clone(), pid.clone()));
        wait_for_socket(&socket).await;

        // connect_at does a version-gated ping (ViewerStatus) — viewer has it.
        let mut client = DaemonClient::connect_at(socket.clone()).await.unwrap();

        // List (ViewerResults) — viewer has it. Should succeed.
        let list_result = client.list_recent().await;
        assert!(
            list_result.is_ok(),
            "viewer should be able to List, got: {list_result:?}"
        );

        // Shutdown (OperatorRestart) — viewer lacks it. Should fail with
        // "forbidden" in the error message.
        let shutdown_result = client.shutdown().await;
        assert!(
            shutdown_result.is_err(),
            "viewer should NOT be able to Shutdown, got Ok"
        );
        let msg = match shutdown_result {
            Err(e) => format!("{e}"),
            Ok(_) => unreachable!(),
        };
        assert!(
            msg.contains("forbidden"),
            "shutdown denial should mention forbidden, got: {msg}"
        );

        drop(client);
        server_handle.abort();
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(&pid);
    }

    // WO 43.6: no-token behavior unchanged. With no auth configured, the
    // actor defaults to admin (KF_CODE_DAEMON_ROLE unset), so both List
    // and Shutdown succeed — today's all-access behavior.
    #[tokio::test]
    async fn no_token_admin_fallback_allows_all() {
        let _guard = crate::session::test_data_dir_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();

        let _env_no_token = EnvGuard::remove("KF_CODE_DAEMON_TOKEN_FILE");
        let _env_no_role = EnvGuard::remove("KF_CODE_DAEMON_ROLE");

        let socket = dir.path().join("nofallback.sock");
        let pid = dir.path().join("nofallback.pid");
        let server_handle = tokio::spawn(run_daemon_at(socket.clone(), pid.clone()));
        wait_for_socket(&socket).await;

        let mut client = DaemonClient::connect_at(socket.clone()).await.unwrap();

        // Both List and Shutdown should succeed — admin fallback.
        assert!(
            client.list_recent().await.is_ok(),
            "admin fallback should allow List"
        );
        assert!(
            client.shutdown().await.is_ok(),
            "admin fallback should allow Shutdown"
        );

        server_handle.await.unwrap().unwrap();
        assert!(!socket.exists(), "daemon left stale socket");
        assert!(!pid.exists(), "daemon left stale pid file");
    }

    // Helper: poll for the daemon socket to appear (1s budget).
    async fn wait_for_socket(socket: &std::path::Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if socket.exists() {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
        panic!("daemon did not bind socket in time at {}", socket.display());
    }
}
