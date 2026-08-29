#[cfg(feature = "pty")]
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

#[cfg(feature = "pty")]
pub struct PtyResult {
    pub stdout: String,
    pub exit_code: Option<i32>,
}

#[cfg(feature = "pty")]
pub fn run_with_pty(
    command: &str,
    workdir: &std::path::Path,
    cols: u16,
    rows: u16,
    event_tx: Option<tokio::sync::mpsc::Sender<crate::session::executor::TurnEvent>>,
    call_id: &str,
    kill_rx: std::sync::mpsc::Receiver<()>,
) -> anyhow::Result<PtyResult> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("sh");
    cmd.arg("-c");
    cmd.arg(command);
    cmd.cwd(workdir);
    // WO 43.28: scrub credential-shaped env vars on the PTY path too.
    // `portable_pty::CommandBuilder` inherits the parent env by default;
    // without this the interactive path leaks the same secrets the
    // foreground/background paths now scrub. PTY is feature-gated off by
    // default, but a default-off gap is still a gap.
    for (name, _) in std::env::vars() {
        if crate::session::bash_runner::is_secret_env_name(&name) {
            cmd.env_remove(name);
        }
    }

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    // WO 48.42: split a kill handle out (vendor `clone_killer`) so the
    // async caller can end this run without touching the child this
    // closure still owns and waits. The mpsc channel latches a request
    // that raced ahead of the spawn — it sits buffered until this
    // watcher starts, so a pre-cancelled call still kills its child.
    // ponytail: kill() is SIGHUP to the direct child only — a
    // HUP-ignoring grandchild survives; killpg upgrade if that ever
    // matters (needs raw libc, a new dep).
    let mut killer = child.clone_killer();
    std::thread::spawn(move || {
        if kill_rx.recv().is_ok() {
            let _ = killer.kill();
        }
    });

    let mut reader = pair.master.try_clone_reader()?;
    drop(pair.master);

    let mut stdout_buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        // A killed child surfaces here as EIO/EOF; either way break and
        // reach the wait() below — an early `?` return would skip the
        // reap and leak a zombie (WO 48.42).
        let n = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        stdout_buf.extend_from_slice(&chunk[..n]);
        if let Some(tx) = &event_tx {
            let text = String::from_utf8_lossy(&chunk[..n]).to_string();
            // Best-effort: a dropped receiver (TUI closed) must not fail
            // the command. call_id routes the chunk to this call's card
            // (WO 48.31) — parallel bash streams stay separate.
            let _ = tx.try_send(crate::session::executor::TurnEvent::BashPartialOutput {
                call_id: call_id.to_string(),
                text,
            });
        }
    }

    let exit_status = child.wait()?;
    let exit_code = if exit_status.success() { Some(0) } else { None };

    Ok(PtyResult {
        stdout: String::from_utf8_lossy(&stdout_buf).to_string(),
        exit_code,
    })
}
