//! Suggest fixes for slow tests.
//!
//! The suggestions are heuristics keyed on the binary name / suite path.
//! They are deliberately conservative — the doctor never rewrites source
//! code; it prints the suggested annotation and the human applies it.
//!
//! WO 12.6 adds `suggestions_from_source`: source-analysis heuristics
//! that read the test's source file and scan for patterns (subprocess
//! spawn, `tokio::time::sleep`, `std::env::set_var`, network calls,
//! `std::fs::write` to a temp dir). The v1 binary-level `suggestions_for`
//! remains the fallback when no source data is available.

use std::path::Path;

use anyhow::Result;
use regex::Regex;

use crate::classify::{classify, classify_per_test, Speed};
use crate::profile::{load, PerTestProfile, TestProfile};

/// A specific, actionable suggestion produced by the smart suggest
/// path (WO 12.6). The v1 path emits plain `String`s; this struct
/// carries enough structure for the `apply` command to attempt a
/// text-based fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Stable slug used as the `--suggestion <id>` argument to `apply`.
    pub id: String,
    /// Test name (matches `TestProfile::name`).
    pub test: String,
    /// `high` / `medium` / `low` — derived from the per-test duration.
    pub severity: String,
    /// One-line fix description (human-readable).
    pub fix: String,
    /// Why this fix applies (references the matched source pattern).
    pub rationale: String,
    /// Machine-readable kind; selects the apply strategy.
    pub kind: SuggestionKind,
}

/// The fix kind. `apply_suggestion` dispatches on this. The variants
/// line up 1:1 with the WO 12.6 heuristics + the `#[ignore]` annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    /// Add `#[ignore = "slow: <reason>"]` above the test fn.
    IgnoreSlow,
    /// Wrap `#[tokio::test]` with `start_paused = true`.
    TokioStartPaused,
    /// Replace `std::env::set_var(K, V)` with an `EnvGuard`-style call.
    EnvGuard,
    /// Mock a spawned subprocess (`std::process::Command`).
    MockSubprocess,
    /// Use `wiremock` instead of a live network endpoint.
    Wiremock,
    /// Use `tempfile::NamedTempFile` for race-safe temp writes.
    NamedTempFile,
}

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

/// Build a map from binary name → source directory by reading the workspace
/// Cargo.toml and each crate's Cargo.toml. This is deterministic and avoids
/// guessing paths.
fn build_binary_map(root: &Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    // Read workspace Cargo.toml to find members.
    let workspace_toml = root.join("Cargo.toml");
    let Ok(content) = std::fs::read_to_string(&workspace_toml) else {
        return map;
    };
    // Parse members list from `[workspace]` section.
    // Format: members = ["crate1", "crates/kf-foo", ...]
    let members = extract_toml_array(&content, "members");
    for member in &members {
        let member_path = root.join(member);
        let cargo_toml = member_path.join("Cargo.toml");
        let Ok(member_content) = std::fs::read_to_string(&cargo_toml) else {
            continue;
        };
        // Extract [[bin]] targets and [lib] name.
        let bins = extract_toml_array(&member_content, "name");
        for name in &bins {
            map.insert(name.clone(), member_path.to_string_lossy().to_string());
        }
        // Also map the crate name from [package].name to the source dir.
        if let Some(pkg_name) = extract_toml_value(&member_content, "name") {
            map.insert(pkg_name, member_path.to_string_lossy().to_string());
        }
    }
    // Also map the root binary (kf-code → src/main/).
    let root_src = root.join("src");
    if root_src.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&root_src) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "rs").unwrap_or(false) {
                    // Map "kf-code" to the root src dir.
                    map.insert(
                        "kf-code".to_string(),
                        root_src.to_string_lossy().to_string(),
                    );
                    break;
                }
            }
        }
    }
    map
}

/// Extract a TOML array value for a key like "members" or "name".
/// Handles `key = ["a", "b"]` format.
fn extract_toml_array(content: &str, key: &str) -> Vec<String> {
    let mut results = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&format!("{key} =")) || trimmed.starts_with(&format!("{key}=")) {
            // Extract strings between quotes.
            let start = match trimmed.find('[') {
                Some(i) => i,
                None => continue,
            };
            let array_str = &trimmed[start..];
            for part in array_str.split(',') {
                let part = part
                    .trim()
                    .trim_start_matches(&['[', ' '])
                    .trim_end_matches(&[']', ' ', ',']);
                let part = part.trim_matches('"').trim_matches('\'');
                if !part.is_empty() {
                    results.push(part.to_string());
                }
            }
            return results;
        }
    }
    results
}

/// Extract a single TOML string value like `name = "kf-code"`.
fn extract_toml_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&format!("{key} =")) || trimmed.starts_with(&format!("{key}=")) {
            let value = trimmed.split('=').nth(1)?.trim();
            let value = value.trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
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

// ---- Source-analysis heuristics (WO 12.6) --------------------------

/// Per-test severity from the duration. >5000ms is `high` (ignore),
/// >2000ms is `medium` (mock/split), otherwise `low`.
fn severity_for(duration_ms: u64) -> &'static str {
    if duration_ms > 5_000 {
        "high"
    } else if duration_ms > 2_000 {
        "medium"
    } else {
        "low"
    }
}

/// Scan a test's source file for the WO 12.6 patterns and produce
/// specific, actionable suggestions. Each pattern maps to one
/// `SuggestionKind`; the per-test `duration_ms` sets the severity.
///
/// This is regex-based for v1 (not a full parser). The patterns are
/// deliberately narrow: `std::process::Command`, `tokio::time::sleep`,
/// `std::env::set_var`, `reqwest::` / `http::`, and `std::fs::write`
/// near a temp dir. False positives are fine (the human reviews the
/// diff); false negatives just leave the v1 fallback in place.
pub fn suggestions_from_source(test_path: &Path, profile: &TestProfile) -> Vec<Suggestion> {
    let Ok(src) = std::fs::read_to_string(test_path) else {
        return Vec::new();
    };
    let severity = severity_for(profile.duration_ms).to_string();
    let mut out = Vec::new();

    // `std::process::Command` — spawns a subprocess (mock it).
    if Regex::new(r"\bstd::process::Command\b").is_ok_and(|r| r.is_match(&src)) {
        out.push(Suggestion {
            id: format!("{}::mock_subprocess", profile.name),
            test: profile.name.clone(),
            severity: severity.clone(),
            fix: "Factor the `std::process::Command` spawn into a trait \
                  the test can stub, or move the slow case behind \
                  `#[ignore = \"slow: spawns subprocess\"]` + a dedicated \
                  `cargo test -- --ignored` job."
                .to_string(),
            rationale: "the test spawns a subprocess; subprocess startup \
                        dominates wall time and is non-deterministic under \
                        tarpaulin instrumentation."
                .to_string(),
            kind: SuggestionKind::MockSubprocess,
        });
    }

    // `tokio::time::sleep` — use `tokio::time::pause` + start_paused.
    if Regex::new(r"\btokio::time::sleep\b").is_ok_and(|r| r.is_match(&src)) {
        out.push(Suggestion {
            id: format!("{}::tokio_start_paused", profile.name),
            test: profile.name.clone(),
            severity: severity.clone(),
            fix: "Replace `tokio::time::sleep` with `tokio::time::pause` \
                  and wrap the test in `#[tokio::test(start_paused = true)]` \
                  — the runtime advances virtual time instantly."
                .to_string(),
            rationale: "the test waits on real time; virtual time makes the \
                        wait instantaneous and the test deterministic."
                .to_string(),
            kind: SuggestionKind::TokioStartPaused,
        });
    }

    // `std::env::set_var` — suggest EnvGuard (WO 10.0 pattern).
    if Regex::new(r"\bstd::env::set_var\b").is_ok_and(|r| r.is_match(&src)) {
        out.push(Suggestion {
            id: format!("{}::env_guard", profile.name),
            test: profile.name.clone(),
            severity: severity.clone(),
            fix: "Replace `std::env::set_var(K, V)` with \
                  `EnvGuard::set(K, V)` (WO 10.0 pattern; see \
                  `crates/kf-budget-core/src/test_support.rs`). The guard \
                  restores the prior value on Drop and serialises env \
                  access under a mutex."
                .to_string(),
            rationale: "the test mutates process-global env vars; under \
                        tarpaulin instrumentation the temp-dir race widens. \
                        EnvGuard serialises env access."
                .to_string(),
            kind: SuggestionKind::EnvGuard,
        });
    }

    // Network calls — `reqwest::` or `http::` → suggest wiremock.
    if Regex::new(r"\breqwest::|\bhttp::").is_ok_and(|r| r.is_match(&src)) {
        out.push(Suggestion {
            id: format!("{}::wiremock", profile.name),
            test: profile.name.clone(),
            severity: severity.clone(),
            fix: "Use `wiremock` (already a dev-dep) to spin up a local \
                  mock server instead of hitting a live endpoint."
                .to_string(),
            rationale: "the test makes a real network call; live endpoints \
                        are slow, rate-limited, and non-deterministic."
                .to_string(),
            kind: SuggestionKind::Wiremock,
        });
    }

    // `std::fs::write` near a temp dir — suggest NamedTempFile.
    // The pattern is `std::fs::write` AND a `tempdir`/`tempfile`/`/tmp`
    // reference within the same file. Narrow to reduce false positives.
    if Regex::new(r"\bstd::fs::write\b").is_ok_and(|r| r.is_match(&src))
        && Regex::new(r"tempdir|tempfile|/tmp").is_ok_and(|r| r.is_match(&src))
    {
        out.push(Suggestion {
            id: format!("{}::named_temp_file", profile.name),
            test: profile.name.clone(),
            severity: severity.clone(),
            fix: "Use `tempfile::NamedTempFile` for race-safe temp writes \
                  — the file is created atomically and cleaned up on Drop."
                .to_string(),
            rationale: "the test writes to a temp dir; a `std::fs::write` \
                        + manual cleanup is racy under parallel test \
                        execution."
                .to_string(),
            kind: SuggestionKind::NamedTempFile,
        });
    }

    // If the test is pathologically slow (>5s) and none of the source
    // patterns matched, still emit an `#[ignore]` suggestion so the
    // apply path can annotate it.
    if out.is_empty() && profile.duration_ms > 5_000 {
        out.push(Suggestion {
            id: format!("{}::ignore_slow", profile.name),
            test: profile.name.clone(),
            severity: severity.clone(),
            fix: "Move the test behind `#[ignore = \"slow: >5s\"]` and run \
                  it in a dedicated `cargo test -- --ignored` job."
                .to_string(),
            rationale: format!(
                "the test takes {:.1}s and no source-level fix pattern \
                 matched; isolating it keeps the fast suite fast.",
                profile.duration_ms as f64 / 1000.0
            ),
            kind: SuggestionKind::IgnoreSlow,
        });
    }

    out
}

/// Print the smart (source-aware) suggestions for every slow test in a
/// per-test profile. Tests whose source file can't be resolved fall
/// back to the v1 `suggestions_for_test` text.
pub fn run_suggest_detailed(per: &PerTestProfile, filter: Option<&str>) -> Result<()> {
    let class = classify_per_test(per);
    let slow: Vec<_> = class
        .tests
        .iter()
        .filter(|t| t.speed == Speed::Slow)
        .filter(|t| filter.is_none_or(|f| t.profile.name.contains(f)))
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

    println!("smart suggestions for {} slow tests:\n", slow.len());
    for t in slow {
        let p = &t.profile;
        println!("── {} ({} — {}ms) ──", p.name, p.binary, p.duration_ms);
        // Resolve a candidate source path. Build a binary→path map
        // from Cargo.toml so we don't have to guess. Fall back to
        // heuristics if the map doesn't cover this binary.
        let binary_map = build_binary_map(std::path::Path::new("."));
        let found_path: Option<String> = binary_map.get(&p.binary).cloned().or_else(|| {
            let tried = [
                format!("tests/{}.rs", p.binary),
                format!("src/{}.rs", p.binary),
                format!("crates/{}/src/lib.rs", p.binary),
            ];
            tried.into_iter().find(|p| Path::new(p).exists())
        });
        let suggestions = match found_path {
            Some(path) => suggestions_from_source(Path::new(&path), p),
            None => Vec::new(),
        };
        if suggestions.is_empty() {
            // v1 fallback: generic text suggestions.
            for s in suggestions_for_test(p) {
                println!("  • {s}");
            }
        } else {
            for s in suggestions {
                println!("  • [{}] {}", s.id, s.fix);
                println!("    severity: {}", s.severity);
                println!("    rationale: {}", s.rationale);
            }
        }
        println!();
    }
    Ok(())
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
            binary: "kf-code".to_string(),
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

    // ---- suggestions_from_source (WO 12.6) ---------------------------

    fn write_fixture(body: &str) -> tempfile::NamedTempFile {
        let f = tempfile::Builder::new()
            .suffix(".rs")
            .tempfile()
            .expect("create temp fixture");
        std::fs::write(f.path(), body).expect("write fixture");
        f
    }

    #[test]
    fn source_subprocess_pattern_suggests_mock() {
        let f = write_fixture(
            "fn test_foo() {\n    let s = std::process::Command::new(\"cargo\").output();\n}\n",
        );
        let p = tp("test_foo", 3_000);
        let s = suggestions_from_source(f.path(), &p);
        assert!(s.iter().any(|x| x.kind == SuggestionKind::MockSubprocess));
    }

    #[test]
    fn source_sleep_pattern_suggests_start_paused() {
        let f = write_fixture(
            "async fn test_sleep() {\n    tokio::time::sleep(Duration::from_secs(2)).await;\n}\n",
        );
        let p = tp("test_sleep", 3_000);
        let s = suggestions_from_source(f.path(), &p);
        assert!(s.iter().any(|x| x.kind == SuggestionKind::TokioStartPaused));
    }

    #[test]
    fn source_env_set_var_suggests_env_guard() {
        let f = write_fixture("fn test_env() {\n    std::env::set_var(\"FOO\", \"bar\");\n}\n");
        let p = tp("test_env", 1_000);
        let s = suggestions_from_source(f.path(), &p);
        assert!(s.iter().any(|x| x.kind == SuggestionKind::EnvGuard));
    }

    #[test]
    fn source_network_call_suggests_wiremock() {
        let f = write_fixture(
            "async fn test_net() {\n    let r = reqwest::get(\"http://x\").await;\n}\n",
        );
        let p = tp("test_net", 3_000);
        let s = suggestions_from_source(f.path(), &p);
        assert!(s.iter().any(|x| x.kind == SuggestionKind::Wiremock));
    }

    #[test]
    fn source_fs_write_with_temp_suggests_named_temp_file() {
        let f = write_fixture(
            "fn test_write() {\n    let d = tempdir().unwrap();\n    std::fs::write(d.path().join(\"x\"), b\"y\").unwrap();\n}\n",
        );
        let p = tp("test_write", 800);
        let s = suggestions_from_source(f.path(), &p);
        assert!(s.iter().any(|x| x.kind == SuggestionKind::NamedTempFile));
    }

    #[test]
    fn source_no_match_returns_empty() {
        let f = write_fixture("fn test_benign() { assert_eq!(2+2, 4); }\n");
        let p = tp("test_benign", 1_000);
        let s = suggestions_from_source(f.path(), &p);
        assert!(
            s.is_empty(),
            "no patterns matched — expected no suggestions, got {s:?}"
        );
    }

    #[test]
    fn source_very_slow_no_match_falls_back_to_ignore() {
        let f = write_fixture("fn test_slow() { for _ in 0..1_000_000 { /* cpu */ } }\n");
        let p = tp("test_slow", 6_000);
        let s = suggestions_from_source(f.path(), &p);
        assert!(s.iter().any(|x| x.kind == SuggestionKind::IgnoreSlow));
    }

    #[test]
    fn source_missing_file_returns_empty() {
        let p = tp("test_x", 3_000);
        let s = suggestions_from_source(Path::new("/nonexistent/path/to/test.rs"), &p);
        assert!(s.is_empty());
    }

    #[test]
    fn source_severity_tracks_duration() {
        let f = write_fixture(
            "fn test_foo() {\n    let s = std::process::Command::new(\"cargo\").output();\n}\n",
        );
        let high = suggestions_from_source(f.path(), &tp("test_foo", 6_000));
        let med = suggestions_from_source(f.path(), &tp("test_foo", 3_000));
        let low = suggestions_from_source(f.path(), &tp("test_foo", 500));
        assert!(high.iter().any(|x| x.severity == "high"));
        assert!(med.iter().any(|x| x.severity == "medium"));
        assert!(low.iter().any(|x| x.severity == "low"));
    }

    #[test]
    fn source_suggestion_ids_are_stable() {
        let f = write_fixture(
            "fn test_foo() {\n    let s = std::process::Command::new(\"cargo\").output();\n}\n",
        );
        let p = tp("test_foo", 3_000);
        let s = suggestions_from_source(f.path(), &p);
        assert!(s.iter().any(|x| x.id == "test_foo::mock_subprocess"));
    }
}
