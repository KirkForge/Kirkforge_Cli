//! ADR-0006 § Implementation notes — regression test pinning
//! the detector's output for a known corpus of (`tool_name`, input)
//! pairs. A contributor who tweaks `from_tool_name` or `from_shape`
//! surfaces the change here, not via silent mis-classification in
//! the user's terminal.
//!
//! ponytail: zero-deps loader — reads two TSV files at
//! `tests/fixtures/detector/{by_tool_name,by_shape}.tsv`. Each
//! non-comment line is `<kind>\t<input>` (shape) or
//! `<tool_name>\t<kind>\t<input>` (tool-name). The shape file's
//! input may span multiple lines (the test reassembles them with
//! `\n` joins), letting a contributor write a realistic cargo-test
//! header without escaping every line.

use std::path::PathBuf;

use plugin3_core::detector::{detect, ToolOutputKind};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/detector")
}

fn parse_kind(s: &str) -> ToolOutputKind {
    match s {
        "TestRunner" => ToolOutputKind::TestRunner,
        "Compiler" => ToolOutputKind::Compiler,
        "BuildLog" => ToolOutputKind::BuildLog,
        "GenericShell" => ToolOutputKind::GenericShell,
        "SearchResults" => ToolOutputKind::SearchResults,
        "FileContent" => ToolOutputKind::FileContent,
        "Json" => ToolOutputKind::Json,
        "Unknown" => ToolOutputKind::Unknown,
        other => panic!("fixture uses unknown kind {other:?}; add it to the parser"),
    }
}

fn load_by_tool_name() -> Vec<(String, ToolOutputKind, String)> {
    let path = fixture_dir().join("by_tool_name.tsv");
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out = Vec::new();
    for (lineno, raw) in body.lines().enumerate() {
        if raw.starts_with('#') || raw.is_empty() {
            continue;
        }
        let mut cols = raw.splitn(3, '\t');
        let tool = cols.next().unwrap_or("").to_string();
        let kind = parse_kind(cols.next().unwrap_or(""));
        let input = cols.next().unwrap_or("").to_string();
        assert!(
            !tool.is_empty(),
            "{}:{}: empty tool_name",
            path.display(),
            lineno + 1
        );
        out.push((tool, kind, input));
    }
    out
}

fn load_by_shape() -> Vec<(ToolOutputKind, String)> {
    let path = fixture_dir().join("by_shape.tsv");
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // ponytail: shape fixtures may span multiple lines after the
    // initial `kind\t`. The first line is `<kind>\t<line1>`; any
    // subsequent non-comment, non-blank line is appended to the
    // same input with a `\n` join. This lets a contributor paste
    // a real `cargo test` header without escaping.
    let mut out = Vec::new();
    let mut current: Option<(ToolOutputKind, Vec<String>)> = None;
    for (lineno, raw) in body.lines().enumerate() {
        if raw.starts_with('#') {
            continue;
        }
        if raw.is_empty() {
            if let Some((k, lines)) = current.take() {
                out.push((k, lines.join("\n")));
            }
            continue;
        }
        if let Some((k, first)) = raw.split_once('\t') {
            if let Some(prev) = current.take() {
                out.push((prev.0, prev.1.join("\n")));
            }
            current = Some((parse_kind(k), vec![first.to_string()]));
        } else if let Some((k, lines)) = current.as_mut() {
            // promote: leading line had no kind? bail out
            let _ = (k, lines);
            panic!(
                "{}:{}: shape line without kind prefix: {raw:?}",
                path.display(),
                lineno + 1
            );
        }
    }
    if let Some((k, lines)) = current.take() {
        out.push((k, lines.join("\n")));
    }
    out
}

#[test]
fn tool_name_layer_matches_fixture_corpus() {
    let cases = load_by_tool_name();
    assert!(!cases.is_empty(), "fixture corpus is empty — add cases");
    let mut mismatches = Vec::new();
    for (tool, expected, input) in &cases {
        let got = detect(input, Some(tool));
        if got != *expected {
            mismatches.push(format!(
                "tool={tool:?} expected={expected:?} got={got:?} input={input:?}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "detector regressions ({} of {}):\n  {}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n  "),
    );
}

#[test]
fn shape_layer_matches_fixture_corpus() {
    let cases = load_by_shape();
    assert!(!cases.is_empty(), "fixture corpus is empty — add cases");
    let mut mismatches = Vec::new();
    for (expected, input) in &cases {
        let got = detect(input, None);
        if got != *expected {
            mismatches.push(format!("expected={expected:?} got={got:?} input={input:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "shape regressions ({} of {}):\n  {}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n  "),
    );
}

#[test]
fn fixture_files_are_present() {
    // ponytail: the test above silently no-ops if the fixture file
    // is missing — `read_to_string` would panic with a path message
    // that names the missing file, but a contributor who refactors
    // the path would see the panic and fix it. Belt + suspenders:
    // also assert the directory has both expected files so a typo
    // in the fixture directory surfaces here too.
    let dir = fixture_dir();
    for name in ["by_tool_name.tsv", "by_shape.tsv"] {
        assert!(
            dir.join(name).is_file(),
            "missing fixture {name} — the regression test corpus must live in tests/fixtures/detector/",
        );
    }
}
