//! Suggest fixes for slow tests.
//!
//! The suggestions are heuristics keyed on the binary name / suite path.
//! They are deliberately conservative — the doctor never rewrites source
//! code; it prints the suggested annotation and the human applies it.

use anyhow::Result;

use crate::classify::{classify, classify_per_test, Speed};
use crate::profile::{load, PerTestProfile, TestProfile};

pub fn run(profile_path: &str) -> Result<()> {
    let profile = load(profile_path)?;
    let class = classify(&profile);
    let slow: Vec<_> = class
        .bins
        .iter()
        .filter(|b| b.speed == Speed::Slow || b.speed == Speed::Ignored)
        .collect();

    if slow.is_empty() {
        println!("no slow or ignored binaries — nothing to suggest.");
        return Ok(());
    }

    println!("suggestions for {} slow/ignored binaries:\n", slow.len());
    for b in slow {
        println!(
            "── {} ({}) — {}ms ──",
            b.profile.binary, b.profile.suite, b.profile.duration_ms
        );
        for s in suggestions_for(&b.profile.binary, &b.profile.suite) {
            println!("  • {s}");
        }
        println!();
    }
    Ok(())
}

/// Print per-test suggestions. When per-test timings are available this
/// names the specific slow test ("`test_foo` takes 2.3s; suggest
/// `#[ignore]` + dedicated job") instead of just the binary. The v1
/// binary-level suggestions remain the fallback when per-test data is
/// absent.
pub fn run_per_test(per: &PerTestProfile) -> Result<()> {
    let class = classify_per_test(per);
    let slow: Vec<_> = class
        .tests
        .iter()
        .filter(|t| t.speed == Speed::Slow)
        .collect();

    if slow.is_empty() {
        println!("no slow tests — nothing to suggest.");
        return Ok(());
    }

    if per.coarse {
        println!(
            "note: per-test timings are coarse (stable fallback) — each test's\n\
             duration is its binary's average, not a per-test measurement.\n"
        );
    }

    println!("suggestions for {} slow tests:\n", slow.len());
    for t in slow {
        println!(
            "── {} ({}::{} — {}ms) ──",
            t.profile.name, t.profile.binary, t.profile.binary, t.profile.duration_ms
        );
        for s in suggestions_for_test(&t.profile) {
            println!("  • {s}");
        }
        println!();
    }
    Ok(())
}

/// Per-test suggestions keyed on the individual test's duration. A test
/// slower than 5000ms gets the strongest suggestion (`#[ignore]` +
/// dedicated job); 2000-5000ms gets a "consider mocking" suggestion.
/// Tests reaching this function are already classified `slow`
/// (`duration_ms > SLOW_TEST_MS = 2000`).
fn suggestions_for_test(t: &TestProfile) -> Vec<String> {
    let mut out = Vec::new();
    let secs = t.duration_ms as f64 / 1000.0;
    if t.duration_ms > 5_000 {
        out.push(format!(
            "`{}` takes {:.1}s; suggest `#[ignore = \"slow: >5s\"]` + run in a dedicated `cargo test -- --ignored` job.",
            t.name, secs
        ));
    } else {
        // 2000ms < duration_ms <= 5000ms: slow enough to mock or split.
        out.push(format!(
            "`{}` takes {:.1}s; consider mocking the slow dependency (subprocess, network, sleep) or splitting the test.",
            t.name, secs
        ));
    }
    out
}

fn suggestions_for(binary: &str, suite: &str) -> Vec<String> {
    let mut out = Vec::new();

    // Integration tests that spawn subprocesses are the common slow case.
    if suite.starts_with("tests/") {
        out.push(
            "Move slow cases behind `#[ignore = \"slow: spawns subprocess\"]` and run \
             them in a dedicated `cargo test -- --ignored` job."
                .to_string(),
        );
        out.push(
            "If the test spawns `cargo` / `docker` / `ollama`, mock the subprocess \
             (factor the command into a trait the test can stub)."
                .to_string(),
        );
    }

    // Tests that wait on time.
    if binary.contains("sleep") || binary.contains("timeout") || binary.contains("wait") {
        out.push(
            "Replace `tokio::time::sleep` with `tokio::time::pause` — the runtime \
             advances virtual time instantly under `#[tokio::test(start_paused = true)]`."
                .to_string(),
        );
    }

    // Tests that hit the network.
    if binary.contains("ollama") || binary.contains("integration") || binary.contains("http") {
        out.push(
            "Use `wiremock` (already a dev-dep) to spin up a local mock server \
             instead of hitting a live Ollama / HTTP endpoint."
                .to_string(),
        );
    }

    // Generic fallback — always present so the output is never empty.
    out.push(
        "If the binary cannot be sped up, exclude it from the coverage suite \
         (`cargo tarpaulin --lib` already skips `tests/*`) and gate it behind \
         the integration job."
            .to_string(),
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integration_test_gets_subprocess_suggestion() {
        let s = suggestions_for("integration_test", "tests/integration_test.rs");
        assert!(s.iter().any(|x| x.contains("#[ignore")));
        assert!(s.iter().any(|x| x.contains("mock the subprocess")));
    }

    #[test]
    fn sleep_test_gets_pause_suggestion() {
        let s = suggestions_for("sleep_test", "tests/sleep_test.rs");
        assert!(s.iter().any(|x| x.contains("tokio::time::pause")));
    }

    #[test]
    fn always_has_fallback() {
        let s = suggestions_for("random_binary", "lib");
        assert!(!s.is_empty());
    }

    fn tp(name: &str, ms: u64) -> TestProfile {
        TestProfile {
            name: name.to_string(),
            binary: "kirkforge".to_string(),
            duration_ms: ms,
            passed: true,
            ignored: false,
        }
    }

    #[test]
    fn per_test_very_slow_gets_ignore_suggestion() {
        let s = suggestions_for_test(&tp("test_foo", 6_000));
        assert!(s.iter().any(|x| x.contains("#[ignore")));
        assert!(s.iter().any(|x| x.contains("test_foo")));
    }

    #[test]
    fn per_test_slow_gets_mocking_suggestion() {
        let s = suggestions_for_test(&tp("test_bar", 3_000));
        assert!(s.iter().any(|x| x.contains("mocking")));
        assert!(s.iter().any(|x| x.contains("test_bar")));
    }

    #[test]
    fn per_test_just_over_threshold_gets_mocking_not_ignore() {
        // 2100ms is slow (>2000) but below the 5000ms ignore threshold.
        let s = suggestions_for_test(&tp("test_baz", 2_100));
        assert!(s.iter().any(|x| x.contains("mocking")));
        assert!(!s.iter().any(|x| x.contains("#[ignore")));
    }
}
