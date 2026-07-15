use crate::session::process_group::{kill_process_group, reap_child, setup_process_group};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::process::Command;

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
pub const MAX_BASH_OUTPUT_BYTES: usize = 1024 * 1024;

/// Marker appended to a stream that hit the cap. Includes the count of
/// dropped bytes so the model can decide whether to re-run with a narrower
/// filter (e.g. `head -n 1000`).
const TRUNCATED_MARKER_FMT: &str =
    "\n[...truncated: {} bytes omitted, output exceeded 1 MiB cap...]\n";

/// Shell interpreter used for model-driven bash commands.
///
/// Unix releases use `/bin/sh` because POSIX `sh` is always present and the
/// deny-list/safety logic is written for Unix shell syntax. Windows releases
/// target the `bash` executable shipped with Git for Windows / WSL so the
/// same safety logic applies; if it is not on PATH the spawn will fail with
/// a clear message instead of silently using `cmd.exe` and bypassing the
/// safety gate.
#[cfg(unix)]
pub(crate) fn shell_program() -> &'static str {
    "/bin/sh"
}

#[cfg(windows)]
pub(crate) fn shell_program() -> &'static str {
    "bash"
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn shell_program() -> &'static str {
    "sh"
}

/// True if `path` is world-writable (Unix other bit set). On non-Unix
/// platforms we cannot easily determine this, so we conservatively treat
/// the directory as safe and rely on the absolute-path filter.
#[cfg(unix)]
fn is_world_writable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(m) => m.permissions().mode() & 0o002 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_world_writable(_path: &Path) -> bool {
    false
}

/// Curated PATH for subprocesses that resolve external commands.
///
/// Starts from the supplied PATH string, drops relative entries and
/// world-writable non-system directories (e.g. `/tmp`), and prepends a core
/// set of standard system directories so basic tooling (`bash`, `cargo`,
/// `git`, `node`, etc.) remains resolvable even on hosts where a system
/// directory happens to be world-writable. This closes a PATH-shadowing
/// attack where a malicious binary in a writable directory is placed
/// earlier on PATH so a legitimate-looking command resolves to it, while
/// still preserving common non-writable user directories (e.g.
/// `~/.cargo/bin`).
pub(crate) fn sanitized_path(original: &str) -> String {
    use std::collections::HashSet;

    let sep = if cfg!(windows) { ';' } else { ':' };

    // Standard system directories are always included, and listed first so
    // they cannot be shadowed by a writable directory that happens to appear
    // earlier in the original PATH.
    let system_dirs: &[&str] = if cfg!(windows) {
        &[
            r"C:\Windows\System32",
            r"C:\Windows",
            r"C:\Program Files\Git\usr\bin",
        ]
    } else {
        &[
            "/usr/local/sbin",
            "/usr/local/bin",
            "/usr/sbin",
            "/usr/bin",
            "/sbin",
            "/bin",
        ]
    };

    let mut seen = HashSet::new();
    let mut kept = Vec::new();

    for dir in system_dirs {
        if seen.insert((*dir).to_string()) {
            kept.push((*dir).to_string());
        }
    }

    for entry in original.split(sep) {
        if entry.is_empty() {
            continue;
        }
        let path = Path::new(entry);
        if !path.is_absolute() {
            continue;
        }
        // System directories were already added above.
        if system_dirs.contains(&entry) {
            continue;
        }
        if is_world_writable(path) {
            continue;
        }
        if seen.insert(entry.to_string()) {
            kept.push(entry.to_string());
        }
    }

    if kept.is_empty() {
        if cfg!(windows) {
            String::from(r"C:\Windows\System32;C:\Windows;C:\Program Files\Git\usr\bin")
        } else {
            String::from("/usr/bin:/bin:/usr/local/bin")
        }
    } else {
        kept.join(&sep.to_string())
    }
}

/// Return a curated PATH for the current process, reading the host PATH once.
///
/// This is the entry point used by the model's bash tool; tests should call
/// `sanitized_path` directly with a constructed string to avoid mutating
/// global environment state.
fn model_command_path() -> String {
    let original = std::env::var("PATH").unwrap_or_default();
    sanitized_path(&original)
}

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
pub async fn drain_capped<R>(r: R, cap: usize) -> std::io::Result<(Vec<u8>, u64)>
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

/// Heuristic to distinguish a timeout produced by `run_shell` from a
/// genuine non-zero exit. `run_shell` prefixes stdout with the timeout
/// marker when the timer fires, and synthesises a killed exit status.
pub(crate) fn is_timeout_marker(output: &ShellOutput, timeout_secs: u64) -> bool {
    !output.status.success()
        && output
            .stdout
            .starts_with(&format!("[timed out after {timeout_secs} seconds]"))
}

pub struct ShellOutput {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
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
pub async fn run_shell(
    cmd: &str,
    workdir: &Path,
    timeout_secs: u64,
) -> Result<ShellOutput, ShellError> {
    run_shell_with_token(cmd, workdir, timeout_secs, None).await
}

/// Run a shell command with optional cancellation. The cancellation
/// token is polled alongside the child so a user cancel stops the shell
/// as promptly as the timeout path does.
pub async fn run_shell_with_token(
    cmd: &str,
    workdir: &Path,
    timeout_secs: u64,
    token: Option<&tokio_util::sync::CancellationToken>,
) -> Result<ShellOutput, ShellError> {
    let mut proc = Command::new(shell_program());
    proc.arg("-c")
        .arg(cmd)
        .current_dir(workdir)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", model_command_path());

    setup_process_group(&mut proc);

    let mut child = proc
        .spawn()
        .map_err(|e| ShellError::Spawn(format!("Failed to execute command: {e}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ShellError::Spawn("no stdout".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ShellError::Spawn("no stderr".to_string()))?;

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
            Err(ShellErrorKind::Timeout)
        }
        _ = async { if let Some(t) = token { t.cancelled().await; } }, if token.is_some() => {
            Err(ShellErrorKind::Cancelled)
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
        Ok(Err(e)) => Err(ShellError::Spawn(format!(
            "Failed to wait for command: {e}"
        ))),
        Err(ShellErrorKind::Timeout) => {
            // Timeout path. The child has been sent SIGKILL; the drain
            // tasks are still running and will see EOF as the pipes
            // close. Join them and report whatever they captured.
            kill_process_group(&mut child);
            let (raw_stdout, stdout_dropped) = join_drain(drain_stdout, "stdout").await?;
            let (raw_stderr, stderr_dropped) = join_drain(drain_stderr, "stderr").await?;
            // Best-effort reap: the drain tasks have closed the pipes,
            // so the child should exit quickly. A short timeout prevents
            // a stuck child from wedging us.
            reap_child(&mut child, Duration::from_secs(2)).await;
            let prefix = format!("[timed out after {timeout_secs} seconds]\n");
            Ok(ShellOutput {
                status: synth_status_killed()?,
                stdout: format!("{}{}", prefix, cap_to_string(raw_stdout, stdout_dropped)),
                stderr: cap_to_string(raw_stderr, stderr_dropped),
            })
        }
        Err(ShellErrorKind::Cancelled) => {
            kill_process_group(&mut child);
            reap_child(&mut child, Duration::from_secs(2)).await;
            Err(ShellError::Cancelled)
        }
    }
}

/// Internal discriminant used only inside the `tokio::select!` so we can
/// distinguish timeout from cancellation without allocating strings.
#[derive(Debug, Clone, Copy)]
enum ShellErrorKind {
    Timeout,
    Cancelled,
}

/// Join a drain task, awaiting its result with a bounded timeout.
///
/// The `label` is used purely for error messages so a stuck/panicked
/// task is debuggable. The timeout prevents a misbehaving child that
/// never closes its stdout/stderr from wedging the whole turn.
const DRAIN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

async fn join_drain(
    handle: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, u64)>>,
    label: &str,
) -> Result<(Vec<u8>, u64), ShellError> {
    match tokio::time::timeout(DRAIN_JOIN_TIMEOUT, handle).await {
        Ok(Ok(Ok(pair))) => Ok(pair),
        Ok(Ok(Err(e))) => Err(ShellError::Drain {
            label: label.to_string(),
            message: e.to_string(),
        }),
        Ok(Err(e)) => Err(ShellError::Drain {
            label: label.to_string(),
            message: format!("task panicked: {e}"),
        }),
        Err(_) => Err(ShellError::Drain {
            label: label.to_string(),
            message: format!("task did not finish within {DRAIN_JOIN_TIMEOUT:?}"),
        }),
    }
}

/// Render a drained stream into a String, appending a truncation marker
/// if the cap was hit.
pub fn cap_to_string(raw: Vec<u8>, dropped: u64) -> String {
    let mut s = String::from_utf8_lossy(&raw).to_string();
    if dropped > 0 {
        s.push_str(&TRUNCATED_MARKER_FMT.replace("{}", &dropped.to_string()));
    }
    s
}

/// Failure modes for a foreground shell invocation.
#[derive(Debug, Clone)]
pub enum ShellError {
    /// Failed to spawn or wait on the child process.
    Spawn(String),
    /// A stdout/stderr drain task did not finish or panicked.
    Drain { label: String, message: String },
    /// The caller cancelled the invocation before it completed.
    Cancelled,
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(msg) => write!(f, "{msg}"),
            Self::Drain { label, message } => write!(f, "drain {label}: {message}"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Synthesize an `ExitStatus` that reports "killed by signal". We don't
/// actually have a real one to return on the timeout path because the
/// child was dropped — but the call site only reads `.success()` and
/// `.code()`, and we want it to take the error branch and prepend the
/// timeout marker.
fn synth_status_killed() -> Result<std::process::ExitStatus, ShellError> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // On Unix, `ExitStatus::from_raw(N)` represents "killed by signal N"
        // (the `wait()` convention stores the signal number directly in the
        // low bits when WIFSIGNALED). SIGKILL = 9.
        Ok(std::process::ExitStatus::from_raw(9))
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        // On Windows, `from_raw` is the exit code. Returning 9 keeps
        // `.success()` false and `.code()` returning `Some(9)`.
        Ok(std::process::ExitStatus::from_raw(9))
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Exotic target fallback: spawn a trivial command that exits 9.
        // This path is only reached on timeout, so the overhead is
        // acceptable and better than failing to compile. Propagate the
        // spawn error instead of panicking so a missing `sh` doesn't abort
        // the CLI.
        std::process::Command::new(shell_program())
            .arg("-c")
            .arg("exit 9")
            .status()
            .map_err(|e| ShellError::Spawn(format!("fallback status command failed: {e}")))
    }
}

mod safety;
pub use safety::{check_bash_command, check_bash_command_str};

#[cfg(test)]
mod tests {
    use super::safety::word_boundary_match;
    use super::*;
    use crate::session::access::{DenyList, PathGuard};
    use crate::shared::test_util::remove_test_file;

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

    /// A timed-out `run_shell` invocation must not leave descendants
    /// behind. We nest a `sleep` inside a subshell so it is a
    /// grandchild of the outer shell and verify the survivor never
    /// touches a marker file.
    #[tokio::test]
    async fn run_shell_timeout_kills_descendants() {
        let tmp = std::env::temp_dir();
        let marker = tmp.join(format!(
            "kirkforge_run_shell_orphan_test_{}",
            std::process::id()
        ));
        let marker_str = marker.to_string_lossy().to_string();
        remove_test_file(&marker);

        // Inner `sh` makes `sleep` a grandchild of the outer shell.
        let cmd = format!("sh -c 'sleep 30; touch {marker_str}'");
        let out = run_shell(&cmd, &tmp, 1)
            .await
            .expect("run_shell should time out, not error");
        assert!(
            out.stdout.contains("timed out"),
            "expected timeout marker, got: {:?}",
            &out.stdout[..out.stdout.len().min(200)]
        );

        // Allow a generous window for a would-be orphan to touch the marker.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(
            !marker.exists(),
            "descendant process survived timeout and touched marker"
        );
        remove_test_file(&marker);
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
        let cmd = format!("yes | head -c {twice}");
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
    #[test]
    fn test_word_boundary_match_exact() {
        assert!(word_boundary_match("rm -rf /", "rm -rf /"));
    }

    #[test]
    fn test_word_boundary_no_false_positive_trailing_slash() {
        assert!(!word_boundary_match("rm -rf /home/user", "rm -rf /"));
    }

    #[test]
    fn test_word_boundary_match_with_pipe_prefix() {
        assert!(word_boundary_match("echo foo | rm -rf /", "rm -rf /"));
    }

    #[test]
    fn test_word_boundary_match_with_semicolon() {
        assert!(word_boundary_match("cd /; rm -rf /", "rm -rf /"));
    }

    #[test]
    fn test_word_boundary_no_match_in_substring() {
        assert!(!word_boundary_match("rm -rf /home", "rm -rf /"));
    }

    #[test]
    fn test_check_bash_command_blocks_dangerous_exact() {
        let args = serde_json::json!({"command": "rm -rf /"});
        let result = check_bash_command(&args, &DenyList::default(), &PathGuard::default(), false);
        assert!(result.is_some(), "rm -rf / should be blocked");
    }

    #[test]
    fn test_check_bash_command_allows_safe_similar() {
        let args = serde_json::json!({"command": "rm -rf /home/user/temp"});
        let result = check_bash_command(&args, &DenyList::default(), &PathGuard::default(), false);
        assert!(
            result.is_none(),
            "rm -rf /home/user/temp should be allowed, got: {result:?}"
        );
    }

    #[test]
    fn test_check_bash_command_blocks_dd_by_substring() {
        let args = serde_json::json!({"command": "dd if=/dev/zero of=/tmp/out bs=1M count=1"});
        let result = check_bash_command(&args, &DenyList::default(), &PathGuard::default(), false);
        assert!(result.is_some(), "dd if=/dev/zero should be blocked");
    }

    #[test]
    fn test_check_bash_command_blocks_fork_bomb() {
        let args = serde_json::json!({"command": ":(){ :|:& };:"});
        let result = check_bash_command(&args, &DenyList::default(), &PathGuard::default(), false);
        assert!(result.is_some(), "Fork bomb should be blocked");
    }

    #[test]
    fn test_check_bash_command_allows_legitimate_curl() {
        let args = serde_json::json!({"command": "curl -s https://api.example.com/data"});
        let result = check_bash_command(&args, &DenyList::default(), &PathGuard::default(), false);
        assert!(
            result.is_none(),
            "curl should not be blocked by check_bash_command"
        );
    }

    #[test]
    fn test_check_bash_command_str_blocks_metadata_endpoint() {
        let result = check_bash_command_str(
            "curl http://169.254.169.254/latest/meta-data/",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result.is_some_and(|m| m.contains("metadata")),
            "metadata endpoint should be blocked"
        );
    }

    #[test]
    fn test_check_bash_command_str_sandbox_workdir_rejects_escape() {
        let path_guard = crate::session::access::PathGuard {
            sandbox_dir: Some(std::env::temp_dir()),
            ..Default::default()
        };
        let result =
            check_bash_command_str("ls", Some("/etc"), &DenyList::default(), &path_guard, true);
        assert!(
            result
                .as_ref()
                .is_some_and(|m| m.contains("outside sandbox")),
            "workdir outside sandbox should be rejected, got: {result:?}"
        );
    }

    #[test]
    fn test_check_bash_command_str_sandbox_rejects_unresolvable_workdir() {
        let path_guard = crate::session::access::PathGuard {
            sandbox_dir: Some(std::env::temp_dir()),
            ..Default::default()
        };
        let result = check_bash_command_str(
            "ls",
            Some("/nonexistent/path/that/cannot/be/canonicalized"),
            &DenyList::default(),
            &path_guard,
            true,
        );
        assert!(
            result
                .as_ref()
                .is_some_and(|m| m.contains("cannot be resolved")),
            "unresolvable workdir should be rejected, got: {result:?}"
        );
    }

    #[test]
    fn test_check_bash_command_str_blocks_privilege_escalation() {
        // Commands chosen to avoid earlier deny-list/path checks so the
        // assertion verifies the privilege-escalation pattern itself.
        for cmd in ["sudo apt update", "su - root", "doas ls"] {
            let result = check_bash_command_str(
                cmd,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            );
            assert!(
                result
                    .as_ref()
                    .is_some_and(|m| m.contains("privilege escalation")),
                "{cmd} should be blocked, got: {result:?}"
            );
        }
    }

    #[test]
    fn test_check_bash_command_str_allows_sudo_in_larger_word() {
        // `sudoku` or `sudoers` should not trip the `sudo` boundary check.
        let result = check_bash_command_str(
            "echo sudoku",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result.is_none(),
            "sudoku should not be blocked, got: {result:?}"
        );
    }

    #[test]
    fn test_check_bash_command_str_blocks_password_prompts() {
        for cmd in ["read -s password", "stty -echo; read", "passwd root"] {
            let result = check_bash_command_str(
                cmd,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            );
            assert!(
                result
                    .as_ref()
                    .is_some_and(|m| m.contains("interactive password prompt")),
                "{cmd} should be blocked, got: {result:?}"
            );
        }
    }

    #[test]
    fn test_check_bash_command_str_blocks_dangerous_redirections() {
        // Use /etc/hosts (not in the earlier denied-path list) so we verify
        // the dangerous-redirection patterns directly.
        for cmd in [
            "echo foo > /etc/hosts",
            "echo bar >| /etc/hosts",
            "echo baz | tee /etc/hosts",
        ] {
            let result = check_bash_command_str(
                cmd,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            );
            assert!(
                result
                    .as_ref()
                    .is_some_and(|m| m.contains("dangerous redirection")),
                "{cmd} should be blocked, got: {result:?}"
            );
        }
    }

    #[test]
    fn test_check_bash_command_str_allows_safe_redirections() {
        let result = check_bash_command_str(
            "echo foo > /tmp/out.txt",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result.is_none(),
            "redirect to /tmp should be allowed, got: {result:?}"
        );
    }

    #[test]
    fn test_check_bash_command_str_blocks_quoted_dangerous_command() {
        // Trivial quoting evasions must not bypass the deny-list.
        for cmd in [
            "r'm -rf /'",
            "rm '-rf' /",
            "rm -rf / # cleanup",
            "rm -rf  /",
            "rm -rf / ; echo done",
            "rm -fr /",
            "rm --no-preserve-root -rf /",
            "chmod -R  777 /",
        ] {
            let result = check_bash_command_str(
                cmd,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            );
            assert!(
                result
                    .as_ref()
                    .is_some_and(|m| m.contains("dangerous pattern")),
                "{cmd} should be blocked, got: {result:?}"
            );
        }
    }

    #[test]
    fn test_check_bash_command_str_blocks_quoted_redirection() {
        // Redirections with extra whitespace or quotes must still be caught.
        for cmd in [
            "echo foo >  /etc/hosts",
            "echo bar >| '/etc/hosts'",
            "echo baz 2>/etc/hosts",
            "echo qux &> /etc/hosts",
        ] {
            let result = check_bash_command_str(
                cmd,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            );
            assert!(
                result
                    .as_ref()
                    .is_some_and(|m| m.contains("dangerous redirection")),
                "{cmd} should be blocked, got: {result:?}"
            );
        }
    }

    #[test]
    fn test_check_bash_command_str_blocks_windows_redirections() {
        for cmd in [
            "echo pwned > C:/Windows/System32/drivers/etc/hosts",
            "echo pwned > C:\\Windows\\System32\\drivers\\etc\\hosts",
            "echo pwned > /c/windows/System32/drivers/etc/hosts",
            "echo pwned | tee /mnt/c/windows/temp/out.txt",
        ] {
            let result = check_bash_command_str(
                cmd,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            );
            assert!(
                result
                    .as_ref()
                    .is_some_and(|m| m.contains("dangerous redirection")),
                "{cmd} should be blocked, got: {result:?}"
            );
        }
    }

    #[test]
    fn test_check_bash_command_str_blocks_backslash_escape_variant() {
        // `rm -rf \/` is the same destructive command to the shell.
        let result = check_bash_command_str(
            "rm -rf \\/",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result
                .as_ref()
                .is_some_and(|m| m.contains("dangerous pattern")),
            "rm -rf \\/ should be blocked, got: {result:?}"
        );
    }

    #[test]
    fn test_check_bash_command_str_allows_quoted_safe_strings() {
        // A safe string that happens to contain a dangerous-looking literal is
        // still a false positive we accept for safety, but a benign command
        // without a real redirection must pass.
        let result = check_bash_command_str(
            "echo 'hello world'",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result.is_none(),
            "benign echo should be allowed, got: {result:?}"
        );
    }

    #[test]
    fn test_check_bash_command_str_blocks_ifs_expansion_evasion() {
        // `${IFS:- }` expands to a space, so the destructive command only
        // materializes at execution time. The literal deny-list must reject it.
        for cmd in [
            "rm${IFS:- }-rf${IFS:- }/",
            "rm${IFS}-rf${IFS}/",
            "rm$IFS-rf$IFS/",
        ] {
            let result = check_bash_command_str(
                cmd,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            );
            assert!(
                result
                    .as_ref()
                    .is_some_and(|m| m.contains("parameter expansion")),
                "{cmd} should be blocked, got: {result:?}"
            );
        }
    }

    #[test]
    fn test_check_bash_command_str_blocks_ansi_c_quoting_evasion() {
        // `$' '` expands to a space; `$'\t'` expands to a tab. These ANSI-C
        // quoting tricks can rebuild forbidden tokens without writing them.
        for cmd in ["rm$' '-rf$' '/", "echo$'\t'/etc/shadow"] {
            let result = check_bash_command_str(
                cmd,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            );
            assert!(
                result
                    .as_ref()
                    .is_some_and(|m| m.contains("parameter expansion")),
                "{cmd} should be blocked, got: {result:?}"
            );
        }
    }

    #[test]
    fn test_check_bash_command_str_blocks_eval_content_bypass() {
        // `eval` executes a string at runtime; if that string is visible in the
        // command it must still pass through the safety gate.
        let result = check_bash_command_str(
            "eval 'rm -rf /'",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result
                .as_ref()
                .is_some_and(|m| m.contains("dangerous pattern")),
            "eval with literal destructive content should be blocked, got: {result:?}"
        );
    }

    #[test]
    fn test_check_bash_command_str_blocks_source_content_bypass() {
        // `source` / process substitution can pull destructive content into
        // the shell. The literal content must still be caught.
        let result = check_bash_command_str(
            "source <(echo 'rm -rf /')",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result
                .as_ref()
                .is_some_and(|m| m.contains("dangerous pattern")),
            "source with literal destructive content should be blocked, got: {result:?}"
        );
    }

    #[test]
    fn test_check_bash_command_str_blocks_denied_url() {
        let mut deny_list = DenyList::default();
        deny_list
            .url_patterns
            .push("https://internal.example.com".into());
        let result = check_bash_command_str(
            "curl https://internal.example.com/secrets",
            None,
            &deny_list,
            &PathGuard::default(),
            false,
        );
        assert!(
            result.as_ref().is_some_and(|m| m.contains("denied URL")),
            "denied URL in bash command should be blocked, got: {result:?}"
        );
    }

    /// `sanitized_path` keeps absolute, non-world-writable directories and
    /// drops relative or world-writable non-system entries. System directories
    /// are always included even if they happen to be world-writable.
    #[test]
    fn test_sanitized_path_filters_world_writable_and_relative() {
        let tmp = std::env::temp_dir();
        let safe = tmp.join("kirkforge_safe_path_test");
        let _ = std::fs::remove_dir_all(&safe);
        std::fs::create_dir_all(&safe).unwrap();
        // Ensure the test directory is NOT world-writable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&safe).unwrap().permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(&safe, perms).unwrap();
        }

        let sep = if cfg!(windows) { ';' } else { ':' };
        let constructed = format!(".{sep}{safe}{sep}/tmp{sep}/usr/bin", safe = safe.display());
        let result = sanitized_path(&constructed);

        let parts: Vec<&str> = result.split(sep).collect();
        assert!(
            !parts.contains(&"."),
            "relative path '.' should be dropped, got: {result}"
        );
        assert!(
            !parts.contains(&"/tmp"),
            "world-writable /tmp should be dropped, got: {result}"
        );
        assert!(
            parts.contains(&"/usr/bin"),
            "safe system path should be kept, got: {result}"
        );
        let safe_str = safe.to_string_lossy().to_string();
        assert!(
            parts.contains(&safe_str.as_str()),
            "safe test dir should be kept, got: {result}"
        );

        let _ = std::fs::remove_dir_all(&safe);
    }

    /// When the supplied PATH is empty, fall back to a known-safe set.
    #[test]
    fn test_sanitized_path_fallback_when_empty() {
        let result = sanitized_path("");
        if cfg!(windows) {
            assert!(result.contains(r"C:\Windows\System32"), "got: {result}");
        } else {
            assert!(result.contains("/usr/bin"), "got: {result}");
            assert!(result.contains("/bin"), "got: {result}");
        }
    }
}
