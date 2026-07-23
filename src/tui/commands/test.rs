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

use crate::session::bash_runner::run_shell;
use crate::tui::app::AppState;
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let cmd = match detect_test_command(&cwd) {
        Some(c) => c,
        None => return "/test: not a Cargo project (no Cargo.toml in current directory). pytest/npm/go support is a v2 follow-up.".into(),
    };

    // Build access control from the live config so /test is subject to
    // the same metadata blocks, deny lists, sandbox containment, and
    // dangerous-pattern checks as the model's `bash` tool. Going through
    // `Bash::run` directly would skip the executor's safety gate.
    let (deny_list, path_guard, _) = crate::session::access::access_from_config(
        &crate::shared::read_shared_config(&state.config),
    );

    let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let workdir = if crate::shared::read_shared_config(&state.config)
        .security
        .bash_sandbox_workdir
    {
        path_guard
            .sandbox_dir
            .as_deref()
            .unwrap_or(current_dir.as_path())
    } else {
        current_dir.as_path()
    };
    let workdir_str = workdir.to_string_lossy().to_string();

    if let Some(reason) = crate::session::bash_runner::check_bash_command_str(
        cmd,
        Some(&workdir_str),
        &deny_list,
        &path_guard,
        crate::shared::read_shared_config(&state.config)
            .security
            .bash_sandbox_workdir,
    ) {
        return format!("🔒 /test blocked: {reason}");
    }

    state.test_in_progress = true;
    let result = run_shell(cmd, workdir, timeout_secs).await;
    state.test_in_progress = false;

    let (raw_stdout, raw_stderr, exit_code) = match result {
        Ok(out) => (out.stdout, out.stderr, out.status.code().unwrap_or(-1)),
        Err(e) => return format!("❌ /test failed to run: {e}"),
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
fn detect_test_command(dir: &std::path::Path) -> Option<&'static str> {
    if dir.join("Cargo.toml").exists() {
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
    fn test_detect_test_command_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("write");
        let cmd = detect_test_command(dir.path());
        assert!(
            cmd.is_some(),
            "expected cargo test command in dir with Cargo.toml"
        );
        assert!(cmd.unwrap().contains("cargo test"));
    }

    #[test]
    fn test_detect_test_command_absent() {
        // Empty tempdir → no Cargo.toml → expect None.
        let dir = tempfile::tempdir().expect("tempdir");
        let cmd = detect_test_command(dir.path());
        assert!(cmd.is_none(), "expected None in non-cargo dir");
    }
}
