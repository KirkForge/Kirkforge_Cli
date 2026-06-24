use crate::session::access::{DenyList, PathGuard};
use crate::session::bash_jobs::global_registry;
use crate::session::process_group::{kill_process_group, setup_process_group};
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::bash_minify;
use crate::tools::Tool;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::process::Command;

pub struct Bash {
    deny_list: DenyList,
    path_guard: PathGuard,
    bash_sandbox_workdir: bool,
}

impl Bash {
    pub fn new(deny_list: DenyList, path_guard: PathGuard, bash_sandbox_workdir: bool) -> Self {
        Self {
            deny_list,
            path_guard,
            bash_sandbox_workdir,
        }
    }
}

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
            match registry
                .spawn(
                    &cmd,
                    workdir,
                    timeout,
                    &self.deny_list,
                    &self.path_guard,
                    self.bash_sandbox_workdir,
                )
                .await
            {
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
) -> Result<ShellOutput, String> {
    let mut proc = Command::new("/bin/sh");
    proc.arg("-c")
        .arg(cmd)
        .current_dir(workdir)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    setup_process_group(&mut proc);

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
            // The child is still owned by the outer scope; kill the
            // whole process group so grandchildren cannot outlive it
            // and keep the pipes open.
            kill_process_group(&mut child);
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
pub fn cap_to_string(raw: Vec<u8>, dropped: u64) -> String {
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
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // On Unix, `ExitStatus::from_raw(N)` represents "killed by signal N"
        // (the `wait()` convention stores the signal number directly in the
        // low bits when WIFSIGNALED). SIGKILL = 9.
        std::process::ExitStatus::from_raw(9)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        // On Windows, `from_raw` is the exit code. Returning 9 keeps
        // `.success()` false and `.code()` returning `Some(9)`.
        std::process::ExitStatus::from_raw(9)
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Exotic target fallback: spawn a trivial command that exits 9.
        // This path is only reached on timeout, so the overhead is
        // acceptable and better than failing to compile.
        std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 9")
            .status()
            .expect("fallback status command")
    }
}

/// Dangerous shell-command substrings. These are blocked regardless of
/// approval state because they can destroy data or compromise the host.
const DANGEROUS_SHELL_COMMANDS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    ":(){ :|:& };:",
    "> /dev/sda",
    "mkfs.",
    "dd if=/dev/zero of=",
    "chmod -R 777 /",
    "chmod 777 /",
    "dd if=/dev/random",
    "> /dev/null < /dev/sda",
];

/// True if `pattern` appears in `cmd` at a word boundary (start/end of
/// string, whitespace, or shell metacharacter). Used so `rm -rf /` blocks
/// the exact dangerous command even when it appears inside a pipeline.
fn word_boundary_match(cmd: &str, pattern: &str) -> bool {
    let boundaries = [' ', '\t', '\n', '|', ';', '&', '(', ')', '<', '>', '\0'];
    let p: Vec<char> = pattern.chars().collect();
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i + p.len() <= chars.len() {
        if chars[i..i + p.len()].iter().collect::<String>() == *pattern {
            let start_ok = i == 0 || boundaries.contains(&chars[i - 1]);
            let end_ok = i + p.len() >= chars.len() || boundaries.contains(&chars[i + p.len()]);
            if start_ok && end_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Safety check for a bash command. Returns `Some(reason)` if the command
/// should be blocked, `None` if it may proceed.
///
/// This is shared between the model's `bash` tool, the `!` bang passthrough,
/// the `/test` slash command, and lifecycle hooks so every shell execution
/// goes through the same sandbox, deny-list, and dangerous-pattern gates.
pub fn check_bash_command_str(
    cmd: &str,
    workdir: Option<&str>,
    deny_list: &DenyList,
    path_guard: &PathGuard,
    bash_sandbox_workdir: bool,
) -> Option<String> {
    // 1. Sandboxed workdir policy. If enabled, reject an explicit workdir
    //    that points outside the sandbox. If we cannot canonicalize the path
    //    we deny: a non-canonical path containing `..` could pass the
    //    prefix check while resolving outside the sandbox.
    if bash_sandbox_workdir {
        if let Some(workdir) = workdir {
            if !workdir.is_empty() {
                let workdir_path = Path::new(workdir);
                let resolved = match workdir_path.canonicalize() {
                    Ok(p) => p,
                    Err(_) => {
                        return Some(format!(
                            "🔒 Bash workdir cannot be resolved: {} (sandbox enforcement active)",
                            workdir
                        ));
                    }
                };
                if let Some(ref sandbox) = path_guard.sandbox_dir {
                    let sb = match sandbox.canonicalize() {
                        Ok(p) => p,
                        Err(_) => {
                            return Some(format!(
                                "🔒 Sandbox directory cannot be resolved: {}",
                                sandbox.display()
                            ));
                        }
                    };
                    if !resolved.starts_with(&sb) {
                        return Some(format!(
                            "🔒 Bash workdir outside sandbox: {} (sandbox: {})",
                            workdir,
                            sandbox.display()
                        ));
                    }
                }
            }
        }
    }

    // 2. Hard-coded metadata endpoint blocks.
    if cmd.contains("169.254.169.254")
        || cmd.contains("metadata.google")
        || cmd.contains("metadata.aws")
    {
        return Some("🔒 Command blocked: contains reference to metadata endpoints".into());
    }

    // 3. User-configured URL deny list.
    for url_prefix in &deny_list.url_patterns {
        if !url_prefix.is_empty() && cmd.contains(url_prefix) {
            return Some(format!(
                "🔒 Command blocked: references denied URL '{}'",
                url_prefix
            ));
        }
    }

    // 4. Built-in dangerous shell patterns and hard-coded system paths.
    for pattern in DANGEROUS_SHELL_COMMANDS {
        let needs_word_boundary = pattern.ends_with('/') || pattern.ends_with(' ');
        let matches = if needs_word_boundary {
            word_boundary_match(cmd, pattern)
        } else {
            cmd.contains(pattern)
        };
        if matches {
            return Some(format!(
                "🔒 Command blocked: dangerous pattern '{}' detected",
                pattern
            ));
        }
    }

    for pat in [
        "/etc/shadow",
        "/etc/passwd",
        "/etc/sudoers",
        "~/.ssh",
        "/root/",
    ] {
        if cmd.contains(pat) {
            return Some(format!(
                "🔒 Command blocked: references denied path '{}'",
                pat
            ));
        }
    }

    // 5. User-configured path deny list. Tokenize the command and check
    //    each token as a path.
    for token in cmd.split_whitespace() {
        if deny_list.is_path_denied(Path::new(token)) {
            return Some(format!(
                "🔒 Command blocked: references denied path '{}'",
                token
            ));
        }
    }

    None
}

/// JSON-args wrapper around [`check_bash_command_str`] for the model's
/// `bash` tool invocation path.
pub fn check_bash_command(
    args: &serde_json::Value,
    deny_list: &DenyList,
    path_guard: &PathGuard,
    bash_sandbox_workdir: bool,
) -> Option<String> {
    let cmd = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let workdir = args.get("workdir").and_then(|w| w.as_str());
    check_bash_command_str(cmd, workdir, deny_list, path_guard, bash_sandbox_workdir)
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
        let _ = std::fs::remove_file(&marker);

        // Inner `sh` makes `sleep` a grandchild of the outer shell.
        let cmd = format!("sh -c 'sleep 30; touch {}'", marker_str);
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
        let _ = std::fs::remove_file(&marker);
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
            "rm -rf /home/user/temp should be allowed, got: {:?}",
            result
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
            "workdir outside sandbox should be rejected, got: {:?}",
            result
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
            "unresolvable workdir should be rejected, got: {:?}",
            result
        );
    }
}
