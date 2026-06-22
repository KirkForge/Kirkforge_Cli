//! Cargo test output parser. Pure functions; no I/O.
//!
//! Two public items:
//! - [`TestRunSummary`] + [`TestFailure`] (the AST)
//! - [`parse_cargo_test_output`] (stdout → summary)
//! - [`format_test_summary`] (summary + cmd + exit code + stderr →
//!   plain-text render string)
//!
//! # Scope
//!
//! v1 parses the canonical `cargo test` output, which has been stable
//! since at least cargo 1.50. Specifically:
//!   - per-test lines: `test foo::bar ... ok` / `... FAILED` / `... ignored`
//!   - the summary line: `test result: ok|FAILED. N passed; M failed; ...`
//!   - failure body markers: `---- foo::bar stdout ----` followed by
//!     a `thread '...' panicked at '<msg>', file:line:col` rustc diagnostic
//!
//! Doctest output, integration test output, and the per-binary
//! `Running unittests src/main.rs (target/debug/...)` banners are
//! tolerated but not specially parsed — they don't change the
//! per-test result line shape and are absorbed into the run
//! counters via the summary line.
//!
//! # Why no regex
//!
//! The four line shapes are simple enough that a hand-rolled
//! parser using `str::splitn` / `strip_prefix` is more readable
//! than four named-capture regexes, has zero external
//! dependencies (the project's `Cargo.toml` doesn't pull in
//! `regex` directly), and is faster (no regex engine startup
//! cost on the first call).

/// Summary of a single `cargo test` invocation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TestRunSummary {
    /// Tests that passed (canonical from the `test result:` line).
    pub passed: usize,
    /// Tests that failed (canonical from the `test result:` line).
    pub failed: usize,
    /// Tests that were marked `#[ignore]`.
    pub ignored: usize,
    /// `passed + failed + ignored` — convenience for "how many tests
    /// ran total" without the user having to add three fields.
    pub total: usize,
    /// Wall-clock duration in seconds. `0.0` when the result line
    /// was missing or used the older `< 0.01s` shorthand (we
    /// approximate that to `0.0` — the user doesn't need to know
    /// the test took 8ms, just that it was fast).
    pub duration_s: f64,
    /// Per-failure detail. Empty when `failed == 0`.
    pub failures: Vec<TestFailure>,
}

/// One failed test, with enough context to jump to the failing
/// assertion from the chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFailure {
    /// The full test path, e.g. `tests::my_test` or
    /// `module::submodule::test_foo`. Taken from the
    /// `test <name> ... FAILED` line.
    pub test_name: String,
    /// Source file, e.g. `src/foo.rs`. `None` when the panic
    /// location regex didn't match (e.g. truncated output, a
    /// custom harness that doesn't print rustc panics).
    pub file: Option<String>,
    /// Line number, 1-based. `None` when not parseable.
    pub line: Option<usize>,
    /// Column number, 1-based. `None` when not parseable.
    pub col: Option<usize>,
    /// The panic message (e.g. "assertion failed: x != y") or
    /// the raw stdout tail if no panic line was found. Never
    /// empty — at minimum it's the test name.
    pub message: String,
}

/// Parse cargo test stdout into a structured summary.
///
/// Tolerant of:
/// - missing summary line (older cargo, suppressed output)
/// - failures with no panic location (truncated output, doctests)
/// - empty input (returns `TestRunSummary::default()`)
///
/// The function is pure — it makes no I/O, spawns no tasks, and
/// is safe to call from any context.
pub fn parse_cargo_test_output(stdout: &str) -> TestRunSummary {
    let mut summary = TestRunSummary::default();
    // Pending failures keyed by test name. The `---- name stdout ----`
    // header, the panic location line, and the message body all
    // arrive on different lines in the cargo output; keeping a
    // map (instead of a single Option<TestFailure>) lets us
    // correctly handle interleaved failures like:
    //
    //   test foo ... FAILED
    //   test bar ... FAILED
    //   ---- foo stdout ----
    //   thread 'foo' panicked at src/foo.rs:1:1:
    //   ---- bar stdout ----
    //   thread 'bar' panicked at src/bar.rs:2:2:
    //
    // With a single Option, the `---- bar ----` line would be
    // treated as a stray (since current_failure still held "foo")
    // and `bar`'s body would be folded into foo's.
    let mut pending: std::collections::HashMap<String, TestFailure> =
        std::collections::HashMap::new();
    // The name of the failure currently receiving body lines.
    // Set by the `---- name ... ----` header; cleared by the
    // `test result:` summary line.
    let mut active_body: Option<String> = None;

    for line in stdout.lines() {
        // Priority 1: per-test result line.
        if let Some(rest) = line.strip_prefix("test ") {
            if let Some((name, status)) = rest.split_once(" ... ") {
                match status {
                    "FAILED" => {
                        // Don't overwrite an existing entry — the
                        // FAILED line and the `---- ... stdout ----`
                        // header both name the same test, so an
                        // entry from the FAILED line just sits
                        // there until the body adds location +
                        // message. If the `---- ... ----` line
                        // never appears (truncated output), the
                        // failure still shows up with just the
                        // name and no location.
                        pending
                            .entry(name.to_string())
                            .or_insert_with(|| TestFailure {
                                test_name: name.to_string(),
                                file: None,
                                line: None,
                                col: None,
                                message: String::new(),
                            });
                    }
                    "ok" | "ignored" => {
                        // Counts come from the summary line.
                    }
                    _ => {}
                }
                continue;
            }
        }

        // Priority 2: failure body header.
        if let Some(rest) = line.strip_prefix("---- ") {
            let name_opt = rest
                .strip_suffix(" stdout ----")
                .or_else(|| rest.strip_suffix(" stderr ----"));
            if let Some(name) = name_opt {
                // Make sure there's a pending failure entry for
                // this name even if we missed the per-test
                // FAILED line (defensive — cargo's output is
                // stable but a tool filtering the stream could
                // break it).
                pending
                    .entry(name.to_string())
                    .or_insert_with(|| TestFailure {
                        test_name: name.to_string(),
                        file: None,
                        line: None,
                        col: None,
                        message: String::new(),
                    });
                active_body = Some(name.to_string());
                continue;
            }
        }

        // Priority 3: panic location. Only meaningful when a
        // body is currently being collected. The location line
        // gives file:line:col; the message is on the next
        // line(s) until the next `---- ... ----` or `test
        // result:` marker (those terminate the active body).
        if let Some(name) = active_body.as_ref() {
            if let Some((file, line_n, col_n)) = parse_panic_location_line(line) {
                if let Some(f) = pending.get_mut(name) {
                    f.file = Some(file);
                    f.line = Some(line_n);
                    f.col = Some(col_n);
                }
                continue;
            }
        }

        // Priority 4: summary line. Source of truth for counts.
        // Also closes all pending failures and clears the
        // active body.
        if let Some(rest) = line.strip_prefix("test result: ") {
            parse_result_line_into(rest, &mut summary);
            active_body = None;
            continue;
        }

        // Fall-through: any line inside an active body is
        // captured into the failure's message field. Lines
        // are joined with `\n` so multi-line panic messages
        // (e.g. `assertion failed: x != y\n  left: 4\n right: 5`)
        // stay readable. The first line replaces an empty
        // message; subsequent lines are appended.
        if let Some(name) = active_body.as_ref() {
            if let Some(f) = pending.get_mut(name) {
                if f.message.is_empty() {
                    f.message = line.to_string();
                } else {
                    f.message.push('\n');
                    f.message.push_str(line);
                }
            }
        }
    }

    // Move pending entries into the summary's failure list,
    // preserving the order in which they were first named
    // (which is the order they appear in the per-test FAILED
    // lines, since we never re-insert a name we've already
    // seen). pending.keys() doesn't preserve insertion order,
    // so we re-derive the order by scanning the original
    // stdout for the first appearance of each name.
    let mut order: Vec<String> = Vec::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("test ") {
            if let Some((name, "FAILED")) = rest.split_once(" ... ") {
                if !order.iter().any(|n| n == name) {
                    order.push(name.to_string());
                }
            }
        }
        if let Some(rest) = line.strip_prefix("---- ") {
            let name_opt = rest
                .strip_suffix(" stdout ----")
                .or_else(|| rest.strip_suffix(" stderr ----"));
            if let Some(name) = name_opt {
                if !order.iter().any(|n| n == name) {
                    order.push(name.to_string());
                }
            }
        }
    }
    for name in order {
        if let Some(mut f) = pending.remove(&name) {
            // If the panic location line was never seen
            // (truncated output, no `---- ... ----` block at
            // all), the body is empty and the message field
            // is empty. Fill in a placeholder so the user
            // sees *something* in the rendered summary.
            if f.message.is_empty() {
                f.message = "(no failure detail captured)".to_string();
            }
            summary.failures.push(f);
        }
    }

    summary
}

/// Try to parse the rustc panic location line:
///
/// ```text
/// thread 'name' panicked at file:line:col:
/// thread 'name' (12345) panicked at file:line:col:
/// ```
///
/// Note: the message (`'msg'`) is on the *next* line, not the
/// same one. Cargo's actual format is two lines:
///
/// ```text
/// thread 'it_breaks' (12345) panicked at src/lib.rs:3:5:
/// assertion `left == right` failed
///   left: 4
///  right: 5
/// ```
///
/// The `parse_cargo_test_output` driver handles the two-line
/// pairing: this function only parses the location line and
/// returns `(file, line, col)`; the next line(s) until the next
/// `---- ... ----` or `test result:` marker become the message.
fn parse_panic_location_line(line: &str) -> Option<(String, usize, usize)> {
    // Strip "thread '" prefix.
    let after_thread = line.strip_prefix("thread '")?;
    // Skip the thread name (until the next single-quote).
    let after_name = after_thread.split_once('\'')?.1;
    // The thread id in parens is optional; strip it if present.
    let after_paren = if let Some(rest) = after_paren_strip(after_name) {
        rest
    } else {
        after_name
    };
    // Strip " panicked at " literal.
    let after_panic = after_paren.strip_prefix(" panicked at ")?;
    // Strip the trailing colon.
    let location = after_panic.strip_suffix(':')?;
    // Split from the right so files with colons (Windows `C:\...`)
    // work. rsplit_once yields (file:line, col); rsplit once more
    // yields (file, line).
    let (file_line, col_str) = location.rsplit_once(':')?;
    let (file, line_str) = file_line.rsplit_once(':')?;
    let line_n: usize = line_str.parse().ok()?;
    let col_n: usize = col_str.parse().ok()?;
    Some((file.to_string(), line_n, col_n))
}

/// If `s` starts with `" ("`, strip the `(...)` parenthesized
/// segment and return the remainder. Else return `None`.
/// Helper for the optional thread-id in panic lines.
fn after_paren_strip(s: &str) -> Option<&str> {
    let after_open = s.strip_prefix(" (")?;
    let after_close = after_open.split_once(')')?.1;
    Some(after_close)
}

/// Parse the part of the summary line *after* the `test result: ` prefix,
/// in-place into the summary. Returns true if the line was a recognized
/// summary line, false if it was something else (defensive — the caller
/// already checked the prefix).
fn parse_result_line_into(rest: &str, summary: &mut TestRunSummary) {
    // Expected shape:
    //   "ok. 47 passed[; 0 failed][; 1 ignored][; 0 measured][; 0 filtered out][; finished in 12.34s]"
    // or:
    //   "FAILED. 47 passed[; 0 failed][; 1 ignored][; 0 measured][; 0 filtered out][; finished in 12.34s]"
    // The status word is informational; we trust the per-field numbers
    // over it (defensive against "ok" with a non-zero failed count,
    // which cargo won't emit, but the input is untrusted).
    let Some((_status, after_status)) = rest.split_once(". ") else {
        return;
    };
    for segment in after_status.split(';') {
        let segment = segment.trim();
        // "47 passed"
        if let Some(n) = segment.strip_suffix(" passed") {
            summary.passed = n.trim().parse().unwrap_or(0);
            continue;
        }
        // "0 failed"
        if let Some(n) = segment.strip_suffix(" failed") {
            summary.failed = n.trim().parse().unwrap_or(0);
            continue;
        }
        // "1 ignored"
        if let Some(n) = segment.strip_suffix(" ignored") {
            summary.ignored = n.trim().parse().unwrap_or(0);
            continue;
        }
        // "finished in 12.34s"
        if let Some(n) = segment.strip_prefix("finished in ") {
            if let Some(stripped) = n.strip_suffix('s') {
                summary.duration_s = stripped.trim().parse().unwrap_or(0.0);
            }
            continue;
        }
        // "< 0.01s" (older cargo)
        if let Some(n) = segment.strip_prefix("< ") {
            if let Some(stripped) = n.strip_suffix('s') {
                summary.duration_s = stripped.trim().parse().unwrap_or(0.0);
            }
            continue;
        }
        // "0 measured" / "0 filtered out" — ignored (not stored in AST).
        let _ = segment;
    }
    summary.total = summary.passed + summary.failed + summary.ignored;
}

/// Render a `TestRunSummary` into a copy-pasteable plain-text
/// block. The output is the string pushed into the TUI chat as
/// a `system` message. Layout:
///
/// ```text
/// $ <cmd>
/// test result: <ok|FAILED>. <P> passed; <F> failed; <I> ignored; finished in <D>s
///
/// FAIL <name>
///   <file>:<line>:<col> — <message>
/// FAIL <name>
///   <file>:<line>:<col> — <message>
/// ```
///
/// The leading two spaces on the failure body line give the
/// failure indent in the chat pane AND make the `file:line:col`
/// line easy to select with the mouse.
///
/// Stderr is appended at the end (rare — usually empty for
/// cargo test). Exit code is rendered as a `$? N` annotation
/// when non-zero.
pub fn format_test_summary(s: &TestRunSummary, cmd: &str, exit_code: i32, stderr: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "$ {}", cmd);

    let status = if s.failed == 0 { "ok" } else { "FAILED" };
    let _ = writeln!(
        out,
        "test result: {}. {} passed; {} failed; {} ignored; finished in {:.2}s",
        status, s.passed, s.failed, s.ignored, s.duration_s
    );

    if exit_code != 0 {
        let _ = writeln!(out, "(exit code {})", exit_code);
    }

    for f in &s.failures {
        let _ = writeln!(out);
        let _ = writeln!(out, "FAIL {}", f.test_name);
        match (&f.file, f.line, f.col) {
            (Some(file), Some(line), Some(col)) => {
                let _ = writeln!(out, "  {}:{}:{} — {}", file, line, col, f.message);
            }
            (Some(file), Some(line), None) => {
                let _ = writeln!(out, "  {}:{} — {}", file, line, f.message);
            }
            (Some(file), None, None) => {
                let _ = writeln!(out, "  {} — {}", file, f.message);
            }
            _ => {
                let _ = writeln!(out, "  {}", f.message);
            }
        }
    }

    if !stderr.trim().is_empty() {
        let _ = writeln!(out, "\nstderr:\n{}", stderr.trim_end());
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cargo_test_output_all_passing() {
        let stdout = r#"
running 48 tests
test tests::a ... ok
test tests::b ... ok
test tests::c ... ok
test result: ok. 47 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 12.34s
"#;
        let s = parse_cargo_test_output(stdout);
        assert_eq!(s.passed, 47);
        assert_eq!(s.failed, 0);
        assert_eq!(s.ignored, 1);
        assert_eq!(s.total, 48);
        assert!((s.duration_s - 12.34).abs() < 0.001);
        assert!(s.failures.is_empty());
    }

    #[test]
    fn test_parse_cargo_test_output_with_failures() {
        // Real cargo output puts the panic location and the
        // message on TWO lines. The "---- name stdout ----"
        // header opens the block, then:
        //   line 1: "thread 'name' [(id)] panicked at file:line:col:"
        //   line 2+: the actual message (which can be multi-line)
        let stdout = r#"
running 7 tests
test tests::test_foo ... FAILED
test tests::test_bar ... ok
test tests::test_baz ... FAILED

failures:

---- tests::test_foo stdout ----
thread 'tests::test_foo' (123) panicked at src/foo.rs:42:5:
assertion failed: x != y
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- tests::test_baz stdout ----
thread 'tests::test_baz' (456) panicked at src/bar.rs:88:13:
index out of bounds: the len is 3 but the index is 9

test result: FAILED. 5 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.45s
"#;
        let s = parse_cargo_test_output(stdout);
        assert_eq!(s.passed, 5);
        assert_eq!(s.failed, 2);
        assert_eq!(s.ignored, 0);
        assert_eq!(s.total, 7);
        assert_eq!(s.failures.len(), 2);

        let foo = &s.failures[0];
        assert_eq!(foo.test_name, "tests::test_foo");
        assert_eq!(foo.file.as_deref(), Some("src/foo.rs"));
        assert_eq!(foo.line, Some(42));
        assert_eq!(foo.col, Some(5));
        assert!(
            foo.message.contains("assertion failed"),
            "expected assertion message in body, got: {:?}",
            foo.message
        );

        let baz = &s.failures[1];
        assert_eq!(baz.test_name, "tests::test_baz");
        assert_eq!(baz.file.as_deref(), Some("src/bar.rs"));
        assert_eq!(baz.line, Some(88));
        assert_eq!(baz.col, Some(13));
        assert!(baz.message.contains("index out of bounds"));
    }

    #[test]
    fn test_parse_cargo_test_output_truncated_no_panic_line() {
        // No panic location line — could be a doctest, a custom
        // harness, or truncated output. The failure should still
        // appear with `file/line/col = None` and a meaningful
        // message (the raw body, since the panic parser didn't
        // match).
        let stdout = r#"
test weird::test ... FAILED

---- weird::test stdout ----
some custom harness output
that doesn't look like a rustc panic
more context
"#;
        let s = parse_cargo_test_output(stdout);
        assert_eq!(s.failures.len(), 1);
        let f = &s.failures[0];
        assert_eq!(f.test_name, "weird::test");
        assert!(f.file.is_none());
        assert!(f.line.is_none());
        assert!(f.col.is_none());
        assert!(f.message.contains("custom harness"));
    }

    #[test]
    fn test_parse_cargo_test_output_empty() {
        let s = parse_cargo_test_output("");
        assert_eq!(s, TestRunSummary::default());
    }

    #[test]
    fn test_parse_cargo_test_output_no_finished_in_line() {
        // Older cargo versions used "< 0.01s" instead of
        // "finished in 0.01s". The parser should not crash and
        // should report duration_s as the value (approximated).
        let stdout = r#"
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; < 0.01s
"#;
        let s = parse_cargo_test_output(stdout);
        assert_eq!(s.passed, 1);
        assert!((s.duration_s - 0.01).abs() < 0.001);
    }

    #[test]
    fn test_parse_panic_line_basic() {
        let line = "thread 'foo' panicked at src/foo.rs:42:5:";
        let (file, line_n, col_n) = parse_panic_location_line(line).unwrap();
        assert_eq!(file, "src/foo.rs");
        assert_eq!(line_n, 42);
        assert_eq!(col_n, 5);
    }

    #[test]
    fn test_parse_panic_line_with_thread_id() {
        // Real cargo output includes the thread id in parens.
        let line = "thread 'it_breaks' (397492) panicked at src/lib.rs:3:5:";
        let (file, line_n, col_n) = parse_panic_location_line(line).unwrap();
        assert_eq!(file, "src/lib.rs");
        assert_eq!(line_n, 3);
        assert_eq!(col_n, 5);
    }

    #[test]
    fn test_parse_panic_line_windows_path() {
        // Windows paths have colons; the rsplit_once in the
        // parser should handle them.
        let line = "thread 't' panicked at C:\\path\\to\\file.rs:10:3:";
        let (file, line_n, col_n) = parse_panic_location_line(line).unwrap();
        assert_eq!(file, "C:\\path\\to\\file.rs");
        assert_eq!(line_n, 10);
        assert_eq!(col_n, 3);
    }

    #[test]
    fn test_format_test_summary_all_passing() {
        let s = TestRunSummary {
            passed: 47,
            failed: 0,
            ignored: 1,
            total: 48,
            duration_s: 12.34,
            failures: vec![],
        };
        let out = format_test_summary(&s, "cargo test --no-fail-fast", 0, "");
        assert!(out.contains("cargo test --no-fail-fast"), "got: {}", out);
        assert!(out.contains("test result: ok."), "got: {}", out);
        assert!(out.contains("47 passed"), "got: {}", out);
        assert!(out.contains("0 failed"), "got: {}", out);
        assert!(out.contains("1 ignored"), "got: {}", out);
        assert!(!out.contains("FAIL "), "should have no failures: {}", out);
        assert!(
            !out.contains("exit code"),
            "exit code 0 should not be shown: {}",
            out
        );
    }

    #[test]
    fn test_format_test_summary_with_failures() {
        let s = TestRunSummary {
            passed: 5,
            failed: 2,
            ignored: 0,
            total: 7,
            duration_s: 0.45,
            failures: vec![
                TestFailure {
                    test_name: "tests::test_foo".into(),
                    file: Some("src/foo.rs".into()),
                    line: Some(42),
                    col: Some(5),
                    message: "assertion failed: x != y".into(),
                },
                TestFailure {
                    test_name: "tests::test_baz".into(),
                    file: Some("src/bar.rs".into()),
                    line: Some(88),
                    col: Some(13),
                    message: "index out of bounds".into(),
                },
            ],
        };
        let out = format_test_summary(&s, "cargo test --no-fail-fast", 101, "");
        assert!(out.contains("test result: FAILED."), "got: {}", out);
        assert!(out.contains("FAIL tests::test_foo"), "got: {}", out);
        assert!(out.contains("src/foo.rs:42:5"), "got: {}", out);
        assert!(out.contains("assertion failed: x != y"), "got: {}", out);
        assert!(out.contains("FAIL tests::test_baz"), "got: {}", out);
        assert!(out.contains("src/bar.rs:88:13"), "got: {}", out);
        assert!(out.contains("index out of bounds"), "got: {}", out);
        assert!(out.contains("(exit code 101)"), "got: {}", out);
    }

    #[test]
    fn test_format_test_summary_with_stderr() {
        let s = TestRunSummary {
            passed: 1,
            failed: 0,
            ignored: 0,
            total: 1,
            duration_s: 0.5,
            failures: vec![],
        };
        let out = format_test_summary(&s, "cargo test", 0, "warning: something\n");
        assert!(out.contains("stderr:\nwarning: something"), "got: {}", out);
    }
}
