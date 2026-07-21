//! Suggest fixes for slow tests.
//!
//! The suggestions are heuristics keyed on the binary name / suite path.
//! They are deliberately conservative — the doctor never rewrites source
//! code; it prints the suggested annotation and the human applies it.

use anyhow::Result;

use crate::classify::{classify, Speed};
use crate::profile::load;

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
}
