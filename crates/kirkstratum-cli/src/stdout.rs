use anyhow::Context;

/// Write `s` to stdout, ignoring `BrokenPipe` so shell pipelines like
/// `stratum run | head` exit cleanly instead of panicking.
#[must_use = "ignoring a stdout write error can hide pipeline failures"]
pub fn write_stdout(s: &str) -> anyhow::Result<()> {
    use std::io::{self, Write};
    let mut stdout = io::stdout().lock();
    match stdout.write_all(s.as_bytes()) {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other.with_context(|| "failed to write to stdout"),
    }
}
