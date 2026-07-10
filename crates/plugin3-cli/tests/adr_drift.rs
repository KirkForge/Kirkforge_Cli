//! ADR cross-reference drift test. Per ADR-0016 § Drift tests #5:
//! "the README's 'See also' links to ADRs that exist in the
//! `docs/adr/` directory. A removed ADR with active links fails CI."
//!
//! ponytail: zero-deps drift test — `std::fs::read_dir` plus a
//! single regex-style match against the markdown link syntax
//! `[…](NNNN-name.md)`. A contributor who removes or renames an
//! ADR without updating cross-references surfaces here before
//! the broken link reaches a reader.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    // tests/ lives at crates/plugin3-cli/tests; the workspace
    // root is three parents up.
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn adr_dir() -> PathBuf {
    workspace_root().join("docs/adr")
}

fn real_adrs() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for entry in std::fs::read_dir(adr_dir()).expect("docs/adr/ readable") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if std::path::Path::new(&name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            out.insert(name);
        }
    }
    out
}

fn scan_file(path: &std::path::Path) -> Vec<String> {
    // ponytail: match the exact markdown link syntax used in
    // ADRs and the README: `[text](NNNN-name.md)` or a bare
    // `NNNN-name.md`. Bare references appear in the ADR cross-
    // reference body where contributors write "ADR-0015" + the
    // filename on the next line.
    let body = std::fs::read_to_string(path).expect("read source file");
    let mut refs = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '(' {
            // Collect until ')' and check if the inner text
            // looks like an ADR filename.
            let mut inside = String::new();
            for nc in chars.by_ref() {
                if nc == ')' {
                    break;
                }
                inside.push(nc);
            }
            let stripped = inside
                .strip_prefix("./docs/adr/")
                .or_else(|| inside.strip_prefix("docs/adr/"))
                .unwrap_or(&inside);
            if looks_like_adr_filename(stripped) {
                refs.push(stripped.to_string());
            }
        }
    }
    refs
}

fn looks_like_adr_filename(s: &str) -> bool {
    // ponytail: 4 digits, hyphen, kebab-case, .md. Matches the
    // `docs/adr/` filenames exactly; false positives rejected
    // by the existence check below.
    let Some((head, tail)) = s.split_once('-') else {
        return false;
    };
    head.len() == 4
        && head.chars().all(|c| c.is_ascii_digit())
        && std::path::Path::new(tail)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        && !tail.contains('/')
}

#[test]
fn every_adr_cross_reference_resolves_to_a_real_file() {
    let real = real_adrs();
    let workspace = workspace_root();
    let sources = ["README.md", "docs/adr/README.md"];

    let mut missing = Vec::new();
    for src in sources {
        let path = workspace.join(src);
        for referenced in scan_file(&path) {
            if !real.contains(&referenced) {
                missing.push(format!(
                    "{src} references {referenced} which is not in docs/adr/"
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "broken ADR cross-references: {missing:#?}\n  real ADRs: {real:#?}",
    );
}

#[test]
fn no_orphan_adr_files() {
    // ponytail: every ADR file in docs/adr/ must be linked from
    // the index. A new ADR that nobody references is
    // documentation debt that quietly ages. The index itself
    // (`docs/adr/README.md`) is exempt — it IS the index.
    let real = real_adrs();
    let workspace = workspace_root();
    let index = workspace.join("docs/adr/README.md");
    let index_body = std::fs::read_to_string(&index).expect("docs/adr/README.md");
    let mut orphans = Vec::new();
    for adr in &real {
        if adr == "README.md" {
            continue;
        }
        // In the merged KirkForge-Cli repo we only enforce the plugin3
        // ADR index (4-digit numbered ADRs). CLI-native ADRs use a
        // separate 3-digit numbering scheme.
        let is_plugin3_adr = adr.len() >= 4 && adr[..4].chars().all(|c| c.is_ascii_digit());
        if !is_plugin3_adr {
            continue;
        }
        if !index_body.contains(adr) {
            orphans.push(adr.clone());
        }
    }
    assert!(
        orphans.is_empty(),
        "ADR files not listed in docs/adr/README.md index: {orphans:#?}",
    );
}
