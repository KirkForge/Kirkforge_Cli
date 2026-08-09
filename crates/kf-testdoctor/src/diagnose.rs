//! Self-diagnosis: scan source files for untested public API surface.
//!
//! Counts `pub` items (fn, struct, enum, trait, const, type) plus `impl`
//! method signatures, excluding `pub(crate)`/`pub(super)`. The `api_surface`
//! metric replaces raw line count for density and ROI: files with many public
//! APIs but few tests float to the top.

use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct FileDiagnosis {
    pub path: String,
    pub lines: usize,
    pub pub_items: usize,
    /// Public API surface: pub items + impl methods, excluding pub(crate)/pub(super).
    pub api_surface: usize,
    pub test_count: usize,
    /// test_count / api_surface × 100 (falls back to lines-based if api_surface == 0).
    pub test_density: f64,
    /// ROI = api_surface × (1 - test_density/100). Higher = more untested API.
    pub roi: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosisReport {
    pub per_dir: Vec<DirDiagnosis>,
    pub top_targets: Vec<FileDiagnosis>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirDiagnosis {
    pub dir: String,
    pub files: usize,
    pub total_lines: usize,
    pub total_tests: usize,
    pub avg_test_density: f64,
}

/// Directories to scan by default. Covers all source directories
/// in the workspace, not just the 3 CI originally scanned.
const DEFAULT_DIRS: &[&str] = &[
    "src/session",
    "src/tools",
    "src/adapters",
    "src/tui",
    "src/daemon",
    "src/jobs",
    "src/main",
    "src/shared",
    "crates",
];

pub fn diagnose_with_dirs(root: &Path, dirs: &[&str]) -> Result<DiagnosisReport> {
    let mut all_files: Vec<FileDiagnosis> = Vec::new();

    for dir in dirs {
        let dir_path = root.join(dir);
        if !dir_path.is_dir() {
            continue;
        }
        let files = collect_rs_files(&dir_path)?;
        for f in files {
            if let Some(diag) = analyze_file(&f, root) {
                all_files.push(diag);
            }
        }
    }

    all_files.sort_by(|a, b| {
        b.roi
            .partial_cmp(&a.roi)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let per_dir = dirs
        .iter()
        .filter_map(|dir| {
            let files_in_dir: Vec<&FileDiagnosis> = all_files
                .iter()
                .filter(|f| f.path.starts_with(dir))
                .collect();
            if files_in_dir.is_empty() {
                return None;
            }
            let total_lines: usize = files_in_dir.iter().map(|f| f.lines).sum();
            let total_tests: usize = files_in_dir.iter().map(|f| f.test_count).sum();
            let avg_density = if total_lines > 0 {
                total_tests as f64 / total_lines as f64
            } else {
                0.0
            };
            Some(DirDiagnosis {
                dir: dir.to_string(),
                files: files_in_dir.len(),
                total_lines,
                total_tests,
                avg_test_density: avg_density,
            })
        })
        .collect();

    let top_targets: Vec<FileDiagnosis> = all_files.into_iter().take(25).collect();

    Ok(DiagnosisReport {
        per_dir,
        top_targets,
    })
}

/// Diagnose with default directories.
pub fn diagnose(root: &Path) -> Result<DiagnosisReport> {
    diagnose_with_dirs(root, DEFAULT_DIRS)
}

fn collect_rs_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_rs_files_recursive(dir, &mut files)?;
    Ok(files)
}

fn collect_rs_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name();
            if name.map(|n| n == "target").unwrap_or(false) {
                continue;
            }
            collect_rs_files_recursive(&path, files)?;
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            files.push(path);
        }
    }
    Ok(())
}

fn analyze_file(path: &Path, root: &Path) -> Option<FileDiagnosis> {
    let text = std::fs::read_to_string(path).ok()?;
    let lines = text.lines().count();
    if lines == 0 {
        return None;
    }

    let (pub_items, impl_methods, test_count) = count_api_and_tests(&text);
    let api_surface = pub_items + impl_methods;
    let test_density = if api_surface > 0 {
        test_count as f64 / api_surface as f64 * 100.0
    } else if lines > 0 {
        test_count as f64 / lines as f64 * 100.0
    } else {
        0.0
    };

    let roi = if api_surface > 0 {
        api_surface as f64 * (1.0 - test_density / 100.0)
    } else {
        lines as f64 * (1.0 - test_density / 100.0)
    };

    let rel_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    Some(FileDiagnosis {
        path: rel_path,
        lines,
        pub_items,
        api_surface,
        test_count,
        test_density,
        roi,
    })
}

/// Count top-level pub items and impl methods in a single pass.
/// Returns (pub_items, impl_methods) where pub_items excludes methods
/// inside `impl` blocks (to avoid double-counting).
#[cfg(test)]
fn count_api_items(text: &str) -> (usize, usize) {
    let mut pub_items = 0;
    let mut impl_methods = 0;
    let mut in_impl = false;
    let mut brace_depth: i32 = 0;

    for line in text.lines() {
        let trimmed = line.trim();
        // Skip pub(crate) and pub(super) — not public API.
        if trimmed.starts_with("pub(crate)") || trimmed.starts_with("pub(super)") {
            // Track braces even for restricted-visibility lines.
            if in_impl {
                for ch in trimmed.chars() {
                    match ch {
                        '{' => brace_depth += 1,
                        '}' => {
                            brace_depth -= 1;
                            if brace_depth <= 0 {
                                in_impl = false;
                            }
                        }
                        _ => {}
                    }
                }
            }
            continue;
        }
        if !in_impl {
            if trimmed.starts_with("impl ") && trimmed.contains('{') {
                in_impl = true;
                for ch in trimmed.chars() {
                    match ch {
                        '{' => brace_depth += 1,
                        '}' => brace_depth -= 1,
                        _ => {}
                    }
                }
                if brace_depth == 0 {
                    in_impl = false;
                }
            } else if trimmed.starts_with("pub fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("pub enum ")
                || trimmed.starts_with("pub trait ")
            {
                pub_items += 1;
            }
        } else {
            // Inside impl: count pub fn/async fn as impl methods.
            if trimmed.starts_with("pub fn ") || trimmed.starts_with("pub async fn ") {
                impl_methods += 1;
            }
            for ch in trimmed.chars() {
                match ch {
                    '{' => brace_depth += 1,
                    '}' => {
                        brace_depth -= 1;
                        if brace_depth <= 0 {
                            in_impl = false;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    (pub_items, impl_methods)
}

/// Single-pass: counts pub items, impl methods, and test attributes
/// in one line iteration. Returns (pub_items, impl_methods, test_count).
fn count_api_and_tests(text: &str) -> (usize, usize, usize) {
    let mut pub_items = 0;
    let mut impl_methods = 0;
    let mut test_count = 0;
    let mut in_impl = false;
    let mut brace_depth: i32 = 0;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed == "#[test]"
            || trimmed == "#[tokio::test]"
            || trimmed.starts_with("#[tokio::test(")
        {
            test_count += 1;
        }

        if trimmed.starts_with("pub(crate)") || trimmed.starts_with("pub(super)") {
            if in_impl {
                for ch in trimmed.chars() {
                    match ch {
                        '{' => brace_depth += 1,
                        '}' => {
                            brace_depth -= 1;
                            if brace_depth <= 0 {
                                in_impl = false;
                            }
                        }
                        _ => {}
                    }
                }
            }
            continue;
        }
        if !in_impl {
            if trimmed.starts_with("impl ") && trimmed.contains('{') {
                in_impl = true;
                for ch in trimmed.chars() {
                    match ch {
                        '{' => brace_depth += 1,
                        '}' => brace_depth -= 1,
                        _ => {}
                    }
                }
                if brace_depth == 0 {
                    in_impl = false;
                }
            } else if trimmed.starts_with("pub fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("pub enum ")
                || trimmed.starts_with("pub trait ")
            {
                pub_items += 1;
            }
        } else {
            if trimmed.starts_with("pub fn ") || trimmed.starts_with("pub async fn ") {
                impl_methods += 1;
            }
            for ch in trimmed.chars() {
                match ch {
                    '{' => brace_depth += 1,
                    '}' => {
                        brace_depth -= 1;
                        if brace_depth <= 0 {
                            in_impl = false;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    (pub_items, impl_methods, test_count)
}

/// Shorthand for just the pub_items count (top-level only).
#[cfg(test)]
fn count_pub_items(text: &str) -> usize {
    count_api_items(text).0
}

#[cfg(test)]
fn count_tests(text: &str) -> usize {
    text.lines()
        .filter(|l| {
            let t = l.trim();
            t == "#[test]" || t == "#[tokio::test]" || t.starts_with("#[tokio::test(")
        })
        .count()
}

/// Cross-reference diagnosis with coverage gaps: files that are both
/// low-test-density AND low-coverage are the highest ROI targets.
pub fn print_coverage_crossref(report: &DiagnosisReport, gaps: &crate::gaps::CoverageGaps) {
    use std::collections::HashMap;
    let cov_map: HashMap<&str, f64> = gaps
        .per_file
        .iter()
        .map(|f| (f.path.as_str(), f.rate))
        .collect();

    let mut cross: Vec<(&FileDiagnosis, f64)> = report
        .top_targets
        .iter()
        .filter_map(|f| {
            let rate = cov_map.get(f.path.as_str())?;
            Some((f, *rate))
        })
        .collect();

    if cross.is_empty() {
        println!("\nNo overlap between diagnose targets and coverage data.");
        return;
    }

    cross.sort_by(|a, b| {
        let sa = a.1 + a.0.test_density;
        let sb = b.1 + b.0.test_density;
        sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
    });

    println!("\nCross-reference: low-coverage + low-test-density (highest ROI):");
    println!(
        "  {path:<50} {cov:>7}  {density:>7}  {roi:>5}",
        path = "file",
        cov = "cov%",
        density = "test%",
        roi = "ROI"
    );
    for (f, cov) in &cross {
        println!(
            "  {path:<50} {api:>3} api {cov:>6.1}% {density:>6.1}%  {roi:>5.0}",
            path = f.path,
            api = f.api_surface,
            cov = cov,
            density = f.test_density,
            roi = f.roi
        );
    }
}

pub fn print_report(report: &DiagnosisReport) {
    println!("Self-diagnosis report:\n");

    for d in &report.per_dir {
        println!(
            "  {dir:<20} {files:>4} files  {lines:>6} lines  {tests:>4} tests  density {density:.1}%",
            dir = d.dir,
            files = d.files,
            lines = d.total_lines,
            tests = d.total_tests,
            density = d.avg_test_density * 100.0,
        );
    }
    println!();

    println!("Top 25 test ROI targets (api_surface × untestedness):");
    for f in &report.top_targets {
        println!(
            "  {path:<55} {lines:>5}L  {api:>3} api  {tests:>3} tests  density={density:.0}%  ROI={roi:.0}",
            path = f.path,
            lines = f.lines,
            api = f.api_surface,
            tests = f.test_count,
            density = f.test_density,
            roi = f.roi,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_pub_fn() {
        assert_eq!(count_pub_items("pub fn foo() {}"), 1);
    }

    #[test]
    fn count_pub_async_fn() {
        assert_eq!(count_pub_items("pub async fn bar() {}"), 1);
    }

    #[test]
    fn count_pub_struct() {
        assert_eq!(count_pub_items("pub struct Foo;"), 1);
    }

    #[test]
    fn count_pub_enum() {
        assert_eq!(count_pub_items("pub enum Color { Red, Blue }"), 1);
    }

    #[test]
    fn count_pub_trait() {
        assert_eq!(count_pub_items("pub trait Foo { fn bar(&self); }"), 1);
    }

    #[test]
    fn count_ignores_private() {
        assert_eq!(count_pub_items("fn foo() {}"), 0);
        assert_eq!(count_pub_items("struct Foo;"), 0);
    }

    #[test]
    fn count_ignores_pub_crate() {
        assert_eq!(count_pub_items("pub(crate) fn foo() {}"), 0);
        assert_eq!(count_pub_items("pub(crate) struct Foo;"), 0);
    }

    #[test]
    fn count_ignores_pub_super() {
        assert_eq!(count_pub_items("pub(super) fn bar() {}"), 0);
    }

    #[test]
    fn count_impl_methods_simple() {
        let text = "impl Foo {\n    pub fn bar(&self) {}\n    pub fn baz(&self) {}\n    fn private(&self) {}\n}\n";
        let (_, impl_methods) = count_api_items(text);
        assert_eq!(impl_methods, 2);
    }

    #[test]
    fn count_impl_methods_async() {
        let text = "impl Foo {\n    pub async fn fetch(&self) {}\n}\n";
        let (_, impl_methods) = count_api_items(text);
        assert_eq!(impl_methods, 1);
    }

    #[test]
    fn count_impl_methods_ignores_pub_crate() {
        let text =
            "impl Foo {\n    pub(crate) fn internal(&self) {}\n    pub fn visible(&self) {}\n}\n";
        let (_, impl_methods) = count_api_items(text);
        assert_eq!(impl_methods, 1);
    }

    #[test]
    fn count_impl_methods_trait_impl() {
        let text = "impl Display for Foo {\n    pub fn fmt(&self) {}\n}\n";
        let (_, impl_methods) = count_api_items(text);
        assert_eq!(impl_methods, 1);
    }

    #[test]
    fn api_surface_includes_impl_methods() {
        let tmp = std::env::temp_dir().join("testdoctor-api-surface.rs");
        let content =
            "pub struct Foo;\nimpl Foo {\n    pub fn bar(&self) {}\n    pub fn baz(&self) {}\n}\n";
        std::fs::write(&tmp, content).unwrap();
        let diag = analyze_file(&tmp, &std::env::temp_dir()).unwrap();
        assert_eq!(diag.pub_items, 1); // pub struct
        assert_eq!(diag.api_surface, 3); // 1 pub struct + 2 pub fn in impl
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn count_multiple_pub_items() {
        let text = "pub fn a() {}\npub fn b() {}\npub struct C;\nfn d() {}";
        assert_eq!(count_pub_items(text), 3);
    }

    #[test]
    fn count_test_attributes() {
        assert_eq!(count_tests("#[test]"), 1);
        assert_eq!(count_tests("#[tokio::test]"), 1);
        assert_eq!(count_tests("#[tokio::test(start_paused = true)]"), 1);
    }

    #[test]
    fn count_tests_in_text() {
        let text = "#[test]\nfn foo() {}\n#[tokio::test]\nasync fn bar() {}\nfn baz() {}";
        assert_eq!(count_tests(text), 2);
    }

    #[test]
    fn count_tests_ignores_comments() {
        assert_eq!(count_tests("// #[test]"), 0);
        assert_eq!(count_tests("/// #[test]"), 0);
    }

    #[test]
    fn analyze_file_returns_diagnosis() {
        let tmp = std::env::temp_dir().join("testdoctor-diag-test.rs");
        std::fs::write(&tmp, "pub fn foo() {}\n#[test]\nfn test_foo() {}\n").unwrap();
        let diag = analyze_file(&tmp, &std::env::temp_dir()).unwrap();
        assert_eq!(diag.lines, 3);
        assert_eq!(diag.pub_items, 1);
        assert_eq!(diag.test_count, 1);
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn analyze_file_empty_returns_none() {
        let tmp = std::env::temp_dir().join("testdoctor-diag-empty.rs");
        std::fs::write(&tmp, "").unwrap();
        let diag = analyze_file(&tmp, &std::env::temp_dir());
        assert!(diag.is_none());
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn roi_is_high_for_large_untested_files() {
        let tmp = std::env::temp_dir().join("testdoctor-roi-test.rs");
        let content = "pub fn foo() {}\n".repeat(100);
        std::fs::write(&tmp, &content).unwrap();
        let diag = analyze_file(&tmp, &std::env::temp_dir()).unwrap();
        assert!(
            diag.roi > 90.0,
            "ROI should be high for 100-line file with 0 tests"
        );
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn roi_is_low_for_well_tested_files() {
        let tmp = std::env::temp_dir().join("testdoctor-roi-low.rs");
        let content = (0..50)
            .map(|i| format!("#[test]\nfn test_{i}() {{}}\n"))
            .collect::<String>();
        std::fs::write(&tmp, &content).unwrap();
        let diag = analyze_file(&tmp, &std::env::temp_dir()).unwrap();
        assert!(
            diag.roi <= 50.0,
            "ROI should be low for well-tested file, got {}",
            diag.roi
        );
        std::fs::remove_file(&tmp).unwrap();
    }
}
