//! Client for the scheduled-job daemon socket.
//!
//! Used by `kf-code jobd --stop` and future TUI reload commands.

use crate::daemon::{read_auth_token, read_line_limited, Request, Response};
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

/// How long the client waits for one jobd response line. Matches the
/// session-daemon client (`READ_TIMEOUT` in `daemon/client.rs`): the server
/// caps its own handler at 30s, so a hung jobd surfaces to the caller instead
/// of hanging `--stop` forever.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

// ponytail: test-only override hook, mirroring `daemon/client.rs`. Production
// callers never set this env var — when unset, READ_TIMEOUT is used. Exists
// so the timeout test can pin the timeout firing in tens of milliseconds
// rather than waiting the full 30s (which would blow the test-fast.sh budget).
fn read_timeout() -> Duration {
    std::env::var("KF_TEST_JOBD_READ_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(READ_TIMEOUT)
}

/// Ask the daemon to shut down gracefully. Sends the auth token from
/// `KF_CODE_DAEMON_TOKEN_FILE` (same file the session daemon uses) so an
/// auth-enabled jobd accepts the request. Returns `Ok` only when the daemon
/// acknowledged with `Response::Ok`; an `Error`/`Busy` response surfaces as
/// `Err` so callers (notably `stop_job_daemon`) do not delete the pid file
/// for a daemon that is still running.
pub async fn send_shutdown(socket_path: &Path) -> Result<()> {
    let request = Request::Shutdown {
        auth_token: read_auth_token(),
    };
    match send_command(socket_path, request).await? {
        Response::Ok { .. } => Ok(()),
        Response::Error { message } => anyhow::bail!("jobd shutdown rejected: {message}"),
        Response::Busy { message } => anyhow::bail!("jobd shutdown rejected: {message}"),
    }
}

/// Send a control command and wait for a one-line response, bounded by
/// `READ_TIMEOUT` so a hung daemon cannot block the caller forever.
async fn send_command(socket_path: &Path, request: Request) -> Result<Response> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to jobd socket at {}", socket_path.display()))?;
    let mut stream = tokio::io::BufStream::new(stream);

    let line = serde_json::to_string(&request).context("serialise jobd request")?;
    stream.write_all(line.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;

    let timeout = read_timeout();
    let mut buf = String::new();
    let n = tokio::time::timeout(timeout, read_line_limited(&mut stream, &mut buf))
        .await
        .with_context(|| format!("jobd response timed out after {timeout:?}"))?
        .context("read jobd response")?;
    if n == 0 {
        anyhow::bail!("jobd closed connection without response");
    }
    let trimmed = buf.trim();
    let resp: Response = serde_json::from_str(trimmed)
        .with_context(|| format!("parsing jobd response: {trimmed}"))?;
    Ok(resp)
}

#[cfg(all(test, unix))]
mod tests {
    use super::send_shutdown;
    use crate::daemon::{read_line_limited, Request, Response};
    use crate::shared::test_util::EnvGuard;
    use anyhow::Context;
    use tokio::io::{AsyncWriteExt, BufStream};
    use tokio::net::UnixListener;

    /// A stub jobd that accepts one connection and runs a user handler with
    /// the stream. The handler reads the request line and writes a response.
    async fn stub_jobd<F, Fut>(socket: std::path::PathBuf, handler: F)
    where
        F: FnOnce(BufStream<tokio::net::UnixStream>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handler(BufStream::new(stream)).await;
        });
    }

    /// Drain one request line so the handler sees a clean read position.
    async fn drain_request(stream: &mut BufStream<tokio::net::UnixStream>) {
        let mut line = String::new();
        let _ = read_line_limited(stream, &mut line).await;
    }

    /// R1: an auth-enabled jobd rejects a shutdown lacking the right token.
    /// The stub responds with `Response::error("authentication required")` so
    /// `send_shutdown` must surface `Err` (not Ok) and the caller must not
    /// delete the pid. The stub rejects regardless of what the client sends,
    /// so this test does not touch `KF_CODE_DAEMON_TOKEN_FILE` — mutating that
    /// process-global env races with the parallel session-daemon auth test.
    #[tokio::test]
    async fn send_shutdown_rejected_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("reject.sock");
        let pid = dir.path().join("reject.pid");
        std::fs::write(&pid, "99999\n").unwrap();

        stub_jobd(socket.clone(), |mut stream| async move {
            drain_request(&mut stream).await;
            let resp = Response::error("authentication required");
            let out = serde_json::to_string(&resp).unwrap();
            stream.write_all(out.as_bytes()).await.unwrap();
            stream.write_all(b"\n").await.unwrap();
            stream.flush().await.unwrap();
        })
        .await;

        let result = send_shutdown(&socket).await;
        assert!(result.is_err(), "rejected shutdown should be Err, got Ok");
        let msg = match result {
            Err(e) => format!("{e}"),
            Ok(_) => unreachable!(),
        };
        assert!(
            msg.contains("authentication required"),
            "error should surface the daemon message, got: {msg}"
        );
        // Caller (stop_job_daemon) must keep the pid file on Err.
        assert!(pid.exists(), "pid file must remain after a rejected stop");
    }

    /// R2: a jobd that accepts but never responds makes `send_shutdown` time
    /// out (not hang). The test shrinks the timeout via the env override so
    /// the inner `tokio::time::timeout` fires in tens of ms. The handler must
    /// keep the accepted stream alive — a bare `_stream` bind would let the
    /// async block drop it on entry, closing the connection before the
    /// client's read blocks (surfacing as an EOF, not a timeout).
    #[tokio::test]
    async fn send_shutdown_times_out_when_daemon_silent() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("silent.sock");
        let _env = EnvGuard::set("KF_TEST_JOBD_READ_TIMEOUT_MS", "100");

        stub_jobd(socket.clone(), |stream| async move {
            // Hold the stream open forever; never read or write. Binding it
            // here keeps it alive across the pending await — without this the
            // stream is dropped immediately and the client sees EOF, not a
            // timeout.
            let _hold = stream;
            std::future::pending::<()>().await;
        })
        .await;

        // Outer bound: a regression (timeout wrap removed) hangs the test
        // instead of failing it.
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), send_shutdown(&socket)).await;
        let inner = match result {
            Err(_) => panic!("send_shutdown did not return within 2s — timeout wrap missing?"),
            Ok(r) => r,
        };
        assert!(inner.is_err(), "silent daemon should time out, got Ok");
        let msg = match inner {
            Err(e) => format!("{e}"),
            Ok(_) => unreachable!(),
        };
        assert!(
            msg.contains("timed out"),
            "error should name the timeout, got: {msg}"
        );
    }

    /// R3 (happy path): an auth-less jobd that acknowledges with `Response::Ok`
    /// makes `send_shutdown` return Ok, and the pid file is removed only then.
    /// The stub acks regardless of the request, so (like R1) it does not touch
    /// the process-global `KF_CODE_DAEMON_TOKEN_FILE`.
    #[tokio::test]
    async fn send_shutdown_ok_when_daemon_acks() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ok.sock");
        let pid = dir.path().join("ok.pid");
        std::fs::write(&pid, "12345\n").unwrap();

        stub_jobd(socket.clone(), |mut stream| async move {
            drain_request(&mut stream).await;
            let out = serde_json::to_string(&Response::ok_empty()).unwrap();
            stream.write_all(out.as_bytes()).await.unwrap();
            stream.write_all(b"\n").await.unwrap();
            stream.flush().await.unwrap();
        })
        .await;

        send_shutdown(&socket)
            .await
            .expect("acked shutdown should be Ok");
    }

    /// R4: `send_shutdown` actually sends the configured auth token. A stub
    /// daemon echoes back Ok only if the request's `auth_token` matches the
    /// token file; otherwise it errors. This pins the "token is read and sent"
    /// contract that R1 leaves implicit.
    ///
    /// Holds the crate-wide `test_data_dir_lock` because setting
    /// `KF_CODE_DAEMON_TOKEN_FILE` is process-global and races with the
    /// session-daemon auth test, which sets the same var to a different path.
    #[tokio::test]
    async fn send_shutdown_sends_configured_auth_token() {
        let _guard = crate::session::test_data_dir_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("auth.sock");
        let token_path = dir.path().join("token");
        std::fs::write(&token_path, "shared-secret\n").unwrap();
        let _env = EnvGuard::set(
            "KF_CODE_DAEMON_TOKEN_FILE",
            token_path.to_string_lossy().as_ref(),
        );

        stub_jobd(socket.clone(), |mut stream| async move {
            let mut line = String::new();
            read_line_limited(&mut stream, &mut line).await.unwrap();
            let req: Request = serde_json::from_str(line.trim()).unwrap();
            let resp = match req {
                Request::Shutdown { auth_token } => match auth_token.as_deref() {
                    Some("shared-secret") => Response::ok_empty(),
                    other => Response::error(format!("token mismatch: {other:?}")),
                },
                _ => Response::error("unexpected request"),
            };
            let out = serde_json::to_string(&resp).unwrap();
            stream.write_all(out.as_bytes()).await.unwrap();
            stream.write_all(b"\n").await.unwrap();
            stream.flush().await.unwrap();
        })
        .await;

        send_shutdown(&socket)
            .await
            .context("token-bearing shutdown should be acked")
            .unwrap();
    }
}
