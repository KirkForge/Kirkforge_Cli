//! ADR-0016 § drift test #5 — ADR cross-reference drift.
//!
//! Two checks:
//!
//! 1. Every `[NNNN](./NNNN-title.md)` link in `docs/adr/README.md`'s
//!    Index table resolves to an existing file. A contributor who
//!    renames or removes an ADR without updating the index fails
//!    CI before a reader hits a dead link.
//! 2. The total count of `- **Status:** Accepted` (and
//!    `Deferred`) headers across `docs/adr/*.md` matches the
//!    per-status entries listed in the Index table.
//!
//! ponytail: the parser is a handful of `split('|')` lines. A full
//! markdown AST would let us catch nested references, but the
//! index table is the only place ADR cross-refs surface today —
//! pulling in `regex` for one call site is YAGNI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Repo root is the grandparent of `CARGO_MANIFEST_DIR`. Tests run
/// from `crates/plugin3-core/tests/`, so the manifest is
/// `crates/plugin3-core/Cargo.toml` and the workspace root sits
/// two levels up.
fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // crates/
        .and_then(Path::parent) // workspace root
        .expect("workspace root resolvable")
        .to_path_buf()
}

fn adr_dir() -> PathBuf {
    repo_root().join("docs").join("adr")
}

/// Parse every `(num, file, title, status)` quad from the Index
/// table in `docs/adr/README.md`. Format per ADR-0016:
///
/// ```text
/// | [0001](./0001-purpose.md) | Purpose | Accepted |
/// ```
///
/// ponytail: we tokenise on `|` then strip `[…](./…)` markdown
/// link wrapping. Three `trim_*` helpers and one match — no regex.
fn parse_index_table(readme: &str) -> Vec<(String, String, String, String)> {
    let mut rows = Vec::new();
    for line in readme.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        // Strip outer pipes and split on `|`.
        // Strip outer pipes; remaining `|`s separate cells.
        // Source row `| A | B | C |` becomes 3 cells after stripping
        // both outer pipes (the inner has two separators).
        let inner = trimmed.trim_matches('|');
        let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
        // Cell 0: `[NNNN](./NNNN-title.md)`. Cell 1: title. Cell 2: status.
        if cells.len() < 3 {
            continue;
        }
        let Some((num, file)) = parse_link_cell(cells[0]) else {
            continue;
        };
        let title = cells[1].to_string();
        let status = cells[2].to_string();
        rows.push((num, file, title, status));
    }
    rows
}

/// Pull `NNNN` and `NNNN-title.md` out of `[NNNN](./NNNN-title.md)`.
fn parse_link_cell(cell: &str) -> Option<(String, String)> {
    let cell = cell.trim();
    // `[NNNN](./NNNN-title.md)` — strip the surrounding `[…]`.
    let after_open = cell.strip_prefix('[')?;
    let (num, rest) = after_open.split_once("](./")?;
    let file = rest.strip_suffix(')')?;
    Some((num.to_string(), file.to_string()))
}

/// Walk every ADR file and tally its declared `- **Status:** X` header.
fn count_statuses(dir: &Path) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let entries = std::fs::read_dir(dir).expect("docs/adr/ readable");
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let stem = p.file_name().unwrap().to_string_lossy().to_string();
        if stem == "README.md" {
            continue;
        }
        // In the merged repo we only validate plugin3's 4-digit ADRs.
        // CLI-native ADRs use a 3-digit scheme and a different header format.
        let is_plugin3_adr = stem.len() >= 4 && stem[..4].chars().all(|c| c.is_ascii_digit());
        if !is_plugin3_adr {
            continue;
        }
        let body = std::fs::read_to_string(&p).expect("ADR readable");
        let status = body
            .lines()
            .find_map(|l| l.strip_prefix("- **Status:** ").map(str::trim))
            .unwrap_or_else(|| panic!("ADR {stem} missing `- **Status:**` header"));
        *counts.entry(status.to_string()).or_insert(0) += 1;
    }
    counts
}

#[test]
fn index_table_links_resolve_to_existing_adrs() {
    // ponytail: the canonical ADR index lives at docs/adr/README.md.
    // A new ADR appended without an Index row is a hidden ADR —
    // nobody reads docs/adr/0007-foo.md unless the table links to it.
    let readme =
        std::fs::read_to_string(adr_dir().join("README.md")).expect("docs/adr/README.md exists");
    let rows = parse_index_table(&readme);
    assert!(
        !rows.is_empty(),
        "no index rows parsed — table format drifted?"
    );

    for (num, file, _title, _status) in &rows {
        let p = adr_dir().join(file);
        assert!(
            p.exists(),
            "Index links {num} -> {file}, but {p:?} is missing"
        );
        // Link text and filename prefix must agree so a renamed
        // file without updating the link text fails here too.
        assert!(
            file.starts_with(&format!("{num}-")),
            "ADR {num} links to {file} whose filename prefix disagrees"
        );
    }
}

#[test]
fn status_counts_match_index_table_summary() {
    // ponytail: two sources of truth — the Index rows (one per
    // status entry) and the file scan (one Status header per
    // ADR). When they disagree, someone added an ADR without
    // updating the table, or removed one without pruning a row.
    let readme =
        std::fs::read_to_string(adr_dir().join("README.md")).expect("docs/adr/README.md exists");
    let rows = parse_index_table(&readme);

    let mut table_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_num, _file, _title, status) in &rows {
        *table_counts.entry(status.clone()).or_insert(0) += 1;
    }
    let file_counts = count_statuses(&adr_dir());
    assert_eq!(
        table_counts, file_counts,
        "ADR Index table summary disagrees with file Status headers:\n\
         index table:  {table_counts:?}\n\
         file headers: {file_counts:?}"
    );
}

#[test]
fn deferred_adrs_consistent_between_index_and_files() {
    // ponytail: a third source of truth is the README "## State"
    // table which says "14 Accepted, 2 Deferred (0011, 0012)".
    // The parenthetical list is what catches the eye; if it
    // diverges from the Index, a contributor deferred/un-deferred
    // an ADR without updating both. We pin by *count agreement*
    // (file scan vs index table) — the exact parenthetical is a
    // doc-only fact we leave to manual review.
    let readme =
        std::fs::read_to_string(adr_dir().join("README.md")).expect("docs/adr/README.md exists");
    let rows = parse_index_table(&readme);
    let deferred: usize = rows.iter().filter(|(_, _, _, s)| s == "Deferred").count();
    let file_counts = count_statuses(&adr_dir());
    let file_deferred = file_counts.get("Deferred").copied().unwrap_or(0);
    assert_eq!(
        deferred, file_deferred,
        "deferred count disagrees: index lists {deferred}, files = {file_deferred}"
    );
}
