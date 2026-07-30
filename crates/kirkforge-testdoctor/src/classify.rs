//! Classify each profiled binary as fast / medium / slow / ignored.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::profile::{BinaryProfile, PerTestProfile, Profile, TestProfile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speed {
    Fast,
    Medium,
    Slow,
    Ignored,
}

impl Speed {
    pub fn as_str(self) -> &'static str {
        match self {
            Speed::Fast => "fast",
            Speed::Medium => "medium",
            Speed::Slow => "slow",
            Speed::Ignored => "ignored",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifiedBinary {
    #[serde(flatten)]
    pub profile: BinaryProfile,
    pub speed: Speed,
    /// Average milliseconds per test (0 if no tests ran).
    pub avg_ms_per_test: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    pub bins: Vec<ClassifiedBinary>,
    pub summary: Summary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub fast: u64,
    pub medium: u64,
    pub slow: u64,
    pub ignored: u64,
    pub fast_total_ms: u64,
    pub medium_total_ms: u64,
    pub slow_total_ms: u64,
}

/// Thresholds (tuned for the KirkForge-Cli workspace; see the design doc).
///
/// The per-test average is the load-bearing signal: a 43s lib suite with
/// 1287 tests (33ms/test) is `fast`, while a 38s integration suite with
/// 14 tests (2700ms/test) is `slow`. The binary-total thresholds are a
/// secondary guard so a single test that takes 60s is still `slow` even
/// when it is the only test in its binary.
const FAST_PER_TEST_MS: u64 = 100;
const MEDIUM_PER_TEST_MS: u64 = 500;
const SLOW_BINARY_MS: u64 = 60_000;
// A single test slower than this is `slow` even if its binary avg is
// fast (WO 12.5 per-test path). 2000ms is the workorder default.
const SLOW_TEST_MS: u64 = 2_000;

pub fn classify(profile: &Profile) -> Classification {
    let mut bins = Vec::with_capacity(profile.binaries.len());
    let mut summary = Summary::default();

    for b in &profile.binaries {
        let ran = b.passed + b.failed;
        let avg_ms_per_test = if ran == 0 { 0 } else { b.duration_ms / ran };
        let speed = if b.passed == 0 && b.failed == 0 && b.ignored > 0 {
            Speed::Ignored
        } else if b.duration_ms >= SLOW_BINARY_MS || avg_ms_per_test > MEDIUM_PER_TEST_MS {
            // Either the binary as a whole is pathologically slow, or the
            // average test takes more than 10s — both are `slow`.
            Speed::Slow
        } else if avg_ms_per_test <= FAST_PER_TEST_MS {
            Speed::Fast
        } else {
            Speed::Medium
        };

        match speed {
            Speed::Fast => {
                summary.fast += 1;
                summary.fast_total_ms += b.duration_ms;
            }
            Speed::Medium => {
                summary.medium += 1;
                summary.medium_total_ms += b.duration_ms;
            }
            Speed::Slow => {
                summary.slow += 1;
                summary.slow_total_ms += b.duration_ms;
            }
            Speed::Ignored => {
                summary.ignored += 1;
            }
        }

        bins.push(ClassifiedBinary {
            profile: b.clone(),
            speed,
            avg_ms_per_test,
        });
    }

    Classification { bins, summary }
}

// ---- Per-test classification (WO 12.5) ------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifiedTest {
    #[serde(flatten)]
    pub profile: TestProfile,
    pub speed: Speed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerTestClassification {
    pub tests: Vec<ClassifiedTest>,
    pub summary: Summary,
    pub coarse: bool,
}

/// Classify each individual test. A test with `duration_ms > SLOW_TEST_MS`
/// is `slow` even when its binary average is fast. Ignored tests are
/// `ignored`. Otherwise the per-test duration is bucketed by the same
/// fast/medium thresholds as the binary path.
pub fn classify_per_test(per: &PerTestProfile) -> PerTestClassification {
    let mut tests = Vec::with_capacity(per.tests.len());
    let mut summary = Summary::default();
    for t in &per.tests {
        let speed = if t.ignored {
            Speed::Ignored
        } else if t.duration_ms > SLOW_TEST_MS || t.duration_ms > MEDIUM_PER_TEST_MS * 1000 {
            // A single test slower than SLOW_TEST_MS is slow, even if its
            // binary average is fast. The MEDIUM_PER_TEST_MS*1000 guard is
            // the same guardrail as SLOW_BINARY_MS for a single test.
            Speed::Slow
        } else if !t.passed {
            // A failing test is not slow/medium/fast — it did not finish
            // cleanly. Keep it as medium so it shows up in the report
            // without inflating the slow count.
            Speed::Medium
        } else if t.duration_ms <= FAST_PER_TEST_MS {
            Speed::Fast
        } else {
            Speed::Medium
        };
        match speed {
            Speed::Fast => {
                summary.fast += 1;
                summary.fast_total_ms += t.duration_ms;
            }
            Speed::Medium => {
                summary.medium += 1;
                summary.medium_total_ms += t.duration_ms;
            }
            Speed::Slow => {
                summary.slow += 1;
                summary.slow_total_ms += t.duration_ms;
            }
            Speed::Ignored => {
                summary.ignored += 1;
            }
        }
        tests.push(ClassifiedTest {
            profile: t.clone(),
            speed,
        });
    }
    PerTestClassification {
        tests,
        summary,
        coarse: per.coarse,
    }
}

pub fn run(profile_path: &str) -> Result<()> {
    let profile = crate::profile::load(profile_path)?;
    let class = classify(&profile);
    println!(
        "{:<30} {:<8} {:>10} {:>10} {:>6}",
        "binary", "speed", "dur_ms", "avg/test", "tests"
    );
    println!("{}", "-".repeat(70));
    for b in &class.bins {
        let tests = b.profile.passed + b.profile.failed + b.profile.ignored;
        println!(
            "{:<30} {:<8} {:>10} {:>10} {:>6}",
            b.profile.binary,
            b.speed.as_str(),
            b.profile.duration_ms,
            b.avg_ms_per_test,
            tests
        );
    }
    println!("{}", "-".repeat(70));
    println!(
        "summary: fast={} ({}ms)  medium={} ({}ms)  slow={} ({}ms)  ignored={}",
        class.summary.fast,
        class.summary.fast_total_ms,
        class.summary.medium,
        class.summary.medium_total_ms,
        class.summary.slow,
        class.summary.slow_total_ms,
        class.summary.ignored,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn fast_lib_is_fast() {
        let p = Profile {
            binaries: vec![bin("kirkforge", "lib", 4_000, 1285, 2)],
            total_duration_ms: 4_000,
            wall_time_ms: 4_000,
        };
        let c = classify(&p);
        assert_eq!(c.bins[0].speed, Speed::Fast);
        assert_eq!(c.summary.fast, 1);
    }

    #[test]
    fn slow_integration_is_slow() {
        let p = Profile {
            binaries: vec![bin(
                "integration_test",
                "tests/integration_test.rs",
                38_000,
                14,
                3,
            )],
            total_duration_ms: 38_000,
            wall_time_ms: 38_000,
        };
        let c = classify(&p);
        assert_eq!(c.bins[0].speed, Speed::Slow);
        assert_eq!(c.summary.slow, 1);
    }

    #[test]
    fn long_binary_with_many_tests_is_medium() {
        // 30s total, 100 tests → 300ms/test: medium on the per-test axis.
        let p = Profile {
            binaries: vec![bin("kirkforge", "lib", 30_000, 100, 0)],
            total_duration_ms: 30_000,
            wall_time_ms: 30_000,
        };
        let c = classify(&p);
        assert_eq!(c.bins[0].speed, Speed::Medium);
    }

    #[test]
    fn all_ignored_binary_is_ignored() {
        let p = Profile {
            binaries: vec![bin("slow_suite", "tests/slow.rs", 0, 0, 5)],
            total_duration_ms: 0,
            wall_time_ms: 0,
        };
        let c = classify(&p);
        assert_eq!(c.bins[0].speed, Speed::Ignored);
        assert_eq!(c.summary.ignored, 1);
    }

    // ---- classify_per_test (WO 12.5) ----

    fn test_profile(name: &str, binary: &str, ms: u64, passed: bool, ignored: bool) -> TestProfile {
        TestProfile {
            name: name.to_string(),
            binary: binary.to_string(),
            duration_ms: ms,
            passed,
            ignored,
        }
    }

    fn per_test_profile(tests: Vec<TestProfile>, coarse: bool) -> PerTestProfile {
        PerTestProfile {
            tests,
            wall_time_ms: 0,
            coarse,
        }
    }

    #[test]
    fn per_test_fast_test_is_fast() {
        let p = per_test_profile(vec![test_profile("a", "kirkforge", 50, true, false)], false);
        let c = classify_per_test(&p);
        assert_eq!(c.tests[0].speed, Speed::Fast);
        assert_eq!(c.summary.fast, 1);
    }

    #[test]
    fn per_test_slow_single_test_is_slow_even_in_a_fast_binary() {
        // One test at 3000ms is slow (above SLOW_TEST_MS=2000), even
        // though a binary with a 30ms avg would be classified fast.
        let p = per_test_profile(
            vec![test_profile("slow_one", "kirkforge", 3000, true, false)],
            false,
        );
        let c = classify_per_test(&p);
        assert_eq!(c.tests[0].speed, Speed::Slow);
        assert_eq!(c.summary.slow, 1);
    }

    #[test]
    fn per_test_medium_test_is_medium() {
        let p = per_test_profile(
            vec![test_profile("m", "kirkforge", 300, true, false)],
            false,
        );
        let c = classify_per_test(&p);
        assert_eq!(c.tests[0].speed, Speed::Medium);
        assert_eq!(c.summary.medium, 1);
    }

    #[test]
    fn per_test_ignored_test_is_ignored() {
        let p = per_test_profile(
            vec![test_profile("ign", "kirkforge", 0, false, true)],
            false,
        );
        let c = classify_per_test(&p);
        assert_eq!(c.tests[0].speed, Speed::Ignored);
        assert_eq!(c.summary.ignored, 1);
    }

    #[test]
    fn per_test_carries_coarse_flag() {
        let p = per_test_profile(vec![test_profile("a", "kirkforge", 100, true, false)], true);
        let c = classify_per_test(&p);
        assert!(c.coarse);
    }

    #[test]
    fn per_test_failing_test_is_medium_not_slow() {
        let p = per_test_profile(
            vec![test_profile("fail", "kirkforge", 100, false, false)],
            false,
        );
        let c = classify_per_test(&p);
        assert_eq!(c.tests[0].speed, Speed::Medium);
    }
}
