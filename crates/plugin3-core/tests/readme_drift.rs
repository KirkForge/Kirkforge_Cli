//! ADR-0016 § Implementation notes prescribes drift tests for
//! every cross-reference that lives outside the code. The ADR
//! index already has one (`adr_xref_drift.rs`); the README's
//! "State" table is a second source-of-truth that drifts the
//! same way — a contributor who adds a test forgets to bump
//! "85 passing" to "86 passing" and the gap grows silently.
//!
//! ponytail: one walk + one parse. Counting `#[test]` markers
//! under `crates/` is enough — inline tests in `src/*.rs` and
//! integration tests in `tests/*.rs` both annotate with the same
//! attribute, and a `#[cfg(test)] mod tests` block does not
//! inflate the count (the mod itself has no `#[test]`). No
//! `regex`, no `walkdir` — `std::fs::read_dir` recurses once per
//! level. Run cargo itself if you need an authoritative count;
//! this drift test catches the common case where the README and
//! the test suite disagree by more than a handful.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // crates/
        .and_then(Path::parent) // workspace root
        .expect("workspace root resolvable")
        .to_path_buf()
}

/// Walk a directory recursively, returning every regular file's
/// path. Skips `target/` (compiled artefacts) so a stale build
/// doesn't pollute the count.
fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().and_then(|s| s.to_str()) == Some("target") {
                continue;
            }
            walk_rs(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

fn count_test_attrs(files: &[PathBuf]) -> usize {
    let mut total = 0;
    for f in files {
        let Ok(body) = std::fs::read_to_string(f) else {
            continue;
        };
        // ponytail: literal substring scan. We match `#[test]` on
        // its own line and require the next non-blank line to
        // start with `fn` — that filters both comment mentions
        // (`// #[test]`) and stray attribute references that
        // never bind to a function. cargo's test discovery uses
        // attribute-then-fn semantics; the only idiom in this
        // codebase is `#[test]` on the line before `fn name`, so
        // we accept that single layout.
        let lines: Vec<&str> = body.lines().collect();
        for i in 0..lines.len() {
            let trimmed = lines[i].trim_start();
            if !trimmed.starts_with("#[test]") {
                continue;
            }
            if let Some(next) = lines.get(i + 1) {
                if next.trim_start().starts_with("fn ") {
                    total += 1;
                }
            }
        }
    }
    total
}

fn parse_readme_test_count(readme: &str) -> Option<usize> {
    // ponytail: README row format is `| Tests     | <N> passing ... |`.
    // Match the leading `| Tests` cell, then read the next pipe-
    // delimited field and pull the leading integer. Anything after
    // the integer is decoration.
    for line in readme.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("| Tests") {
            continue;
        }
        let inner = trimmed.trim_matches('|');
        let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
        if cells.len() < 2 {
            continue;
        }
        let cell = cells[1];
        let digits: String = cell.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        return digits.parse().ok();
    }
    None
}

#[test]
fn readme_test_count_matches_test_attributes() {
    let root = repo_root();
    let readme_path = root.join("README.md");
    let readme =
        std::fs::read_to_string(&readme_path).unwrap_or_else(|e| panic!("read README.md: {e}"));

    let claimed = parse_readme_test_count(&readme)
        .unwrap_or_else(|| panic!("README 'Tests | N passing' row missing or unparseable"));

    let mut files = Vec::new();
    walk_rs(&root.join("crates"), &mut files);
    let actual = count_test_attrs(&files);

    // ponytail: allow a small fudge because the README "passing"
    // number can lag a PR by one or two tests (a contributor adds
    // the test, forgets the README bump, and the next round is the
    // one that notices). A test that asserts equality to the byte
    // would be brittle on every additive commit. A 2-test window
    // catches the silent-drift case (the README stuck at N while
    // the suite grows to N+5) without forcing an update on every
    // new test. Tighten to exact equality once the README is
    // generated rather than hand-edited. The earlier 5-test fudge
    // was too generous — contributors could land 4 of every 5
    // commits without bumping the README and the drift would
    // accumulate silently.
    let drift = claimed.abs_diff(actual);
    assert!(
        drift <= 2,
        "README says {claimed} passing tests but #[test] count under crates/ is {actual} (drift {drift}) — \
         update README.md State table or run `cargo test --workspace` to confirm the real count",
    );
}

#[test]
fn readme_test_count_row_present() {
    // ponytail: a contributor who deletes the row silently breaks
    // the drift test above with a panic rather than a stale-pass.
    // This test pins the row's *presence* so the panic message is
    // actionable.
    let root = repo_root();
    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md readable");
    assert!(
        readme.lines().any(|l| l.trim().starts_with("| Tests")),
        "README.md State table is missing the '| Tests | N passing' row",
    );
}
