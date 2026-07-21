//! Partition the profiled suite into fast / full / coverage manifests.
//!
//! Each manifest is a JSON file that records the exact `cargo test`
//! (or `cargo nextest`) invocation plus the per-binary allow-list, so
//! CI can run `kirkforge-testdoctor run --suite fast` without
//! re-deriving the partition.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::classify::{classify, Speed};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suite {
    /// Suite name: `fast`, `full`, or `coverage`.
    pub name: String,
    /// Human-readable target for this suite.
    pub target: String,
    /// When to run this suite in CI.
    pub when: String,
    /// The cargo subcommand args (without leading `cargo`).
    pub cargo_args: Vec<String>,
    /// Prefer `cargo nextest run` if available; fall back to `cargo test`.
    pub prefer_nextest: bool,
    /// Binaries included in this suite.
    pub binaries: Vec<SuiteBinary>,
    /// Estimated wall time in milliseconds (sum of member durations).
    pub estimated_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteBinary {
    pub binary: String,
    pub suite: String,
    pub duration_ms: u64,
}

pub fn run(profile_path: &str, out_dir: &str) -> Result<()> {
    let profile = crate::profile::load(profile_path)?;
    let class = classify(&profile);
    let out = Path::new(out_dir);
    std::fs::create_dir_all(out).with_context(|| format!("failed to create {out_dir}"))?;

    let fast = build_fast_suite(&class);
    let full = build_full_suite(&class);
    let coverage = build_coverage_suite(&class);

    write_suite(&out.join("fast-suite.json"), &fast)?;
    write_suite(&out.join("full-suite.json"), &full)?;
    write_suite(&out.join("coverage-suite.json"), &coverage)?;

    println!("wrote 3 suite manifests to {out_dir}/");
    println!(
        "  fast      {:>6}ms ({} binaries) — every PR",
        fast.estimated_ms,
        fast.binaries.len()
    );
    println!(
        "  full      {:>6}ms ({} binaries) — merge to main",
        full.estimated_ms,
        full.binaries.len()
    );
    println!(
        "  coverage  {:>6}ms ({} binaries) — tarpaulin --lib",
        coverage.estimated_ms,
        coverage.binaries.len()
    );
    Ok(())
}

fn write_suite(path: &Path, suite: &Suite) -> Result<()> {
    let json = serde_json::to_string_pretty(suite)?;
    std::fs::write(path, &json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn bin_entry(b: &crate::classify::ClassifiedBinary) -> SuiteBinary {
    SuiteBinary {
        binary: b.profile.binary.clone(),
        suite: b.profile.suite.clone(),
        duration_ms: b.profile.duration_ms,
    }
}

fn build_fast_suite(class: &crate::classify::Classification) -> Suite {
    let mut binaries = Vec::new();
    let mut est = 0u64;
    for b in &class.bins {
        // Fast suite = lib-only fast + medium binaries. Skip `tests/*`
        // (integration) and `slow` — those go in the full/coverage/integration jobs.
        if (b.speed == Speed::Fast || b.speed == Speed::Medium) && b.profile.suite == "lib" {
            binaries.push(bin_entry(b));
            est += b.profile.duration_ms;
        }
    }
    Suite {
        name: "fast".into(),
        target: "< 60s wall time".into(),
        when: "every PR".into(),
        cargo_args: vec!["test".into(), "--lib".into(), "--no-fail-fast".into()],
        prefer_nextest: true,
        binaries,
        estimated_ms: est,
    }
}

fn build_full_suite(class: &crate::classify::Classification) -> Suite {
    let mut binaries = Vec::new();
    let mut est = 0u64;
    for b in &class.bins {
        if b.speed != Speed::Ignored {
            binaries.push(bin_entry(b));
            est += b.profile.duration_ms;
        }
    }
    Suite {
        name: "full".into(),
        target: "all non-ignored tests across the workspace".into(),
        when: "merge to main / dev".into(),
        cargo_args: vec!["test".into(), "--workspace".into(), "--no-fail-fast".into()],
        prefer_nextest: true,
        binaries,
        estimated_ms: est,
    }
}

fn build_coverage_suite(class: &crate::classify::Classification) -> Suite {
    let mut binaries = Vec::new();
    let mut est = 0u64;
    for b in &class.bins {
        // Coverage = lib-only fast + medium. Tarpaulin's `--lib` already
        // skips integration tests; this manifest documents *which* lib
        // binaries are expected to be slow and therefore tolerated.
        if (b.speed == Speed::Fast || b.speed == Speed::Medium) && b.profile.suite == "lib" {
            binaries.push(bin_entry(b));
            est += b.profile.duration_ms;
        }
    }
    Suite {
        name: "coverage".into(),
        target: "unit tests only — skip slow integration tests".into(),
        when: "coverage job (every PR + main)".into(),
        cargo_args: vec![
            "tarpaulin".into(),
            "--out".into(),
            "Xml".into(),
            "--locked".into(),
            "--lib".into(),
            "--timeout".into(),
            "120".into(),
        ],
        prefer_nextest: false,
        binaries,
        estimated_ms: est,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::classify;
    use crate::profile::{BinaryProfile, Profile};

    fn bin(name: &str, suite: &str, ms: u64, passed: u64, ignored: u64) -> BinaryProfile {
        BinaryProfile {
            binary: name.to_string(),
            suite: suite.to_string(),
            duration_ms: ms,
            passed,
            failed: 0,
            ignored,
        }
    }

    fn profile() -> Profile {
        Profile {
            binaries: vec![
                bin("kirkforge", "lib", 43_000, 1285, 2),
                bin(
                    "integration_test",
                    "tests/integration_test.rs",
                    38_000,
                    14,
                    3,
                ),
                bin("smoke_test", "tests/smoke_test.rs", 2_000, 5, 0),
            ],
            total_duration_ms: 83_000,
            wall_time_ms: 83_000,
        }
    }

    #[test]
    fn fast_suite_excludes_tests_dir() {
        let class = classify(&profile());
        let s = build_fast_suite(&class);
        // lib is 43s → medium; still included in fast suite (lib-only).
        assert_eq!(s.binaries.len(), 1);
        assert_eq!(s.binaries[0].binary, "kirkforge");
        assert!(s.cargo_args.contains(&"--lib".to_string()));
    }

    #[test]
    fn full_suite_includes_everything_non_ignored() {
        let class = classify(&profile());
        let s = build_full_suite(&class);
        assert_eq!(s.binaries.len(), 3);
        assert!(s.cargo_args.contains(&"--workspace".to_string()));
    }

    #[test]
    fn coverage_suite_uses_tarpaulin_lib() {
        let class = classify(&profile());
        let s = build_coverage_suite(&class);
        assert!(s.cargo_args[0] == "tarpaulin");
        assert!(s.cargo_args.contains(&"--lib".to_string()));
        assert!(!s.cargo_args.contains(&"--workspace".to_string()));
        // Only the lib binary.
        assert_eq!(s.binaries.len(), 1);
        assert_eq!(s.binaries[0].binary, "kirkforge");
    }
}
