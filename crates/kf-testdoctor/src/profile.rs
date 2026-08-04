//! Profile the test suite by shelling out to `cargo test`.
//!
//! On stable Rust (1.88+) the `--format json` test output requires the
//! nightly compiler, so we parse the standard text output instead:
//!
//! ```text
//!      Running unittests src/lib.rs (target/debug/deps/kf_code-<hash>)
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

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryProfile {
    /// Human-readable name, e.g. `kf-code (lib)` or `integration_test`.
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

// Per-test timing (WO 12.5). The nightly JSON path gives a real
// duration per test; the stable fallback gives one duration per
// binary, attributed to each test in that binary as the binary's
// average (`coarse = true`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestProfile {
    pub name: String,
    pub binary: String,
    pub duration_ms: u64,
    pub passed: bool,
    pub ignored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerTestProfile {
    pub tests: Vec<TestProfile>,
    /// Wall-clock time of the whole `cargo test` invocation.
    pub wall_time_ms: u64,
    /// `true` when the durations came from the per-binary fallback
    /// (coarse, not per-test). `false` when the nightly JSON path was
    /// used and each `duration_ms` is a real per-test measurement.
    pub coarse: bool,
}

pub fn load(path: &str) -> Result<Profile> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!("failed to read profile at {path} (run `kf-testdoctor profile` first)")
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

// ---- Per-test timings (WO 12.5) -------------------------------------

/// Detect whether a nightly rustup toolchain is installed. The nightly
/// JSON path (`--format json -Z unstable-options --report-time`) is
/// preferred; the stable fallback is coarse (one duration per binary).
pub fn nightly_available() -> bool {
    let Ok(out) = Command::new("rustup").arg("toolchain").arg("list").output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().any(|l| l.trim_start().starts_with("nightly"))
}

/// Profile per-test timings. Preferred path: nightly JSON
/// (`cargo +nightly test --workspace --no-fail-fast -- --format json
/// -Z unstable-options --report-time`), which gives a real `exec_time`
/// per test. Fallback: per-binary timings attributed to each test in
/// the binary as that binary's average (coarse, not per-test).
///
/// `profile_path` is an optional path to write the JSON report to.
pub fn profile_per_test(profile_path: Option<&Path>) -> Result<PerTestProfile> {
    let start = Instant::now();
    if nightly_available() {
        let output = Command::new("cargo")
            .arg("+nightly")
            .args([
                "test",
                "--workspace",
                "--no-fail-fast",
                "--",
                "--format",
                "json",
                "-Z",
                "unstable-options",
                "--report-time",
            ])
            .output()
            .context("failed to spawn `cargo +nightly test`")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}\n{stderr}");
        let tests = parse_nightly_json(&combined);
        if tests.is_empty() {
            anyhow::bail!(
                "nightly JSON path produced no test events — first 500 bytes:\n{}",
                combined.chars().take(500).collect::<String>()
            );
        }
        let wall_time_ms = start.elapsed().as_millis() as u64;
        let p = PerTestProfile {
            tests,
            wall_time_ms,
            coarse: false,
        };
        if let Some(path) = profile_path {
            let json = serde_json::to_string_pretty(&p)?;
            std::fs::write(path, &json).with_context(|| {
                format!("failed to write per-test profile to {}", path.display())
            })?;
        }
        eprintln!(
            "profiled {} tests via nightly JSON in {}ms → {}",
            p.tests.len(),
            wall_time_ms,
            profile_path
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(stdout)".into())
        );
        return Ok(p);
    }

    // Stable fallback: per-binary timings, attributed to each test in
    // a binary as that binary's average. Coarse, not per-test.
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
    let combined = format!("{stdout}\n{stderr}");
    let binaries = parse_cargo_test_output(&combined);
    if binaries.is_empty() {
        anyhow::bail!(
            "stable fallback produced no binaries — first 500 bytes:\n{}",
            combined.chars().take(500).collect::<String>()
        );
    }
    let tests = per_test_from_binary_profiles(&binaries);
    let wall_time_ms = start.elapsed().as_millis() as u64;
    let p = PerTestProfile {
        tests,
        wall_time_ms,
        coarse: true,
    };
    if let Some(path) = profile_path {
        let json = serde_json::to_string_pretty(&p)?;
        std::fs::write(path, &json)
            .with_context(|| format!("failed to write per-test profile to {}", path.display()))?;
    }
    eprintln!(
        "profiled {} tests via stable fallback (coarse, not per-test) in {}ms → {}",
        p.tests.len(),
        wall_time_ms,
        profile_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(stdout)".into())
    );
    Ok(p)
}

/// Coarse fallback: turn per-binary timings into per-test entries.
/// Each test in a binary gets the binary's average duration. The test
/// names are not available from the text output, so each entry's
/// `name` is a synthetic `<binary>::test_<n>` placeholder.
fn per_test_from_binary_profiles(bins: &[BinaryProfile]) -> Vec<TestProfile> {
    let mut out = Vec::new();
    for b in bins {
        let ran = b.passed + b.failed;
        if ran == 0 {
            continue;
        }
        let avg = b.duration_ms / ran;
        for i in 0..b.passed {
            out.push(TestProfile {
                name: format!("{}::test_{}", b.binary, i),
                binary: b.binary.clone(),
                duration_ms: avg,
                passed: true,
                ignored: false,
            });
        }
        for i in 0..b.failed {
            out.push(TestProfile {
                name: format!("{}::failed_{}", b.binary, i),
                binary: b.binary.clone(),
                duration_ms: avg,
                passed: false,
                ignored: false,
            });
        }
    }
    out
}

/// Parse nightly `cargo test -- --format json -Z unstable-options
/// --report-time` output into per-test profiles.
///
/// The output is one JSON object per line for the test events, plus
/// interleaved `Running <binary>` lines (printed by cargo to stdout/
/// stderr before each binary's JSON block) that name the binary. Each
/// `test` event has an `exec_time` field (float seconds) when the test
/// finished; `started` events have no duration and are skipped.
///
/// Binary tracking: a non-JSON `Running <...>` line sets the current
/// binary; a `suite` `started` event confirms it; subsequent `test`
/// events are attributed to that binary until the next `Running` line.
///
/// This is a pure function (no I/O) so it is unit-testable without
/// invoking cargo.
pub fn parse_nightly_json(text: &str) -> Vec<TestProfile> {
    let mut out = Vec::new();
    let mut current_binary: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        // Non-JSON `Running <binary>` line names the current binary.
        if let Some(rest) = trimmed.strip_prefix("Running ") {
            current_binary = Some(parse_binary_name(rest));
            continue;
        }
        if !trimmed.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("test") {
            continue;
        }
        let event = match v.get("event").and_then(|e| e.as_str()) {
            Some(e) => e,
            None => continue,
        };
        // `started` events carry no duration; skip them.
        if event == "started" {
            continue;
        }
        let name = v
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("<unknown>")
            .to_string();
        let binary = current_binary.clone().unwrap_or_else(|| "<unknown>".into());
        let exec_time = v.get("exec_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
        let duration_ms = (exec_time * 1000.0).round() as u64;
        let (passed, ignored) = match event {
            "ok" => (true, false),
            "ignored" => (false, true),
            // `failed` and any other terminal event.
            _ => (false, false),
        };
        out.push(TestProfile {
            name,
            binary,
            duration_ms,
            passed,
            ignored,
        });
    }
    out
}

/// Parse the text output of `cargo test --workspace --no-fail-fast`.
///
/// The output is a sequence of blocks, one per test binary:
///
/// ```text
///      Running unittests src/lib.rs (target/debug/deps/kf_code-<hash>)
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
///   `unittests src/lib.rs (target/debug/deps/kf_code-<hash>)` → `kf-code`
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
     Running unittests src/lib.rs (target/debug/deps/kf_code-7e7205b290ba3b36)
running 1287 tests
test adapters::anthropic::tests::body_hoists_system_messages ... ok
test result: ok. 1285 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 43.14s
";
        let bins = parse_cargo_test_output(text);
        assert_eq!(bins.len(), 1);
        let b = &bins[0];
        assert_eq!(b.binary, "kf-code");
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
     Running unittests src/lib.rs (target/debug/deps/kf_code-aaa)
test result: ok. 10 passed; 0 failed; 0 ignored; finished in 1.50s
     Running tests/smoke_test.rs (target/debug/deps/smoke_test-bbb)
test result: ok. 5 passed; 0 failed; 0 ignored; finished in 2.50s
";
        let bins = parse_cargo_test_output(text);
        assert_eq!(bins.len(), 2);
        assert_eq!(bins[0].binary, "kf-code");
        assert_eq!(bins[0].duration_ms, 1500);
        assert_eq!(bins[1].binary, "smoke_test");
        assert_eq!(bins[1].duration_ms, 2500);
    }

    #[test]
    fn parse_failed_result_line() {
        let line =
            "FAILED. 10 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out; finished in 5.0s";
        let b = parse_result_line(line, "kf-code", "lib").unwrap();
        assert_eq!(b.passed, 10);
        assert_eq!(b.failed, 2);
        assert_eq!(b.ignored, 1);
        assert_eq!(b.duration_ms, 5000);
    }

    // ---- parse_nightly_json (WO 12.5) ----

    #[test]
    fn parse_nightly_json_ok_event_carries_exec_time() {
        let text = "\
     Running unittests src/lib.rs (target/debug/deps/kf_code-aaa)
{ \"type\": \"suite\", \"event\": \"started\", \"test_count\": 2 }
{ \"type\": \"test\", \"event\": \"started\", \"name\": \"foo::a\" }
{ \"type\": \"test\", \"name\": \"foo::a\", \"event\": \"ok\", \"exec_time\": 0.0235 }
{ \"type\": \"test\", \"name\": \"foo::b\", \"event\": \"ok\", \"exec_time\": 2.3 }
";
        let tests = parse_nightly_json(text);
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].name, "foo::a");
        assert_eq!(tests[0].binary, "kf-code");
        assert_eq!(tests[0].duration_ms, 24);
        assert!(tests[0].passed);
        assert!(!tests[0].ignored);
        assert_eq!(tests[1].name, "foo::b");
        assert_eq!(tests[1].duration_ms, 2300);
    }

    #[test]
    fn parse_nightly_json_failed_and_ignored_events() {
        let text = "\
     Running tests/slow.rs (target/debug/deps/slow-aaa)
{ \"type\": \"test\", \"name\": \"x\", \"event\": \"failed\", \"exec_time\": 1.5 }
{ \"type\": \"test\", \"name\": \"y\", \"event\": \"ignored\" }
{ \"type\": \"test\", \"name\": \"z\", \"event\": \"ok\", \"exec_time\": 0.0 }
";
        let tests = parse_nightly_json(text);
        assert_eq!(tests.len(), 3);
        assert!(!tests[0].passed);
        assert!(!tests[0].ignored);
        assert_eq!(tests[0].duration_ms, 1500);
        assert!(!tests[1].passed);
        assert!(tests[1].ignored);
        assert_eq!(tests[1].duration_ms, 0);
        assert!(tests[2].passed);
    }

    #[test]
    fn parse_nightly_json_attributes_tests_to_their_binary() {
        let text = "\
     Running unittests src/lib.rs (target/debug/deps/kf_code-aaa)
{ \"type\": \"test\", \"name\": \"lib_test\", \"event\": \"ok\", \"exec_time\": 0.1 }
     Running tests/integration_test.rs (target/debug/deps/integration_test-bbb)
{ \"type\": \"test\", \"name\": \"int_test\", \"event\": \"ok\", \"exec_time\": 3.0 }
";
        let tests = parse_nightly_json(text);
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].binary, "kf-code");
        assert_eq!(tests[1].binary, "integration_test");
        assert_eq!(tests[1].duration_ms, 3000);
    }

    #[test]
    fn parse_nightly_json_skips_started_events_and_non_json_lines() {
        let text = "\
some random cargo log line
{ \"type\": \"suite\", \"event\": \"started\", \"test_count\": 1 }
{ \"type\": \"test\", \"event\": \"started\", \"name\": \"a\" }
{ \"type\": \"test\", \"name\": \"a\", \"event\": \"ok\", \"exec_time\": 0.05 }
{ \"type\": \"suite\", \"event\": \"ok\", \"passed\": 1, \"exec_time\": 0.06 }
not json either
";
        let tests = parse_nightly_json(text);
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "a");
        assert_eq!(tests[0].duration_ms, 50);
    }

    #[test]
    fn parse_nightly_json_missing_exec_time_is_zero() {
        let text = "\
     Running unittests src/lib.rs (target/debug/deps/kf_code-aaa)
{ \"type\": \"test\", \"name\": \"no_time\", \"event\": \"ok\" }
";
        let tests = parse_nightly_json(text);
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].duration_ms, 0);
    }

    #[test]
    fn per_test_from_binary_profiles_splits_pass_fail() {
        let bins = vec![
            BinaryProfile {
                binary: "kf-code".into(),
                suite: "lib".into(),
                duration_ms: 1000,
                passed: 4,
                failed: 1,
                ignored: 0,
            },
            BinaryProfile {
                binary: "ignored_only".into(),
                suite: "tests/x.rs".into(),
                duration_ms: 0,
                passed: 0,
                failed: 0,
                ignored: 3,
            },
        ];
        let tests = per_test_from_binary_profiles(&bins);
        // 4 passed + 1 failed from kf-code; ignored-only binary yields none.
        assert_eq!(tests.len(), 5);
        assert_eq!(tests[0].duration_ms, 200);
        assert!(tests[0].passed);
        assert!(!tests[4].passed);
        assert_eq!(tests[4].name, "kf_code::failed_0");
    }
}
