//! Coverage-gap analysis: parse Cobertura XML from `cargo tarpaulin`,
//! find the least-covered files, group uncovered lines into ranges, and
//! suggest test targets.
//!
//! The Cobertura XML format is:
//! ```xml
//! <coverage line-rate="0.51">
//!   <packages>
//!     <package>
//!       <classes>
//!         <class filename="src/session/foo.rs" line-rate="0.45">
//!           <lines>
//!             <line number="42" hits="0"/>
//!             <line number="43" hits="1"/>
//!           </lines>
//!         </class>
//!       </classes>
//!     </package>
//!   </packages>
//! </coverage>
//! ```

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct FileCoverage {
    pub path: String,
    pub lines_total: u32,
    pub lines_covered: u32,
    pub rate: f64,
    pub uncovered_ranges: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirCoverage {
    pub dir: String,
    pub lines_total: u32,
    pub lines_covered: u32,
    pub rate: f64,
    pub threshold: f64,
    pub headroom: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverageGaps {
    pub overall_rate: f64,
    pub per_file: Vec<FileCoverage>,
    pub per_dir: Vec<DirCoverage>,
}

/// Default thresholds matching `.github/workflows/ci.yml`.
// Must match the CI thresholds in .github/workflows/ci.yml (the coverage gate).
const DEFAULT_THRESHOLDS: &[(&str, f64)] = &[
    ("src/session", 68.5),
    ("src/tools", 76.0),
    ("src/adapters", 75.0),
];

pub fn analyze_gaps(xml_path: &Path) -> Result<CoverageGaps> {
    let text = std::fs::read_to_string(xml_path)
        .with_context(|| format!("failed to read coverage XML at {}", xml_path.display()))?;
    let classes = parse_cobertura(&text)?;
    let overall_rate = parse_overall_rate(&text);

    let mut per_file: Vec<FileCoverage> = classes
        .into_iter()
        .map(|c| compute_file_coverage(&c))
        .collect();
    per_file.sort_by(|a, b| {
        a.rate
            .partial_cmp(&b.rate)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let per_dir = compute_dir_coverage(&per_file);

    Ok(CoverageGaps {
        overall_rate,
        per_file,
        per_dir,
    })
}

struct RawClass {
    filename: String,
    lines: Vec<(u32, u32)>,
}

fn parse_cobertura(xml: &str) -> Result<Vec<RawClass>> {
    let mut classes = Vec::new();
    for class_xml in extract_class_blocks(xml) {
        let filename = extract_attr(&class_xml, "filename").unwrap_or_default();
        if filename.is_empty() {
            continue;
        }
        let lines = extract_lines(&class_xml);
        classes.push(RawClass { filename, lines });
    }
    if classes.is_empty() {
        anyhow::bail!("no <class> blocks found in Cobertura XML");
    }
    Ok(classes)
}

fn extract_class_blocks(xml: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut search_pos = 0;
    while let Some(start) = xml[search_pos..].find("<class ") {
        let abs_start = search_pos + start;
        if let Some(end) = xml[abs_start..].find("</class>") {
            let abs_end = abs_start + end + "</class>".len();
            blocks.push(xml[abs_start..abs_end].to_string());
            search_pos = abs_end;
        } else {
            break;
        }
    }
    blocks
}

fn extract_attr(xml: &str, attr: &str) -> Option<String> {
    let pat = format!("{attr}=\"");
    let start = xml.find(&pat)? + pat.len();
    let end = xml[start..].find('"')?;
    Some(xml[start..start + end].to_string())
}

fn extract_lines(class_xml: &str) -> Vec<(u32, u32)> {
    let mut lines = Vec::new();
    let mut search_pos = 0;
    while let Some(line_start) = class_xml[search_pos..].find("<line ") {
        let abs = search_pos + line_start;
        let line_end = class_xml[abs..]
            .find("/>")
            .map(|e| abs + e + 2)
            .unwrap_or(class_xml.len());
        let line_xml = &class_xml[abs..line_end];
        if let (Some(num), Some(hits)) = (
            extract_attr(line_xml, "number"),
            extract_attr(line_xml, "hits"),
        ) {
            if let (Ok(n), Ok(h)) = (num.parse::<u32>(), hits.parse::<u32>()) {
                lines.push((n, h));
            }
        }
        search_pos = line_end;
    }
    lines
}

fn parse_overall_rate(xml: &str) -> f64 {
    extract_attr(xml, "line-rate")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
        * 100.0
}

fn compute_file_coverage(raw: &RawClass) -> FileCoverage {
    let lines_total = raw.lines.len() as u32;
    let lines_covered = raw.lines.iter().filter(|(_, h)| *h > 0).count() as u32;
    let rate = if lines_total > 0 {
        (lines_covered as f64 / lines_total as f64) * 100.0
    } else {
        0.0
    };
    let uncovered: Vec<u32> = raw
        .lines
        .iter()
        .filter(|(_, h)| *h == 0)
        .map(|(n, _)| *n)
        .collect();
    let uncovered_ranges = group_consecutive(&uncovered);

    FileCoverage {
        path: raw.filename.clone(),
        lines_total,
        lines_covered,
        rate,
        uncovered_ranges,
    }
}

fn group_consecutive(lines: &[u32]) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    let mut start: Option<u32> = None;
    let mut prev: Option<u32> = None;

    for &n in lines {
        if let Some(s) = start {
            if let Some(p) = prev {
                if n == p + 1 {
                    prev = Some(n);
                    continue;
                } else {
                    ranges.push((s, p));
                    start = Some(n);
                    prev = Some(n);
                }
            }
        } else {
            start = Some(n);
            prev = Some(n);
        }
    }
    if let (Some(s), Some(p)) = (start, prev) {
        ranges.push((s, p));
    }
    ranges
}

fn compute_dir_coverage(per_file: &[FileCoverage]) -> Vec<DirCoverage> {
    DEFAULT_THRESHOLDS
        .iter()
        .map(|(dir, threshold)| {
            let files: Vec<&FileCoverage> = per_file
                .iter()
                .filter(|f| f.path.starts_with(dir))
                .collect();
            let lines_total: u32 = files.iter().map(|f| f.lines_total).sum();
            let lines_covered: u32 = files.iter().map(|f| f.lines_covered).sum();
            let rate = if lines_total > 0 {
                (lines_covered as f64 / lines_total as f64) * 100.0
            } else {
                0.0
            };
            let headroom = rate - threshold;
            DirCoverage {
                dir: dir.to_string(),
                lines_total,
                lines_covered,
                rate,
                threshold: *threshold,
                headroom,
            }
        })
        .collect()
}

pub fn print_report(gaps: &CoverageGaps) {
    println!("Coverage report (overall: {:.1}%):\n", gaps.overall_rate);

    for d in &gaps.per_dir {
        let status = if d.headroom >= 0.0 { "OK" } else { "BELOW" };
        println!(
            "  {dir:<20} {rate:.1}% (threshold {threshold:.0}%, headroom {headroom:+.1}%) [{status}]",
            dir = d.dir,
            rate = d.rate,
            threshold = d.threshold,
            headroom = d.headroom,
            status = status,
        );
    }
    println!();

    let low: Vec<&FileCoverage> = gaps.per_file.iter().take(20).collect();
    println!("Lowest-covered files (top 20):");
    for f in low {
        let ranges: Vec<String> = f
            .uncovered_ranges
            .iter()
            .map(|(s, e)| {
                if s == e {
                    format!("{s}")
                } else {
                    format!("{s}-{e}")
                }
            })
            .collect();
        println!(
            "  {path:<50} {rate:.1}%  [{ranges}]",
            path = f.path,
            rate = f.rate,
            ranges = ranges.join(", "),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_xml() -> &'static str {
        r#"<?xml version="1.0"?>
<coverage line-rate="0.51">
  <packages>
    <package>
      <classes>
        <class filename="src/session/foo.rs" line-rate="0.40">
          <lines>
            <line number="1" hits="1"/>
            <line number="2" hits="1"/>
            <line number="10" hits="0"/>
            <line number="11" hits="0"/>
            <line number="12" hits="0"/>
            <line number="20" hits="1"/>
            <line number="21" hits="0"/>
          </lines>
        </class>
        <class filename="src/tools/bar.rs" line-rate="0.75">
          <lines>
            <line number="1" hits="1"/>
            <line number="2" hits="1"/>
            <line number="3" hits="1"/>
            <line number="4" hits="0"/>
          </lines>
        </class>
      </classes>
    </package>
  </packages>
</coverage>"#
    }

    #[test]
    fn parse_two_classes() {
        let classes = parse_cobertura(fixture_xml()).unwrap();
        assert_eq!(classes.len(), 2);
        assert_eq!(classes[0].filename, "src/session/foo.rs");
        assert_eq!(classes[0].lines.len(), 7);
        assert_eq!(classes[1].filename, "src/tools/bar.rs");
    }

    #[test]
    fn compute_file_coverage_rates() {
        let classes = parse_cobertura(fixture_xml()).unwrap();
        let fc = compute_file_coverage(&classes[0]);
        assert_eq!(fc.lines_total, 7);
        assert_eq!(fc.lines_covered, 3);
        assert!((fc.rate - 42.86).abs() < 0.1);
    }

    #[test]
    fn group_consecutive_lines() {
        let classes = parse_cobertura(fixture_xml()).unwrap();
        let fc = compute_file_coverage(&classes[0]);
        // Uncovered: 10, 11, 12, 21
        assert_eq!(fc.uncovered_ranges, vec![(10, 12), (21, 21)]);
    }

    #[test]
    fn dir_coverage_aggregates() {
        let classes = parse_cobertura(fixture_xml()).unwrap();
        let per_file: Vec<FileCoverage> = classes.iter().map(compute_file_coverage).collect();
        let dirs = compute_dir_coverage(&per_file);
        assert_eq!(dirs.len(), 3); // session, tools, adapters

        let session = dirs.iter().find(|d| d.dir == "src/session").unwrap();
        assert_eq!(session.lines_total, 7);
        assert_eq!(session.lines_covered, 3);

        let tools = dirs.iter().find(|d| d.dir == "src/tools").unwrap();
        assert_eq!(tools.lines_total, 4);
        assert_eq!(tools.lines_covered, 3);
    }

    #[test]
    fn overall_rate_parsed() {
        let rate = parse_overall_rate(fixture_xml());
        assert!((rate - 51.0).abs() < 0.01);
    }

    #[test]
    fn empty_xml_errors() {
        let result = parse_cobertura("");
        assert!(result.is_err());
    }

    #[test]
    fn analyze_gaps_from_file() {
        let tmp = std::env::temp_dir().join("testdoctor-gaps-test.xml");
        std::fs::write(&tmp, fixture_xml()).unwrap();
        let gaps = analyze_gaps(&tmp).unwrap();
        assert_eq!(gaps.per_file.len(), 2);
        assert!(gaps.per_file[0].rate <= gaps.per_file[1].rate);
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn group_consecutive_empty() {
        assert!(group_consecutive(&[]).is_empty());
    }

    #[test]
    fn group_consecutive_single() {
        assert_eq!(group_consecutive(&[5]), vec![(5, 5)]);
    }

    #[test]
    fn group_consecutive_all_consecutive() {
        assert_eq!(group_consecutive(&[1, 2, 3, 4, 5]), vec![(1, 5)]);
    }

    #[test]
    fn group_consecutive_gaps() {
        assert_eq!(
            group_consecutive(&[1, 2, 5, 6, 10]),
            vec![(1, 2), (5, 6), (10, 10)]
        );
    }

    #[test]
    fn extract_attr_finds_value() {
        let xml = r#"<class filename="foo.rs" line-rate="0.5"/>"#;
        assert_eq!(extract_attr(xml, "filename"), Some("foo.rs".into()));
        assert_eq!(extract_attr(xml, "line-rate"), Some("0.5".into()));
    }

    #[test]
    fn extract_attr_missing_returns_none() {
        let xml = r#"<class filename="foo.rs"/>"#;
        assert_eq!(extract_attr(xml, "missing"), None);
    }

    #[test]
    #[ignore]
    // ponytail: #[ignore] until coverage-gate targets dict is restored in ci.yml — upgrade path: remove #[ignore] when coverage gate lands (WO 26.x or manual).
    fn default_thresholds_match_ci_yml() {
        // Drift guard: DEFAULT_THRESHOLDS must match the thresholds
        // enforced by the coverage gate in .github/workflows/ci.yml.
        // Parses the gate's `targets = { ... }` dict so a bump on one
        // side that forgets the other fails here instead of drifting.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let ci_yml = manifest
            .join("..")
            .join("..")
            .join(".github")
            .join("workflows")
            .join("ci.yml");
        let text = std::fs::read_to_string(&ci_yml)
            .unwrap_or_else(|e| panic!("read {}: {e}", ci_yml.display()));
        let line = text
            .lines()
            .find(|l| l.contains("targets = {"))
            .expect("ci.yml coverage-gate `targets = {` line not found");
        let start = line.find('{').expect("`{` in targets dict");
        let end = line.find('}').expect("`}` in targets dict");
        let dict = &line[start + 1..end];

        let mut ci: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
        for entry in dict.split(',') {
            let mut parts = entry.split(':');
            let key = parts
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('\'')
                .trim_matches('"');
            if let (false, Ok(v)) = (
                key.is_empty(),
                parts.next().unwrap_or("").trim().parse::<f64>(),
            ) {
                ci.insert(key, v);
            }
        }
        assert!(
            !ci.is_empty(),
            "parsed no thresholds from ci.yml targets dict"
        );

        for (dir, threshold) in DEFAULT_THRESHOLDS {
            let ci_val = ci
                .get(*dir)
                .copied()
                .unwrap_or_else(|| panic!("ci.yml coverage gate has no threshold for `{dir}`"));
            assert!(
                (ci_val - threshold).abs() < 1e-9,
                "DEFAULT_THRESHOLDS[{dir}] = {threshold} but ci.yml coverage gate = {ci_val} (drift)",
            );
        }
    }
}
