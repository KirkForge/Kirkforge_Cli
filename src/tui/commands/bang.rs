//! `!` bash passthrough slash-command.
//!
//! A line starting with `!` is a UX escape hatch: `!cargo test` runs
//! the command directly via the shell and shows the output in the
//! chat, with no model round trip and no approval gate. This is the
//! feature users reach for when they want to "do this now and don't
//! ask the model" — for fast feedback loops, eyeballing a build, or
//! running a one-liner before composing a longer prompt.
//!
//! Design choices:
//! - **No model round trip.** The whole point is "don't wait for
//!   inference." The command runs in `~ms`, not seconds.
//! - **No approval gate by default.** The user *typed* the command.
//!   When the config flag `bang_requires_approval` is set to `true`,
//!   the TUI pauses and asks for Y/N confirmation before executing
//!   the command (handled in `src/tui/approval_keys.rs`).
//! - **30-second timeout** (matches the bash tool's default).
//! - **Output goes through the existing chat widget** so the
//!   collapse/expand UX applies automatically. A `!find .` that
//!   returns 500 lines is collapsed to a 4-line summary box; the
//!   user hits Enter or Tab on empty input to see the full output.
//! - **Stderr is shown separately** with a `⚠ stderr:` marker.
//! - **Exit code is always shown**, even on success.
//! - **Pure formatting helpers are unit-tested**; the shell spawn
//!   is tested with fast `echo` / `false` / `true` commands.

/// Default timeout for `!` commands. Matches the bash tool's
/// foreground-execution timeout; users running long commands should
/// use `!cmd &` (background) and then `/jobs` to poll status.
pub const BANG_DEFAULT_TIMEOUT_SECS: u64 = 30;

/// What a `!` command actually did. Pure data — the formatting helpers
/// turn this into a display string. Splitting spawn from formatting
/// keeps the presentation policy testable without I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BangResult {
    /// The command that was run, verbatim. Display only.
    pub cmd: String,
    /// Process exit code, or `-1` if the process was killed by signal.
    pub exit_code: i32,
    /// Captured stdout (UTF-8 lossy).
    pub stdout: String,
    /// Captured stderr (UTF-8 lossy).
    pub stderr: String,
    /// `true` if the command hit the timeout and was killed.
    pub timed_out: bool,
    /// How long the command took, in milliseconds. Display only.
    pub elapsed_ms: u64,
}

impl BangResult {
    /// `true` if the process exited with status 0 and was not killed by
    /// the timeout. Used by `format_bang_output` to pick the icon and
    /// banner colour.
    pub fn is_success(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }
}

/// Run a shell command directly without going through the model. The
/// user typed `!` deliberately — no approval gate, no model round trip.
///
/// This is a thin wrapper over `tokio::process::Command` with a timeout
/// and `kill_on_drop`, matching the bash tool's foreground-execution
/// shape (`src/tools/bash.rs::run_shell`). Working dir is the current
/// process dir; we deliberately don't chdir to the project root here —
/// `!` is a "I want to do this now in the shell I'm in" feature, not a
/// re-skin of the bash tool.
///
/// Returns a `BangResult` capturing stdout/stderr/exit_code/timed_out
/// for the formatter to consume. Does not write to `state` — the
/// caller (`keys.rs`) is responsible for pushing the formatted string
/// into `state.messages` so the conversation log records what happened.
pub async fn run_bang_command(cmd: &str) -> BangResult {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let mut proc = tokio::process::Command::new("/bin/sh");
    proc.arg("-c")
        .arg(cmd)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = tokio::time::timeout(
        Duration::from_secs(BANG_DEFAULT_TIMEOUT_SECS),
        proc.output(),
    )
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match output {
        Ok(Ok(out)) => BangResult {
            cmd: cmd.to_string(),
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            timed_out: false,
            elapsed_ms,
        },
        Ok(Err(e)) => BangResult {
            cmd: cmd.to_string(),
            // `-1` signals "could not even spawn", distinct from a process
            // that ran and exited non-zero. The formatter surfaces this
            // as a clear error to the user.
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("Failed to execute command: {}", e),
            timed_out: false,
            elapsed_ms,
        },
        Err(_) => BangResult {
            cmd: cmd.to_string(),
            exit_code: -1,
            stdout: String::new(),
            stderr: format!(
                "Command timed out after {} seconds",
                BANG_DEFAULT_TIMEOUT_SECS
            ),
            timed_out: true,
            elapsed_ms,
        },
    }
}

/// Format a `BangResult` into a single display string. Pure function —
/// given the same `BangResult`, produces the same string. This is what
/// the user sees in the chat view.
///
/// Layout (success, no stderr):
/// ```text
/// $ cargo build
/// ✅ exit 0 in 1.42s
/// <stdout>
/// ```
pub fn format_bang_output(result: &BangResult) -> String {
    if result.timed_out {
        return format!(
            "$ {}\n⏰ timed out after {}s",
            result.cmd, BANG_DEFAULT_TIMEOUT_SECS
        );
    }

    let elapsed = format_elapsed(result.elapsed_ms);
    let icon = if result.is_success() { "✅" } else { "❌" };

    let mut out = format!(
        "$ {}\n{} exit {} in {}",
        result.cmd, icon, result.exit_code, elapsed
    );

    if !result.stdout.is_empty() {
        out.push('\n');
        out.push_str(&result.stdout);
    }

    if !result.stderr.is_empty() {
        // Trim trailing whitespace from stdout before appending the
        // stderr marker so the layout is clean even when stdout didn't
        // end in a newline.
        if !result.stdout.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("⚠ stderr:\n");
        out.push_str(&result.stderr);
    }

    out
}

/// Format a millisecond duration as a short human string. Pure helper,
/// unit-tested alongside `format_bang_output`.
///
/// Examples:
/// - `0`     → `"0ms"`
/// - `42`    → `"42ms"`
/// - `1420`  → `"1.42s"`
/// - `90000` → `"1m30s"`
///
/// `pub` (rather than private) so the unit tests in
/// `commands/mod.rs` can exercise the boundary cases directly
/// without going through `format_bang_output`.
pub fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        // Two decimal places for sub-minute durations. e.g. 1.42s, 12.34s.
        format!("{:.2}s", ms as f64 / 1000.0)
    } else {
        let secs = ms / 1000;
        let minutes = secs / 60;
        let rem = secs % 60;
        format!("{}m{:02}s", minutes, rem)
    }
}

/// The full `!` command pipeline: run the command, format the result.
/// The caller is responsible for pushing the returned string into
/// `state.messages`.
///
/// This is the function `keys.rs::Enter` calls when the input buffer
/// starts with `!`. The `!` itself is stripped before this is called
/// (the keys handler is responsible for the prefix detection).
///
/// Returns a `String` ready to display. The caller may want to wrap
/// it in `ConversationEntry::tool(summary, full)` for collapse support,
/// but for the v1.2-p14 first cut we return the full string and let
/// the caller decide how to display it.
pub async fn handle_bang_command(cmd: &str) -> String {
    if cmd.trim().is_empty() {
        // Empty `!` is a no-op. We could also surface a hint about
        // "type `!help` for what this does" but the user already
        // knows they're in a TUI.
        return "Usage: !<command>  — runs <command> via /bin/sh with no model round trip."
            .to_string();
    }
    let result = run_bang_command(cmd).await;
    format_bang_output(&result)
}
