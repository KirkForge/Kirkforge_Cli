//! Self-diagnosis: scan the project's own source files to find public
//! functions/types that have no co-located tests, and suggest test targets.
//!
//! This is the "self-diagnosis" evolution of the testdoctor: instead of
//! just profiling test *time*, it analyzes the *structure* of the codebase
//! to find untested code. It works by:
//!
//! 1. Scanning `.rs` files in the given directories for `pub fn`, `pub async fn`,
//!    `pub struct`, `pub enum`, `pub trait`.
//! 2. Checking whether the file (or its parent module's test block) has any
//!    `#[test]` or `#[tokio::test]` attributes.
//! 3. Reporting files with public items but zero or few tests, ranked by
//!    "test ROI" = line_count × (1 - test_density).
//!
//! The analysis is static (no compilation needed) — it's a heuristic that
//! helps developers find the highest-impact files to add tests to.

use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct FileDiagnosis {
    pub path: String,
    pub lines: usize,
    pub pub_items: usize,
    pub test_count: usize,
    pub test_density: f64,
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

/// Directories to scan by default.
const DEFAULT_DIRS: &[&str] = &["src/session", "src/tools", "src/adapters"];

pub fn diagnose(root: &Path) -> Result<DiagnosisReport> {
    let mut all_files: Vec<FileDiagnosis> = Vec::new();

    for dir in DEFAULT_DIRS {
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

    let per_dir = DEFAULT_DIRS
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

    let pub_items = count_pub_items(&text);
    let test_count = count_tests(&text);
    let test_density = if lines > 0 {
        test_count as f64 / lines as f64 * 100.0
    } else {
        0.0
    };

    // ROI = lines × (1 - test_density/100) — higher for large files
    // with few tests.
    let roi = lines as f64 * (1.0 - test_density / 100.0);

    let rel_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    Some(FileDiagnosis {
        path: rel_path,
        lines,
        pub_items,
        test_count,
        test_density,
        roi,
    })
}

fn count_pub_items(text: &str) -> usize {
    let mut count = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("pub struct ")
            || trimmed.starts_with("pub enum ")
            || trimmed.starts_with("pub trait ")
        {
            count += 1;
        }
    }
    count
}

fn count_tests(text: &str) -> usize {
    text.lines()
        .filter(|l| {
            let t = l.trim();
            t == "#[test]" || t == "#[tokio::test]" || t.starts_with("#[tokio::test(")
        })
        .count()
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

    println!("Top 25 test ROI targets (lines × untestedness):");
    for f in &report.top_targets {
        println!(
            "  {path:<55} {lines:>5}L  {pubs:>3} pub  {tests:>3} tests  ROI={roi:.0}",
            path = f.path,
            lines = f.lines,
            pubs = f.pub_items,
            tests = f.test_count,
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
            .map(|i| format!("#[test]\nfn test_{}() {{}}\n", i))
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
