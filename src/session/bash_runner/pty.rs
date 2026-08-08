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

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    drop(pair.master);

    let mut stdout_buf = Vec::new();
    std::io::copy(&mut reader, &mut stdout_buf)?;

    let exit_status = child.wait()?;
    let exit_code = if exit_status.success() { Some(0) } else { None };

    Ok(PtyResult {
        stdout: String::from_utf8_lossy(&stdout_buf).to_string(),
        exit_code,
    })
}
