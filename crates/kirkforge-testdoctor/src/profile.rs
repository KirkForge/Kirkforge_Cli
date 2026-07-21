//! Profile the test suite by shelling out to `cargo test`.
//!
//! On stable Rust (1.88+) the `--format json` test output requires the
//! nightly compiler, so we parse the standard text output instead:
//!
//! ```text
//!      Running unittests src/lib.rs (target/debug/deps/kirkforge-<hash>)
//!  running 1287 tests
//!  test adapters::anthropic::tests::body_hoists_system_messages ... ok
//!  ...
//!  test result: ok. 1285 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 43.14s
//! ```
//!
//! We capture the per-binary `test result:` line (which includes the
//! `finished in X.XXs` total) and the `Running <binary>` header that
//! precedes it. This gives us per-binary timings on stable Rust without
//! any nightly-only flag.

use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryProfile {
    /// Human-readable name, e.g. `kirkforge (lib)` or `integration_test`.
    pub binary: String,
    /// Suite kind: `lib`, `tests/<name>.rs`, `benches/<name>`.
    pub suite: String,
    /// Wall-clock duration of this binary's test run, in milliseconds.
    pub duration_ms: u64,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub binaries: Vec<BinaryProfile>,
    /// Sum of per-binary durations (may exceed wall time when tests run
    /// in parallel across binaries).
    pub total_duration_ms: u64,
    /// Wall-clock time of the whole `cargo test` invocation.
    pub wall_time_ms: u64,
}

pub fn load(path: &str) -> Result<Profile> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!("failed to read profile at {path} (run `kirkforge-testdoctor profile` first)")
    })?;
    let profile: Profile = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse profile JSON at {path}"))?;
    Ok(profile)
}

pub fn run(out: &str) -> Result<()> {
    let start = Instant::now();
    let output = Command::new("cargo")
        .args([
            "test",
            "--workspace",
            "--no-fail-fast",
            "--",
            "--test-threads=1",
        ])
        .output()
        .context("failed to spawn `cargo test`")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // `cargo test` exits non-zero on test failure, but we still want
    // the profile. Only abort on spawn / IO errors.
    if !output.status.success() && stdout.is_empty() && stderr.is_empty() {
        anyhow::bail!("`cargo test` produced no output (status {})", output.status);
    }

    let combined = format!("{stdout}\n{stderr}");
    let binaries = parse_cargo_test_output(&combined);
    if binaries.is_empty() {
        anyhow::bail!(
            "parsed no binaries from `cargo test` output — first 500 bytes:\n{}",
            combined.chars().take(500).collect::<String>()
        );
    }
    let total_duration_ms: u64 = binaries.iter().map(|b| b.duration_ms).sum();
    let wall_time_ms = start.elapsed().as_millis() as u64;

    let profile = Profile {
        binaries,
        total_duration_ms,
        wall_time_ms,
    };
    let json = serde_json::to_string_pretty(&profile)?;
    std::fs::write(out, &json).with_context(|| format!("failed to write profile to {out}"))?;
    eprintln!(
        "profiled {} binaries in {}ms ({}ms total test time) → {out}",
        profile.binaries.len(),
        wall_time_ms,
        total_duration_ms
    );
    Ok(())
}

/// Parse the text output of `cargo test --workspace --no-fail-fast`.
///
/// The output is a sequence of blocks, one per test binary:
///
/// ```text
///      Running unittests src/lib.rs (target/debug/deps/kirkforge-<hash>)
///  running 1287 tests
///  test foo::bar ... ok
///  ...
///  test result: ok. 1285 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 43.14s
/// ```
///
/// We pair each `Running` header with the next `test result:` summary.
fn parse_cargo_test_output(text: &str) -> Vec<BinaryProfile> {
    let mut out = Vec::new();
    let mut current_binary: Option<String> = None;
    let mut current_suite: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("Running ") {
            // New binary block starts. Save the previous one if it has no
            // result line yet (shouldn't happen, but be defensive).
            current_binary = Some(parse_binary_name(rest));
            current_suite = Some(parse_suite_kind(rest));
            continue;
        }
        if let Some(result) = trimmed.strip_prefix("test result:") {
            let bin = current_binary.take().unwrap_or_else(|| "<unknown>".into());
            let suite = current_suite.take().unwrap_or_else(|| "unknown".into());
            if let Some(parsed) = parse_result_line(result, &bin, &suite) {
                out.push(parsed);
            }
        }
    }
    out
}

/// Extract a human-readable binary name from a `Running <...>` line.
///
/// Examples:
///   `unittests src/lib.rs (target/debug/deps/kirkforge-<hash>)` → `kirkforge`
///   `tests/integration_test.rs (target/debug/deps/integration_test-<hash>)` → `integration_test`
fn parse_binary_name(rest: &str) -> String {
    // `rest` looks like: `unittests src/lib.rs (target/debug/deps/<name>-<hash>)`
    // We want the final path component inside the parens, with the trailing
    // `-<hash>` stripped.
    if let Some(start) = rest.find('(') {
        let after = &rest[start + 1..];
        if let Some(end) = after.find(')') {
            let path = &after[..end];
            // Final component is everything after the last `/`.
            let last = path.rsplit('/').next().unwrap_or(path);
            // Strip the trailing `-<hash>` (last dash in the component).
            if let Some(dash) = last.rfind('-') {
                return last[..dash].to_string();
            }
            return last.to_string();
        }
    }
    // Fallback: use the source path.
    let mut tokens = rest.split_whitespace();
    let _kind = tokens.next();
    if let Some(path) = tokens.next() {
        return path.to_string();
    }
    rest.to_string()
}

/// Classify the suite as `lib`, `tests/<name>.rs`, or `benches/<name>`.
fn parse_suite_kind(rest: &str) -> String {
    if rest.starts_with("unittests") {
        "lib".to_string()
    } else if let Some(start) = rest.find("tests/") {
        let after = &rest[start..];
        let end = after.find(' ').unwrap_or(after.len());
        after[..end].to_string()
    } else if let Some(start) = rest.find("benches/") {
        let after = &rest[start..];
        let end = after.find(' ').unwrap_or(after.len());
        after[..end].to_string()
    } else {
        "unknown".to_string()
    }
}

/// Parse a `test result: ok. 1285 passed; 0 failed; 2 ignored; ...` line.
fn parse_result_line(line: &str, binary: &str, suite: &str) -> Option<BinaryProfile> {
    let mut passed = 0u64;
    let mut failed = 0u64;
    let mut ignored = 0u64;
    let mut duration_ms = 0u64;

    // The line starts with a status word like `ok.` or `FAILED.` followed
    // by the counts. Drop the leading status so `extract_count` sees
    // `<n> <key>` tokens.
    let line = line.trim_start();
    let stripped = if let Some(space) = line.find(' ') {
        // Only strip if the first token looks like a status (`ok.`, `FAILED.`,
        // etc.) — i.e. it does not start with a digit.
        let first = &line[..space];
        if first.parse::<u64>().is_err() {
            &line[space + 1..]
        } else {
            line
        }
    } else {
        line
    };

    for token in stripped.split(';') {
        let token = token.trim();
        if let Some(n) = extract_count(token, "passed") {
            passed = n;
        } else if let Some(n) = extract_count(token, "failed") {
            failed = n;
        } else if let Some(n) = extract_count(token, "ignored") {
            ignored = n;
        } else if let Some(s) = extract_duration_seconds(token) {
            duration_ms = (s * 1000.0).round() as u64;
        }
    }

    Some(BinaryProfile {
        binary: binary.to_string(),
        suite: suite.to_string(),
        duration_ms,
        passed,
        failed,
        ignored,
    })
}

fn extract_count(token: &str, key: &str) -> Option<u64> {
    // `1285 passed` or `0 passed`
    let mut parts = token.split_whitespace();
    let n = parts.next()?.parse::<u64>().ok()?;
    let k = parts.next()?;
    if k == key {
        Some(n)
    } else {
        None
    }
}

fn extract_duration_seconds(token: &str) -> Option<f64> {
    // `finished in 43.14s`
    let trimmed = token.strip_prefix("finished in")?;
    let s = trimmed.trim().trim_end_matches('s');
    s.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_typical_lib_block() {
        let text = "\
     Running unittests src/lib.rs (target/debug/deps/kirkforge-7e7205b290ba3b36)
running 1287 tests
test adapters::anthropic::tests::body_hoists_system_messages ... ok
test result: ok. 1285 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 43.14s
";
        let bins = parse_cargo_test_output(text);
        assert_eq!(bins.len(), 1);
        let b = &bins[0];
        assert_eq!(b.binary, "kirkforge");
        assert_eq!(b.suite, "lib");
        assert_eq!(b.passed, 1285);
        assert_eq!(b.failed, 0);
        assert_eq!(b.ignored, 2);
        assert_eq!(b.duration_ms, 43140);
    }

    #[test]
    fn parse_integration_test_block() {
        let text = "\
     Running tests/integration_test.rs (target/debug/deps/integration_test-abc123)
running 14 tests
test result: ok. 14 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 38.00s
";
        let bins = parse_cargo_test_output(text);
        assert_eq!(bins.len(), 1);
        let b = &bins[0];
        assert_eq!(b.binary, "integration_test");
        assert_eq!(b.suite, "tests/integration_test.rs");
        assert_eq!(b.duration_ms, 38000);
    }

    #[test]
    fn parse_multiple_binaries() {
        let text = "\
     Running unittests src/lib.rs (target/debug/deps/kirkforge-aaa)
test result: ok. 10 passed; 0 failed; 0 ignored; finished in 1.50s
     Running tests/smoke_test.rs (target/debug/deps/smoke_test-bbb)
test result: ok. 5 passed; 0 failed; 0 ignored; finished in 2.50s
";
        let bins = parse_cargo_test_output(text);
        assert_eq!(bins.len(), 2);
        assert_eq!(bins[0].binary, "kirkforge");
        assert_eq!(bins[0].duration_ms, 1500);
        assert_eq!(bins[1].binary, "smoke_test");
        assert_eq!(bins[1].duration_ms, 2500);
    }

    #[test]
    fn parse_failed_result_line() {
        let line =
            "FAILED. 10 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out; finished in 5.0s";
        let b = parse_result_line(line, "kirkforge", "lib").unwrap();
        assert_eq!(b.passed, 10);
        assert_eq!(b.failed, 2);
        assert_eq!(b.ignored, 1);
        assert_eq!(b.duration_ms, 5000);
    }
}
