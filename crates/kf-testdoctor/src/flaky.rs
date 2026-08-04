//! Flaky-test detection (WO 12.5).
//!
//! Runs a single test (or filter) N times via `cargo test -- <filter>
//! --exact` and reports the pass/fail rate. A test with <100% pass rate
//! is flaky; the report keeps the failure messages from the failing
//! runs.
//!
//! This is a developer tool — it is slow (N × test time) and is NOT
//! run in CI. Invoke it manually when investigating a flake:
//! `kf-code doctor flaky --runs 10 --filter <test_name>`.

use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct FlakyReport {
    pub filter: String,
    pub runs: u32,
    pub passes: u32,
    pub failures: u32,
    /// Fraction of runs that passed, in 0.0..=1.0.
    pub pass_rate: f64,
    pub failure_messages: Vec<String>,
    /// `true` when at least one run passed AND at least one failed.
    /// A test that fails 10/10 is consistently failing, not flaky.
    pub flaky: bool,
}

/// Run `cargo test -- <filter> --exact` `runs` times and report the
/// pass/fail rate. The actual `cargo test` invocation is delegated to
/// `runner` so the counting logic is unit-testable without invoking
/// cargo.
pub fn detect_flaky(test_filter: &str, runs: u32) -> Result<FlakyReport> {
    detect_flaky_with_runner(test_filter, runs, run_cargo_test_once)
}

/// Testable core: takes a runner that returns `(success, stderr)` for
/// a single run. A run whose `cargo test` exits 0 is a pass; a non-zero
/// exit is a fail (the stderr holds the failure message).
pub fn detect_flaky_with_runner(
    test_filter: &str,
    runs: u32,
    runner: impl Fn(&str) -> Result<(bool, String)>,
) -> Result<FlakyReport> {
    let mut passes = 0u32;
    let mut failures = 0u32;
    let mut failure_messages = Vec::new();

    for _ in 0..runs {
        let (success, stderr) = runner(test_filter)?;
        if success {
            passes += 1;
        } else {
            failures += 1;
            let msg = extract_failure_message(&stderr);
            if !msg.is_empty() {
                failure_messages.push(msg);
            }
        }
    }

    let pass_rate = if runs == 0 {
        0.0
    } else {
        passes as f64 / runs as f64
    };
    let flaky = passes > 0 && failures > 0;

    Ok(FlakyReport {
        filter: test_filter.to_string(),
        runs,
        passes,
        failures,
        pass_rate,
        failure_messages,
        flaky,
    })
}

fn run_cargo_test_once(test_filter: &str) -> Result<(bool, String)> {
    let output = Command::new("cargo")
        .args(["test", "--", test_filter, "--exact"])
        .output()
        .context("failed to spawn `cargo test`")?;
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((output.status.success(), stderr))
}

/// Pull the first useful failure line out of `cargo test` stderr. The
/// standard libtest harness prints `test <name> ... FAILED` followed by
/// a `---- <name> stdout ----` block and a panic message. We keep the
/// first `FAILED` line plus the next non-blank line that looks like a
/// panic/assertion message.
fn extract_failure_message(stderr: &str) -> String {
    let mut lines = stderr.lines();
    while let Some(line) = lines.next() {
        if line.contains("FAILED") {
            let mut out = line.trim().to_string();
            for next in lines.by_ref().take(20) {
                let t = next.trim();
                if t.starts_with("assert") || t.starts_with("panicked") || t.contains("assertion") {
                    out.push('\n');
                    out.push_str(t);
                    break;
                }
            }
            return out;
        }
    }
    stderr.lines().next().unwrap_or("").trim().to_string()
}

pub fn print_report(report: &FlakyReport) {
    println!(
        "flaky report for `{}` — {} runs, {} passed, {} failed (pass rate {:.0}%)",
        report.filter,
        report.runs,
        report.passes,
        report.failures,
        report.pass_rate * 100.0
    );
    if report.flaky {
        println!("FLAKY: this test passes intermittently.");
    } else if report.failures == 0 {
        println!("stable: all runs passed.");
    } else {
        println!("consistently failing: all runs failed (not flaky — this is a real failure).");
    }
    if !report.failure_messages.is_empty() {
        println!("\nfailure messages:");
        for (i, m) in report.failure_messages.iter().enumerate() {
            println!("  [run {}] {m}", i + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner_pass(_filter: &str) -> Result<(bool, String)> {
        Ok((true, String::new()))
    }

    fn runner_fail(_filter: &str) -> Result<(bool, String)> {
        Ok((
            false,
            "test foo::bar ... FAILED\nassertion `left == right` failed\n".to_string(),
        ))
    }

    fn runner_n(n: u32) -> impl Fn(&str) -> Result<(bool, String)> {
        let i = std::cell::Cell::new(0u32);
        move |_filter| {
            let cur = i.get();
            i.set(cur + 1);
            if cur < n {
                Ok((true, String::new()))
            } else {
                Ok((
                    false,
                    "test foo::bar ... FAILED\nassertion failed".to_string(),
                ))
            }
        }
    }

    #[test]
    fn all_pass_is_not_flaky() {
        let report = detect_flaky_with_runner("foo::bar", 10, runner_pass).unwrap();
        assert_eq!(report.passes, 10);
        assert_eq!(report.failures, 0);
        assert!(!report.flaky);
        assert_eq!(report.pass_rate, 1.0);
        assert!(report.failure_messages.is_empty());
    }

    #[test]
    fn eight_of_ten_pass_is_flaky() {
        let report = detect_flaky_with_runner("foo::bar", 10, runner_n(8)).unwrap();
        assert_eq!(report.passes, 8);
        assert_eq!(report.failures, 2);
        assert!(report.flaky);
        assert_eq!(report.pass_rate, 0.8);
        assert_eq!(report.failure_messages.len(), 2);
        assert!(report.failure_messages[0].contains("FAILED"));
    }

    #[test]
    fn zero_of_ten_pass_is_not_flaky() {
        let report = detect_flaky_with_runner("foo::bar", 10, runner_fail).unwrap();
        assert_eq!(report.passes, 0);
        assert_eq!(report.failures, 10);
        assert!(!report.flaky);
        assert_eq!(report.pass_rate, 0.0);
        assert_eq!(report.failure_messages.len(), 10);
    }

    #[test]
    fn zero_runs_is_empty_report() {
        let report = detect_flaky_with_runner("foo::bar", 0, runner_pass).unwrap();
        assert_eq!(report.runs, 0);
        assert_eq!(report.passes, 0);
        assert!(!report.flaky);
        assert_eq!(report.pass_rate, 0.0);
    }

    #[test]
    fn extract_failure_message_finds_assert() {
        let stderr = "running 1 test\ntest foo::bar ... FAILED\n\n---- foo::bar stdout ----\nassertion `left == right` failed\n";
        let msg = extract_failure_message(stderr);
        assert!(msg.contains("FAILED"));
        assert!(msg.contains("assertion"));
    }

    #[test]
    fn extract_failure_message_falls_back_to_first_line() {
        let msg = extract_failure_message("no failed marker here");
        assert_eq!(msg, "no failed marker here");
    }
}
