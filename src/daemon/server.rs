//! Daemon server — listens on a Unix domain socket and serves JSON-RPC.

use crate::daemon::paths;
use crate::daemon::{DaemonState, Request, Response};
use anyhow::Context;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufStream};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

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
        daemonize()?;
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

    // Remove stale socket from a previous crash.
    if let Err(e) = std::fs::remove_file(&socket_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, path = %socket_path.display(), "Failed to remove stale daemon socket");
        }
    }

    // Write PID file.
    let pid = std::process::id();
    if let Err(e) = std::fs::write(&pid_path, format!("{pid}\n")) {
        tracing::warn!(error = %e, path = %pid_path.display(), "Failed to write daemon PID file");
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind daemon socket at {}", socket_path.display()))?;

    let state = Arc::new(Mutex::new(DaemonState::new()));
    let shutdown = Arc::new(tokio::sync::Notify::new());

    // Initial refresh.
    {
        let mut s = state.lock().await;
        s.refresh();
    }

    // Signal handlers: SIGINT, plus SIGHUP and SIGTERM on Unix.
    // All route to the same shutdown Notify so the daemon exits cleanly
    // whether the user presses Ctrl+C, their terminal session ends, or a
    // service manager sends SIGTERM.
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("daemon received SIGINT; shutting down");
            shutdown_clone.notify_one();
        }
    });

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let hup_shutdown = shutdown.clone();
        tokio::spawn(async move {
            match signal(SignalKind::hangup()) {
                Ok(mut hup) => {
                    if hup.recv().await.is_some() {
                        tracing::info!("daemon received SIGHUP; shutting down");
                        hup_shutdown.notify_one();
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not install SIGHUP handler");
                }
            }
        });

        let term_shutdown = shutdown.clone();
        tokio::spawn(async move {
            match signal(SignalKind::terminate()) {
                Ok(mut term) => {
                    if term.recv().await.is_some() {
                        tracing::info!("daemon received SIGTERM; shutting down");
                        term_shutdown.notify_one();
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not install SIGTERM handler");
                }
            }
        });
    }

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
                        tokio::spawn(handle_client(stream, state, shutdown));
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

/// Detach the process into the background (Unix-only).
#[cfg(unix)]
fn daemonize() -> anyhow::Result<()> {
    let current_exe = std::env::current_exe().context("get current exe")?;
    let mut cmd = std::process::Command::new(current_exe);
    cmd.arg("daemon").arg("--foreground");
    if let Ok(v) = std::env::var("KIRKFORGE_DATA_DIR") {
        cmd.env("KIRKFORGE_DATA_DIR", v);
    }
    let _ = cmd.spawn().context("spawn daemon foreground process")?;
    std::process::exit(0);
}

#[cfg(not(unix))]
fn daemonize() -> anyhow::Result<()> {
    anyhow::bail!("background daemon mode is only supported on Unix; use --foreground")
}

/// Serve a single client connection until it closes.
async fn handle_client(
    stream: UnixStream,
    state: Arc<Mutex<DaemonState>>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let mut stream = BufStream::new(stream);
    let mut line = String::new();

    loop {
        line.clear();
        match stream.read_line(&mut line).await {
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

        let is_shutdown = matches!(req, Request::Shutdown);
        let resp = handle_request(req, state.clone()).await;
        if write_response(&mut stream, resp).await.is_err() {
            break;
        }
        if is_shutdown {
            shutdown.notify_one();
            break;
        }
    }
}

/// Execute one request and produce a response.
async fn handle_request(req: Request, state: Arc<Mutex<DaemonState>>) -> Response {
    match req {
        Request::Ping => Response::ok_empty(),

        Request::List => {
            let mut s = state.lock().await;
            s.refresh();
            let sessions: Vec<_> = s.recent.iter().cloned().collect();
            let arr: Vec<serde_json::Value> = sessions
                .into_iter()
                .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
                .collect();
            Response::ok_json(serde_json::json!({ "sessions": arr }))
        }

        Request::Resolve { id } => {
            let s = state.lock().await;
            match s.resolve(&id) {
                Some(entry) => Response::ok_json(serde_json::json!({
                    "id": entry.id,
                    "path": entry.path.to_string_lossy().to_string(),
                })),
                None => Response::error(format!("session '{id}' not found")),
            }
        }

        Request::Touch { id, path } => {
            let mut s = state.lock().await;
            s.touch(&id, path.path);
            Response::ok_empty()
        }

        Request::Shutdown => {
            tracing::info!("daemon received shutdown request");
            Response::ok_empty()
        }
    }
}

/// Serialize a response and send it to the client.
async fn write_response(stream: &mut BufStream<UnixStream>, resp: Response) -> anyhow::Result<()> {
    let line = serde_json::to_string(&resp).context("serialize response")?;
    stream
        .write_all(line.as_bytes())
        .await
        .context("write response")?;
    stream.write_all(b"\n").await.context("write newline")?;
    stream.flush().await.context("flush response")?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::daemon::client::DaemonClient;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn client_server_round_trip() {
        let _guard = crate::session::test_data_dir_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var("KIRKFORGE_DATA_DIR").ok();
        std::env::set_var("KIRKFORGE_DATA_DIR", dir.path());

        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let touch_path = sessions_dir.join("test-session.conv.ndjson");
        std::fs::write(&touch_path, "").unwrap();

        let socket = dir.path().join("daemon.sock");
        let pid = dir.path().join("daemon.pid");

        let server_handle = tokio::spawn(run_daemon_at(socket.clone(), pid.clone()));

        // Wait for the daemon to bind its socket.
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            sleep(Duration::from_millis(20)).await;
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

        match previous {
            Some(v) => std::env::set_var("KIRKFORGE_DATA_DIR", v),
            None => std::env::remove_var("KIRKFORGE_DATA_DIR"),
        }
    }
}
