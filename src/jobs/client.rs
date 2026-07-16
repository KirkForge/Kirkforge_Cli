//! Client for the scheduled-job daemon socket.
//!
//! Used by `kirkforge jobd --stop` and future TUI reload commands.

use crate::daemon::{read_line_limited, Request, Response};
use anyhow::{Context, Result};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

/// Ask the daemon to shut down gracefully.
pub async fn send_shutdown(socket_path: &Path) -> Result<()> {
    send_command(socket_path, Request::Shutdown).await?;
    Ok(())
}

/// Ask the daemon to reload jobs from disk.
pub async fn send_reload(socket_path: &Path) -> Result<()> {
    send_command(socket_path, Request::List).await?;
    Ok(())
}

/// Send a control command and wait for a one-line response.
async fn send_command(socket_path: &Path, request: Request) -> Result<Response> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to jobd socket at {}", socket_path.display()))?;
    let mut stream = tokio::io::BufStream::new(stream);

    let line = serde_json::to_string(&request).context("serialise jobd request")?;
    stream.write_all(line.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;

    let mut buf = String::new();
    let n = read_line_limited(&mut stream, &mut buf).await?;
    if n == 0 {
        anyhow::bail!("jobd closed connection without response");
    }
    let resp: Response =
        serde_json::from_str(&buf).with_context(|| format!("parsing jobd response: {buf}"))?;
    Ok(resp)
}
