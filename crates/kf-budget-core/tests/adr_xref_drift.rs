//! ADR-0016 § drift test #5 — ADR cross-reference drift.
//!
//! Two checks:
//!
//! 1. Every `[NNNN](./NNNN-title.md)` link in `docs/adr/README.md`'s
//!    Index table resolves to an existing file. A contributor who
//!    renames or removes an ADR without updating the index fails
//!    CI before a reader hits a dead link.
//! 2. The total count of Status headers across `docs/adr/*.md` (for
//!    ADRs indexed in the table) matches the per-status entries
//!    listed in the Index table. Handles both header formats: the
//!    bullet form `- **Status:** X` and the heading form `## Status`.
//!
//! ponytail: the parser is a handful of `split('|')` lines. A full
//! markdown AST would let us catch nested references, but the
//! index table is the only place ADR cross-refs surface today —
//! pulling in `regex` for one call site is YAGNI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Repo root is the grandparent of `CARGO_MANIFEST_DIR`. Tests run
/// from `crates/kf-budget-core/tests/`, so the manifest is
/// `crates/kf-budget-core/Cargo.toml` and the workspace root sits
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

/// Walk every ADR file and collect `(num, file, status)` records.
///
/// ponytail: the merged `docs/adr/` dir holds two naming conventions
/// side by side — 4-digit Plugin3 ADRs (`0047-kf-budget-fold-in.md`) and
/// 3-digit native CLI ADRs (`046-stratum-fold-in.md`). A digit-prefix
/// filter like `stem[..4].all_ascii_digit()` is blind to the 3-digit
/// scheme (the 4th char is `-`, not a digit), so it silently drops
/// recent ADRs from the count. We take the leading digit run instead,
/// which handles both, and count every non-README `.md` file.
fn collect_adr_records(dir: &Path) -> Vec<(String, String, String)> {
    let mut records = Vec::new();
    let entries = std::fs::read_dir(dir).expect("docs/adr/ readable");
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let file = p.file_name().unwrap().to_string_lossy().to_string();
        if file == "README.md" {
            continue;
        }
        let num = adr_number_from_stem(&file)
            .unwrap_or_else(|| panic!("ADR {file} has no leading digit run in its filename"));
        let body = std::fs::read_to_string(&p).expect("ADR readable");
        let status = parse_status_header(&body)
            .unwrap_or_else(|| panic!("ADR {file} missing a parseable Status header"));
        records.push((num, file, status));
    }
    records
}

/// Pull the leading digit run from an ADR filename stem. Handles both
/// the 3-digit scheme (`046-stratum-fold-in` → `046`) and the 4-digit
/// scheme (`0047-kf-budget-fold-in` → `0047`).
fn adr_number_from_stem(stem: &str) -> Option<String> {
    let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// Parse the Status header from an ADR body. The merged dir uses three
/// header formats:
///
///   - `- **Status:** X`  (bullet + bold label, value inline)
///   - `**Status:** X`    (bold label, no bullet, value inline)
///   - `## Status`        (heading, blank line, then value on its own line)
///
/// ponytail: a single `strip_prefix` only catches the first form and
/// was the original blind spot — ADRs 043/046/048/049 use the heading
/// form and 030/035/036/037 use the bare-bold form, so they were
/// silently dropped. We try all three and return the first match.
fn parse_status_header(body: &str) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("- **Status:**") {
            return Some(rest.trim().to_string());
        }
        if let Some(rest) = t.strip_prefix("**Status:**") {
            return Some(rest.trim().to_string());
        }
        if t == "## Status" {
            // Value sits on the next non-empty line after the heading.
            for nxt in lines.iter().skip(i + 1) {
                let nt = nxt.trim();
                if !nt.is_empty() {
                    return Some(nt.to_string());
                }
            }
        }
    }
    None
}

/// Tally statuses for the ADRs whose number appears in the Index table.
/// The table only indexes the Plugin3 4-digit series; the 3-digit
/// native CLI ADRs live in a bulleted list and are out of scope for the
/// table-vs-file count agreement, but `collect_adr_records` still
/// validates they carry a parseable Status header.
fn count_statuses_for_table(
    records: &[(String, String, String)],
    table_nums: &std::collections::BTreeSet<String>,
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for (num, _file, status) in records {
        if table_nums.contains(num) {
            *counts.entry(status.clone()).or_insert(0) += 1;
        }
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
    let mut table_nums = std::collections::BTreeSet::new();
    for (num, _file, _title, status) in &rows {
        *table_counts.entry(status.clone()).or_insert(0) += 1;
        table_nums.insert(num.clone());
    }
    let records = collect_adr_records(&adr_dir());
    let file_counts = count_statuses_for_table(&records, &table_nums);
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
    let mut table_nums = std::collections::BTreeSet::new();
    for (num, _file, _title, _status) in &rows {
        table_nums.insert(num.clone());
    }
    let records = collect_adr_records(&adr_dir());
    let file_counts = count_statuses_for_table(&records, &table_nums);
    let file_deferred = file_counts.get("Deferred").copied().unwrap_or(0);
    assert_eq!(
        deferred, file_deferred,
        "deferred count disagrees: index lists {deferred}, files = {file_deferred}"
    );
}

// ponytail: WO 22.12 requested extending this test to assert that every
// crates/X path literal in ADR prose references an actual directory. The
// ADR prose was fixed (28 files, commit ce3518b) but the path-literal check
// itself was never implemented. Implemented in WO 25.10 (R2) below.

/// Extract unique `crates/X` crate-dir references from a line of ADR prose.
/// Only captures the first path segment after `crates/` (i.e. the crate name).
/// Skips lines inside code fences (tracked by the `in_fence` parameter).
fn extract_crate_refs(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(idx) = rest.find("crates/") {
        let after = &rest[idx + 7..];
        let name: &str = after.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_').next().unwrap_or("");
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
        rest = after;
    }
    out
}

/// Strip code-fenced blocks (triple backtick) from ADR body, returning
/// only the prose lines. ceiling: code-fenced examples may reference
/// hypothetical or future paths that don't yet exist on disk; skipping
/// them avoids false positives from aspirational ADR sections.
/// Upgrade: if we want to validate fenced paths too, gate them behind
/// a status check (only enforce for "Accepted" ADRs).
fn prose_lines(body: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            lines.push(line);
        }
    }
    lines
}

#[test]
fn adr_path_literals_reference_existing_crates() {
    // WO 25.10 R2: every `crates/X` reference in ADR prose (outside code
    // fences) must point to a directory that exists under `crates/`.
    let dir = adr_dir();
    let entries = std::fs::read_dir(&dir).expect("docs/adr/ readable");
    let crates_dir = repo_root().join("crates");
    let mut failures = Vec::new();

    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let file = p.file_name().unwrap().to_string_lossy().to_string();
        if file == "README.md" {
            continue;
        }
        let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("ADR {file} readable: {e}"));
        for line in prose_lines(&body) {
            for crate_name in extract_crate_refs(line) {
                let crate_path = crates_dir.join(crate_name);
                if !crate_path.is_dir() {
                    failures.push(format!("{file}: crates/{crate_name} (dir does not exist)"));
                }
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "ADR path-literal violations:\n{}",
            failures.join("\n")
        );
    }
}
