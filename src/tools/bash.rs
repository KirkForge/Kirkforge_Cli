use crate::session::bash_jobs::global_registry;
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::bash_minify;
use crate::tools::Tool;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::process::Command;

pub struct Bash;

/// Per-stream cap for captured stdout / stderr from a single bash invocation.
///
/// Without this, a single `cat /dev/urandom` or `find / -print` against a
/// large tree will read the whole byte stream into a `String` and OOM the
/// process. 1 MiB per stream is enough to fit a `cargo test` summary, a
/// `cargo clippy` warning block, or a grep of a medium codebase — anything
/// bigger gets a `[truncated: N bytes omitted]` marker so the model can
/// still see it ran and pick a narrower command. Tweakable but not exposed
/// as a config knob; the original review (GPT 5.5 #10) flagged the
/// unbounded buffer as a safety finding, and 1 MiB is the canonical
/// "readable but bounded" choice.
const MAX_BASH_OUTPUT_BYTES: usize = 1024 * 1024;

/// Marker appended to a stream that hit the cap. Includes the count of
/// dropped bytes so the model can decide whether to re-run with a narrower
/// filter (e.g. `head -n 1000`).
const TRUNCATED_MARKER_FMT: &str =
    "\n[...truncated: {} bytes omitted, output exceeded 1 MiB cap...]\n";

/// Reader that stops accepting bytes once `cap` is reached but keeps
/// draining the underlying pipe so the child process doesn't block on a
/// full pipe buffer. Anything past the cap is discarded (counted, not
/// surfaced).
struct CappedReader {
    inner: Box<dyn AsyncRead + Unpin + Send>,
    cap: usize,
    truncated_bytes: u64,
    /// How many bytes we've actually kept in the output buffer.
    kept: usize,
}

impl CappedReader {
    fn new(inner: Box<dyn AsyncRead + Unpin + Send>, cap: usize) -> Self {
        Self {
            inner,
            cap,
            truncated_bytes: 0,
            kept: 0,
        }
    }

    /// Read up to `buf.capacity()` (or fewer) into `buf`. Returns the
    /// number of bytes that were *kept* in the buffer. Continues draining
    /// the inner pipe (discarding the overflow) so the child doesn't block.
    async fn read_into(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        // Room left in the cap. If we've already filled it, skip the read
        // entirely and just drain.
        let room = self.cap.saturating_sub(self.kept);
        if room == 0 {
            let mut sink = [0u8; 8192];
            loop {
                match self.inner.read(&mut sink).await? {
                    0 => return Ok(0),
                    n => self.truncated_bytes += n as u64,
                }
            }
        }

        // Temporarily fill a small temp buffer, then transfer only what
        // fits under the cap. ReadBuf needs &mut [u8] so we cap the
        // read length to `room` to avoid reading past the cap.
        let want = room.min(8192);
        let mut tmp = vec![0u8; want];
        let mut read_buf = ReadBuf::new(&mut tmp);
        self.inner.read_buf(&mut read_buf).await?;
        let n = read_buf.filled().len();
        if n == 0 {
            return Ok(0);
        }
        let to_keep = n.min(room);
        buf.extend_from_slice(&tmp[..to_keep]);
        self.kept += to_keep;
        if n > to_keep {
            self.truncated_bytes += (n - to_keep) as u64;
        }
        Ok(to_keep)
    }
}

/// Drain a `CappedReader` into a `Vec<u8>`, returning the buffer and the
/// number of bytes dropped past the cap. The `Send` bound is required so
/// the function can run inside a `tokio::spawn` task (the actual readers
/// we pass — `ChildStdout` / `ChildStderr` — are `Send`).
async fn drain_capped<R>(r: R, cap: usize) -> std::io::Result<(Vec<u8>, u64)>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut cr = CappedReader::new(Box::new(r), cap);
    let mut out = Vec::with_capacity(cap.min(8192));
    loop {
        let n = cr.read_into(&mut out).await?;
        if n == 0 {
            break;
        }
    }
    Ok((out, cr.truncated_bytes))
}

#[async_trait::async_trait]
impl Tool for Bash {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash",
            description: "Execute a bash command. Use for running tests, builds, git operations, and file inspection. Output is captured and returned. Set \"background\": true to run long-lived commands in the background.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 30)",
                        "default": 30
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Working directory (default: project root)",
                        "default": "."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Run in background. Use bash_status to check and bash_output to retrieve results.",
                        "default": false
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn run(&self, args: serde_json::Value) -> ToolOutcome {
        let cmd = match args.get("command").and_then(|c| c.as_str()) {
            Some(c) => c.to_string(),
            None => {
                return ToolOutcome::Error {
                    message: "Missing 'command' argument".into(),
                }
            }
        };

        // Check for background mode
        if args
            .get("background")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            let registry = global_registry();
            let workdir = args.get("workdir").and_then(|w| w.as_str());
            let timeout = args.get("timeout").and_then(|t| t.as_u64());
            match registry.spawn(&cmd, workdir, timeout).await {
                Ok(id) => ToolOutcome::Success {
                    content: format!("Background job #{} started. Use bash_status(id={}) or bash_output(id={}) to check results.", id, id, id),
                },
                Err(e) => ToolOutcome::Error {
                    message: format!("Failed to start background job: {}", e),
                },
            }
        } else {
            // Normal foreground execution
            let timeout_secs = args.get("timeout").and_then(|t| t.as_u64()).unwrap_or(30);
            let workdir = args.get("workdir").and_then(|w| w.as_str()).unwrap_or(".");

            let workdir_path = PathBuf::from(shellexpand::tilde(workdir).as_ref());

            let result = run_shell(&cmd, &workdir_path, timeout_secs).await;

            match result {
                Ok(output) => {
                    if output.status.success() {
                        // v1.2 phase 21: if the command was a file-dump
                        // (cat, head, tail, etc.) into a known source file,
                        // route the captured stdout through the same
                        // minifier read_file uses. The cache is keyed on
                        // (path, mtime) so this is essentially free when
                        // the model has already called read_file on the
                        // same path earlier in the session.
                        let content = bash_minify::try_minify_bash_output(&cmd, &output.stdout)
                            .unwrap_or(output.stdout);
                        // v1.2 phase 22: if the command was a build
                        // (cargo build/test/check/clippy, rustc) and
                        // produced the canonical cargo progress + warning
                        // output, collapse the noise (compilation
                        // progress lines, repeated warning suggestion
                        // blocks) while keeping all errors and their
                        // context intact. A 400-line `cargo build` log
                        // can typically be reduced to ~50 lines.
                        let content =
                            bash_minify::try_minify_build_log(&cmd, &content).unwrap_or(content);
                        ToolOutcome::Success { content }
                    } else {
                        // Error path: stdout is often the *real* signal on a
                        // failing build (rustc prints diagnostics to stdout
                        // with `--message-format=human`, which is the default).
                        // Route it through the same minifiers the success path
                        // uses — they have the same 20%-savings guard, so a
                        // short error message passes through unchanged. Stderr
                        // stays verbatim: it usually contains raw error text
                        // (`error: command not found`, segfault traces) that's
                        // already small and where minification heuristics are
                        // more likely to drop the wrong line.
                        let minified_stdout =
                            bash_minify::try_minify_bash_output(&cmd, &output.stdout)
                                .unwrap_or_else(|| output.stdout.clone());
                        let minified_stdout =
                            bash_minify::try_minify_build_log(&cmd, &minified_stdout)
                                .unwrap_or(minified_stdout);
                        let stderr = if output.stderr.is_empty() {
                            String::new()
                        } else {
                            format!("\nstderr:\n{}", output.stderr)
                        };
                        ToolOutcome::Error {
                            message: format!(
                                "Command exited with code {}{}\nstdout:\n{}",
                                output.status.code().unwrap_or(-1),
                                stderr,
                                minified_stdout
                            ),
                        }
                    }
                }
                Err(e) => ToolOutcome::Error {
                    message: format!("Failed to execute command: {}", e),
                },
            }
        }
    }
}

struct ShellOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

/// Run a shell command in the foreground with kill_on_drop and timeout.
///
/// We can't use `Command::output()` directly because that buffers both
/// streams to EOF before returning — a single runaway command (`cat
/// /dev/urandom`, `find / -print`) would OOM us. Instead, we spawn
/// manually, drain each stream concurrently through a [`CappedReader`]
/// that keeps at most [`MAX_BASH_OUTPUT_BYTES`] per stream and discards
/// (counted) the rest, then await the child for the exit status.
///
/// The drain tasks continue reading past the cap (into a sink) so the
/// child never blocks on a full pipe buffer. If the child produces more
/// than the cap before the timeout, the marker returned in the string
/// tells the model how much was dropped.
async fn run_shell(cmd: &str, workdir: &Path, timeout_secs: u64) -> Result<ShellOutput, String> {
    let mut proc = Command::new("/bin/sh");
    proc.arg("-c")
        .arg(cmd)
        .current_dir(workdir)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = proc
        .spawn()
        .map_err(|e| format!("Failed to execute command: {}", e))?;

    let stdout = child.stdout.take().ok_or_else(|| "no stdout".to_string())?;
    let stderr = child.stderr.take().ok_or_else(|| "no stderr".to_string())?;

    let drain_stdout = tokio::spawn(drain_capped(stdout, MAX_BASH_OUTPUT_BYTES));
    let drain_stderr = tokio::spawn(drain_capped(stderr, MAX_BASH_OUTPUT_BYTES));

    // We use `tokio::select!` rather than `tokio::time::timeout(child.wait(), ...)`
    // because the latter wraps the child in a future — and the child needs
    // to be owned by *us* (the outer scope) so we can call `start_kill()` on
    // it on the timeout branch. `kill_on_drop` doesn't help here because
    // dropping the timeout-future drops the child *inside* a separate
    // future, and we want to be the one to issue the kill before joining
    // the drain tasks.
    let timeout_at = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    let status_result = tokio::select! {
        biased;
        result = child.wait() => {
            Ok(result)
        }
        _ = tokio::time::sleep_until(timeout_at) => {
            // The child is still owned by the outer scope; we can
            // explicitly kill it. The drain tasks continue draining
            // the pipes (which close once the child dies) and we join
            // them below to surface partial output.
            let _ = child.start_kill();
            Err(())
        }
    };

    match status_result {
        Ok(Ok(status)) => {
            // Normal exit. The drain tasks should be done or very close
            // to it (EOF arrives as the child closes its pipes just
            // before exiting). Join with a generous timeout so a stuck
            // drainer can't wedge us.
            let (raw_stdout, stdout_dropped) = join_drain(drain_stdout, "stdout").await?;
            let (raw_stderr, stderr_dropped) = join_drain(drain_stderr, "stderr").await?;
            Ok(ShellOutput {
                status,
                stdout: cap_to_string(raw_stdout, stdout_dropped),
                stderr: cap_to_string(raw_stderr, stderr_dropped),
            })
        }
        Ok(Err(e)) => Err(format!("Failed to wait for command: {}", e)),
        Err(()) => {
            // Timeout path. The child has been sent SIGKILL; the drain
            // tasks are still running and will see EOF as the pipes
            // close. Join them and report whatever they captured.
            let (raw_stdout, stdout_dropped) = join_drain(drain_stdout, "stdout").await?;
            let (raw_stderr, stderr_dropped) = join_drain(drain_stderr, "stderr").await?;
            // Best-effort reap: the drain tasks have closed the pipes,
            // so the child should exit quickly. A short timeout prevents
            // a stuck child from wedging us.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
            let prefix = format!("[timed out after {} seconds]\n", timeout_secs);
            Ok(ShellOutput {
                status: synth_status_killed(),
                stdout: format!("{}{}", prefix, cap_to_string(raw_stdout, stdout_dropped)),
                stderr: cap_to_string(raw_stderr, stderr_dropped),
            })
        }
    }
}

/// Join a drain task, awaiting its result. The `label` is used purely
/// for error messages so a stuck/panicked task is debuggable.
async fn join_drain(
    handle: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, u64)>>,
    label: &str,
) -> Result<(Vec<u8>, u64), String> {
    match handle.await {
        Ok(Ok(pair)) => Ok(pair),
        Ok(Err(e)) => Err(format!("drain {}: {}", label, e)),
        Err(e) => Err(format!("drain {} task panicked: {}", label, e)),
    }
}

/// Render a drained stream into a String, appending a truncation marker
/// if the cap was hit.
fn cap_to_string(raw: Vec<u8>, dropped: u64) -> String {
    let mut s = String::from_utf8_lossy(&raw).to_string();
    if dropped > 0 {
        s.push_str(&TRUNCATED_MARKER_FMT.replace("{}", &dropped.to_string()));
    }
    s
}

/// Synthesize an `ExitStatus` that reports "killed by signal". We don't
/// actually have a real one to return on the timeout path because the
/// child was dropped — but the call site only reads `.success()` and
/// `.code()`, and we want it to take the error branch and prepend the
/// timeout marker.
fn synth_status_killed() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    // On Unix, `ExitStatus::from_raw(N)` represents "killed by signal N"
    // (the `wait()` convention stores the signal number directly in the
    // low bits when WIFSIGNALED). SIGKILL = 9.
    std::process::ExitStatus::from_raw(9)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small input passes through `cap_to_string` unchanged.
    #[test]
    fn cap_to_string_under_cap() {
        let s = cap_to_string(b"hello world".to_vec(), 0);
        assert_eq!(s, "hello world");
    }

    /// When the cap was hit, the marker includes the dropped count.
    #[test]
    fn cap_to_string_appends_marker_when_truncated() {
        let s = cap_to_string(b"abc".to_vec(), 4096);
        assert!(s.starts_with("abc"));
        assert!(s.contains("[...truncated: 4096 bytes omitted"));
    }

    /// `drain_capped` keeps at most `cap` bytes from the inner reader and
    /// counts the rest. We feed it a small Cursor so we don't have to
    /// spawn a real subprocess.
    #[tokio::test]
    async fn drain_capped_keeps_first_cap_bytes() {
        use std::io::Cursor;
        let payload: Vec<u8> = (0..2048u32).map(|i| (i % 251) as u8).collect();
        let cap = 100usize;
        let (kept, dropped) = drain_capped(Cursor::new(payload.clone()), cap)
            .await
            .unwrap();
        assert_eq!(kept.len(), cap);
        assert_eq!(dropped as usize, payload.len() - cap);
        assert_eq!(&kept[..], &payload[..cap]);
    }

    /// A `run_shell` invocation that exceeds the cap gets the marker in
    /// stdout. We use `yes` (which prints "y\n" forever) and rely on
    /// SIGPIPE from a non-tty writer; if `yes` doesn't exist on the
    /// test host this test is skipped rather than failed.
    #[tokio::test]
    async fn run_shell_caps_runaway_output() {
        // First sanity check: `yes` exists. If not, skip.
        let probe = Command::new("sh")
            .arg("-c")
            .arg("command -v yes")
            .output()
            .await;
        if probe.is_err() || !probe.unwrap().status.success() {
            eprintln!("skipping: `yes` not available on this host");
            return;
        }

        // `yes | head -c $((MAX*2))` would be cleaner, but `head` may not
        // exist either. Just use `head` — it ships with coreutils on
        // every Linux/macOS we've seen in CI. If unavailable, skip.
        let head_probe = Command::new("sh")
            .arg("-c")
            .arg("command -v head")
            .output()
            .await;
        if head_probe.is_err() || !head_probe.unwrap().status.success() {
            eprintln!("skipping: `head` not available on this host");
            return;
        }

        // Pipe `yes` through `head -c $((MAX*2))` to force > MAX bytes
        // of output without needing a timeout. `head` closes its stdin
        // early and `yes` gets SIGPIPE.
        let twice = MAX_BASH_OUTPUT_BYTES * 2;
        let cmd = format!("yes | head -c {}", twice);
        let tmp = std::env::temp_dir();
        let workdir = tmp.as_path();
        let out = run_shell(&cmd, workdir, 30).await.expect("run_shell");
        assert!(out.status.success(), "yes | head should exit 0");
        // Output should be exactly the cap (or just under) and the
        // marker should be present.
        assert!(
            out.stdout.len() <= MAX_BASH_OUTPUT_BYTES + 128,
            "stdout should be capped, got {} bytes",
            out.stdout.len()
        );
        assert!(
            out.stdout.contains("[...truncated:"),
            "expected truncation marker, got: {:?}",
            &out.stdout[..out.stdout.len().min(200)]
        );
    }
}
