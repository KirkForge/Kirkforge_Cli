//! `/test` slash command — run the project's test suite via the same
//! `Bash` tool the model uses, parse the output, render a structured
//! pass/fail summary.
//!
//! Review.md gap #9: the "edit → run tests → read failures → fix"
//! loop was entirely manual prompting. `/test` shortens the loop by
//! giving the user a parsed summary with copy-pasteable file:line:col
//! tokens for each failure.
//!
//! The same `Bash` tool is used (so approval gate, sandbox,
//! deny_paths all apply uniformly), but the output is parsed and
//! rendered here rather than dumped to the chat as a raw tool
//! result.
//!
//! # Timeout
//!
//! The bash tool's default timeout is 30s, which is wrong for
//! `cargo test`. `/test` overrides it via the `timeout` arg, with
//! [`TEST_DEFAULT_TIMEOUT_SECS`] as the default and
//! [`TEST_MIN_TIMEOUT_SECS`] / [`TEST_MAX_TIMEOUT_SECS`] as the
//! clamp bounds. The user can override with `/test <seconds>`.
//!
//! # v1 scope
//!
//! Cargo only. `detect_test_command` is the single dispatch
//! point for the test command; v2 will add `pytest` / `npm test`
//! / `go test` arms there.

use crate::shared::ToolOutcome;
use crate::tools::Tool;
use crate::tui::app::AppState;
use std::path::Path;

/// Default timeout in seconds when the user types `/test` with
/// no argument. 5 minutes covers most real-world `cargo test`
/// runs (the project's own suite finishes in ~12s; a fresh
/// `cargo build` + tests on a large crate can hit 60–90s).
pub const TEST_DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Minimum timeout the user can request via `/test <N>`. Below
/// 30s, `cargo test` barely has time to spawn the test binary
/// before getting killed.
pub const TEST_MIN_TIMEOUT_SECS: u64 = 30;
/// Maximum timeout. 1 hour is a hard cap — anything longer is
/// almost certainly a hung test that the user should debug with
/// `cargo test --no-run` and a manual run, not a 3-hour `/test`.
pub const TEST_MAX_TIMEOUT_SECS: u64 = 3600;

/// Handle the `/test` slash command. Returns the rendered
/// summary string (or an error message) to be pushed into the
/// chat as a `system` `ConversationEntry`.
///
/// Always async because it `await`s the `Bash::run` call. The
/// caller in `keys.rs::handle_input_key` is already inside an
/// `async fn` (`match cmd` block), so this is a plain `.await`.
pub async fn handle_test_command(args: &str, state: &mut AppState) -> String {
    // Concurrency gate: don't stack test runs, and don't fight
    // the model for input.
    if state.is_generating {
        return "/test: a turn is in progress; wait for the model to finish (or press Ctrl+C) before running tests.".into();
    }
    if state.test_in_progress {
        return "/test: another test run is already in flight.".into();
    }

    let timeout_secs = parse_timeout_arg(args);

    // v1: cargo only. detect_test_command returns the full cargo
    // invocation; later versions branch on cwd contents.
    let cmd = match detect_test_command() {
        Some(c) => c,
        None => return "/test: not a Cargo project (no Cargo.toml in current directory). pytest/npm/go support is a v2 follow-up.".into(),
    };

    state.test_in_progress = true;
    let result = state
        .bash_tool
        .run(serde_json::json!({
            "command": cmd,
            "timeout": timeout_secs,
            "workdir": ".",
        }))
        .await;
    state.test_in_progress = false;

    let (raw_stdout, raw_stderr, exit_code) = match result {
        ToolOutcome::Success { content } => (content, String::new(), 0),
        ToolOutcome::Error { message } => parse_bash_error(&message),
        _ => return "/test: unexpected tool outcome from Bash.".into(),
    };

    let summary = super::test_parse::parse_cargo_test_output(&raw_stdout);
    super::test_parse::format_test_summary(&summary, cmd, exit_code, &raw_stderr)
}

/// v1: always return `cargo test --no-fail-fast` when a
/// `Cargo.toml` exists in the cwd. The `--no-fail-fast` flag
/// means all failures are reported, not just the first — the
/// whole point of `/test` is to give the user the full failure
/// list, not stop at the first.
///
/// Returns `None` when no `Cargo.toml` is present. Future
/// versions will fall through to `pytest` / `npm test` / `go test`
/// in a single match arm.
fn detect_test_command() -> Option<&'static str> {
    if Path::new("Cargo.toml").exists() {
        Some("cargo test --no-fail-fast")
    } else {
        None
    }
}

/// Parse the optional `/test <timeout-secs>` argument. Empty /
/// unparseable args return [`TEST_DEFAULT_TIMEOUT_SECS`];
/// out-of-range values are clamped to
/// [`TEST_MIN_TIMEOUT_SECS`, `TEST_MAX_TIMEOUT_SECS`].
pub fn parse_timeout_arg(args: &str) -> u64 {
    args.trim()
        .parse::<u64>()
        .ok()
        .map(|n| n.clamp(TEST_MIN_TIMEOUT_SECS, TEST_MAX_TIMEOUT_SECS))
        .unwrap_or(TEST_DEFAULT_TIMEOUT_SECS)
}

/// Splits the bash tool's error-message format
/// `"Command exited with code {N}{stderr_block_if_any}\nstdout:\n{stdout}"`
/// into the `(stdout, stderr, exit_code)` triple the formatter
/// needs. The format is produced by `src/tools/bash.rs:225-232`.
///
/// Stderr is in the message only when non-empty; when stderr is
/// empty the message is `"Command exited with code {N}\nstdout:\n{stdout}"`.
fn parse_bash_error(msg: &str) -> (String, String, i32) {
    // Strip "Command exited with code " prefix.
    let after_prefix = match msg.strip_prefix("Command exited with code ") {
        Some(s) => s,
        None => return (String::new(), String::new(), -1),
    };

    // The exit code is the first integer at the start of the
    // remaining string. cargo test may have a 3-digit code, so
    // parse greedily until a non-digit character.
    let mut digits = String::new();
    let mut after_code = after_prefix;
    for c in after_code.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            break;
        }
    }
    after_code = &after_code[digits.len()..];
    let exit_code: i32 = digits.parse().unwrap_or(-1);

    // After the exit code, the format is either:
    //   "\nstdout:\n<…>"           (no stderr)
    //   "\nstderr:\n<…>\nstdout:\n<…>"   (with stderr)
    let (stderr, stdout) = split_stderr_stdout(after_code);
    (stdout, stderr, exit_code)
}

/// After the exit code, splits the rest of the bash error
/// message into `(stderr, stdout)`. Defensive against the order
/// being swapped (shouldn't happen, but the input is from a
/// different module's string formatting).
fn split_stderr_stdout(rest: &str) -> (String, String) {
    if let Some(after_stderr) = rest.strip_prefix("\nstderr:\n") {
        // Find the "stdout:" marker that follows.
        if let Some(idx) = after_stderr.find("\nstdout:\n") {
            let stderr = after_stderr[..idx].to_string();
            let stdout = after_stderr[idx + "\nstdout:\n".len()..].to_string();
            return (stderr, stdout);
        }
        // No stdout marker found — treat the whole tail as
        // stderr (degenerate but doesn't lose data).
        return (after_stderr.to_string(), String::new());
    }
    if let Some(after_stdout) = rest.strip_prefix("\nstdout:\n") {
        return (String::new(), after_stdout.to_string());
    }
    // No recognizable marker — treat the whole rest as stdout
    // (matches the no-stderr path of the bash tool).
    (String::new(), rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_timeout_arg_default_on_empty() {
        assert_eq!(parse_timeout_arg(""), TEST_DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_arg_default_on_whitespace() {
        assert_eq!(parse_timeout_arg("   \t  "), TEST_DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_arg_default_on_unparseable() {
        assert_eq!(parse_timeout_arg("not a number"), TEST_DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_arg_in_range() {
        assert_eq!(parse_timeout_arg("60"), 60);
        assert_eq!(parse_timeout_arg("300"), 300);
        assert_eq!(parse_timeout_arg("1800"), 1800);
    }

    #[test]
    fn test_parse_timeout_arg_clamped_low() {
        assert_eq!(parse_timeout_arg("0"), TEST_MIN_TIMEOUT_SECS);
        assert_eq!(parse_timeout_arg("5"), TEST_MIN_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_timeout_arg_clamped_high() {
        assert_eq!(parse_timeout_arg("10000"), TEST_MAX_TIMEOUT_SECS);
        assert_eq!(parse_timeout_arg("99999"), TEST_MAX_TIMEOUT_SECS);
    }

    #[test]
    fn test_parse_bash_error_with_stderr() {
        // Mirrors the format produced by src/tools/bash.rs:225-232
        // when both stdout and stderr are non-empty.
        let msg = "Command exited with code 101\nstderr:\nwarning: unused variable\nstdout:\ntest result: FAILED. 1 passed; 1 failed";
        let (stdout, stderr, code) = parse_bash_error(msg);
        assert_eq!(code, 101);
        assert_eq!(stdout, "test result: FAILED. 1 passed; 1 failed");
        assert_eq!(stderr, "warning: unused variable");
    }

    #[test]
    fn test_parse_bash_error_no_stderr() {
        // Mirrors the format when stderr is empty (the bash tool
        // omits the "\nstderr:\n…" block in that case).
        let msg = "Command exited with code 0\nstdout:\ntest result: ok. 5 passed; 0 failed";
        let (stdout, stderr, code) = parse_bash_error(msg);
        assert_eq!(code, 0);
        assert_eq!(stdout, "test result: ok. 5 passed; 0 failed");
        assert_eq!(stderr, "");
    }

    #[test]
    fn test_detect_test_command_present() {
        // Use a tempdir so this test is independent of the
        // process cwd. (Other tests in the suite — notably
        // `src/session/undo.rs` — change cwd for their own
        // setup, and a test that asserts on cwd-relative
        // paths becomes flaky when run in parallel.)
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("write");
        let prev = std::env::current_dir().ok();
        std::env::set_current_dir(dir.path()).expect("set cwd");
        let cmd = detect_test_command();
        // Restore the previous cwd so the rest of the suite
        // isn't disturbed.
        if let Some(p) = prev {
            let _ = std::env::set_current_dir(&p);
        }
        assert!(cmd.is_some(), "expected cargo test command in cwd");
        assert!(cmd.unwrap().contains("cargo test"));
    }

    #[test]
    fn test_detect_test_command_absent() {
        // Empty tempdir → no Cargo.toml → expect None.
        let dir = tempfile::tempdir().expect("tempdir");
        let prev = std::env::current_dir().ok();
        std::env::set_current_dir(dir.path()).expect("set cwd");
        let cmd = detect_test_command();
        if let Some(p) = prev {
            let _ = std::env::set_current_dir(&p);
        }
        assert!(cmd.is_none(), "expected None in non-cargo dir");
    }
}
