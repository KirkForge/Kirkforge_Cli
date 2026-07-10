//! ADR-0015 (CLI precedence chain) drift test — the precedence
//! order is load-bearing: it dictates which source wins when
//! multiple are set. The precedence string appears in five
//! doc surfaces that must all agree:
//!
//! 1. ADR-0015 § Context bullet (docs/adr/0015-cli-design.md)
//! 2. ADR-0015 § Precedence chain header
//! 3. ADR-0015 § Consequences Positives bullet
//! 4. `precedence.rs` module docstring (crates/plugin3-cli/src/precedence.rs)
//! 5. ADR-0002 file-tree annotation (docs/adr/0002-workspace.md)
//!
//! A contributor who inverts the prose to "env > CLI > file"
//! in any one of those five places without inverting the impl
//! would document a phantom order that contradicts
//! `precedence::resolve_config_path` and its four in-file tests.
//!
//! ponytail: five literal-substring scans that pin the spelling
//! the impl/tests agree on. No markdown parser — the docs own
//! the exact wording, and `contains` catches the silent
//! regressions (a contributor who re-types the order from
//! memory and accidentally swaps CLI/env in any one of the
//! five doc surfaces fails CI before the wrong precedence
//! reaches a reader).

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // crates/
        .and_then(Path::parent) // workspace root
        .expect("workspace root resolvable")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ponytail: pin the ADR-0015 § Context bullet. The Context
// section enumerates five CLI design properties; the
// precedence bullet must name CLI as the highest source so
// the ADR's prose matches `resolve_config_path` (which checks
// cli first) and the in-file tests (which pin CLI > env > XDG).
#[test]
fn adr_0015_context_bullet_names_cli_first() {
    let adr = read(&repo_root().join("docs/adr/0015-cli-design.md"));
    let context_section_start = adr
        .find("## Context")
        .expect("ADR-0015 must have a Context section");
    let context_section_end = adr[context_section_start..]
        .find("## Decision")
        .expect("ADR-0015 must have a Decision section after Context");
    let section = &adr[context_section_start..context_section_start + context_section_end];
    assert!(
        section.contains("CLI > env > file > default"),
        "ADR-0015 § Context bullet must describe CLI > env > file > default; \
         a regression to 'env > CLI' documents a precedence the impl does \
         not implement. Got section:\n{section}",
    );
    // ponytail: pin the negative — the inverted ordering must NOT
    // appear anywhere in the Context section. A contributor who
    // retypes the order and accidentally swaps CLI/env surfaces here.
    assert!(
        !section.contains("env > CLI > file > default"),
        "ADR-0015 § Context must not claim 'env > CLI > file > default'; \
         the impl and in-file tests pin CLI > env > XDG.",
    );
}

// ponytail: pin the ADR-0015 § Precedence chain section header.
// This is the prose readers will copy when implementing a new
// source — a contributor who reads the heading and wires the
// chain in the wrong order is reading stale documentation.
#[test]
fn adr_0015_precedence_section_header_names_cli_first() {
    let adr = read(&repo_root().join("docs/adr/0015-cli-design.md"));
    let header_start = adr
        .find("### Precedence chain (")
        .expect("ADR-0015 must have a § Precedence chain subsection");
    let header_end = adr[header_start..]
        .find('\n')
        .expect("Precedence chain subsection must end with newline");
    let header = &adr[header_start..header_start + header_end];
    assert!(
        header.contains("CLI > env > file > default"),
        "ADR-0015 § Precedence chain header must lead with CLI; got:\n{header}",
    );
    assert!(
        !header.contains("env > CLI > file > default"),
        "ADR-0015 § Precedence chain header must not lead with env; got:\n{header}",
    );
}

// ponytail: pin the ADR-0015 § Consequences Positives bullet.
// Round 9 found that the Context bullet (line 13) and the
// § Precedence chain header (line 158) both name "CLI > env >
// file > default", but the § Consequences Positives bullet
// claimed "Env > CLI > file > default" — the opposite. A
// contributor who reads the Positives summary and never looks
// at the § Precedence chain section learns the wrong precedence.
// The drift test below pins both the positive spelling AND the
// negative across the entire § Consequences section so a future
// regression surfaces in either bullet (Positives OR Negatives).
#[test]
fn adr_0015_consequences_bullet_names_cli_first() {
    let adr = read(&repo_root().join("docs/adr/0015-cli-design.md"));
    let section_start = adr
        .find("## Consequences")
        .expect("ADR-0015 must have a § Consequences section");
    let section_end_rel = adr[section_start..]
        .find("## Implementation notes")
        .expect("ADR-0015 must have a § Implementation notes section after Consequences");
    let section = &adr[section_start..section_start + section_end_rel];
    assert!(
        section.contains("CLI > env > file > default"),
        "ADR-0015 § Consequences must name CLI > env > file > default somewhere; \
         the § Precedence chain and impl agree on this ordering. Got section:\n{section}",
    );
    assert!(
        !section.contains("env > CLI > file > default"),
        "ADR-0015 § Consequences must not claim 'env > CLI > file > default'; \
         that precedence contradicts the § Precedence chain section, the \
         § Context bullet, the impl, and the in-file tests. Got section:\n{section}",
    );
}

// ponytail: pin the module docstring on `precedence.rs`. Round 10
// found that the function body and its four in-file tests all
// agree on "CLI > env > XDG", but the `//!` header at the top
// of the module said "env > CLI > XDG default" — the opposite.
// A contributor who reads the module header before scrolling
// to the impl learns the wrong precedence. The drift test
// below pins both the positive spelling AND the negative so
// a future regression (or a "fix" that flips the impl to match
// the doc) surfaces here.
#[test]
fn precedence_rs_module_docstring_names_cli_first() {
    let src = read(&repo_root().join("crates/plugin3-cli/src/precedence.rs"));
    // ponytail: the first `//!` line is the module-level summary
    // readers see in rustdoc and `cargo doc --open`. Find it by
    // scanning for the first `//!` so we don't pin the file's
    // comment shape beyond the leading line.
    let first_doc = src
        .lines()
        .find(|l| l.trim_start().starts_with("//!"))
        .expect("precedence.rs must have a leading //! doc line");
    assert!(
        first_doc.contains("CLI > env"),
        "precedence.rs module docstring must lead with 'CLI > env'; \
         the impl and in-file tests pin CLI > env > XDG. Got:\n{first_doc}",
    );
    assert!(
        !first_doc.contains("env > CLI"),
        "precedence.rs module docstring must not claim 'env > CLI'; \
         the impl and in-file tests pin CLI > env > XDG. Got:\n{first_doc}",
    );
}

// ponytail: pin ADR-0002's file-tree annotation. Round 10 found
// that ADR-0002 listed `precedence.rs` as the "env > CLI >
// file > default chain" — the opposite of what ADR-0015 (the
// source-of-truth for the chain) says, what the impl does, and
// what the in-file tests pin. A contributor wiring a new
// precedence source from the ADR-0002 directory tree alone
// (without opening precedence.rs) learns the wrong order.
// The drift test below pins both the positive spelling AND
// the negative so a future regression surfaces here.
#[test]
fn adr_0002_file_tree_annotation_names_cli_first() {
    let adr = read(&repo_root().join("docs/adr/0002-workspace.md"));
    // ponytail: the file-tree annotation lives on a single line
    // near the `precedence.rs` entry under `plugin3-cli/src/`.
    // We don't pin column alignment — we pin the precedence
    // ordering on that line, which is the load-bearing claim.
    let line = adr
        .lines()
        .find(|l| l.contains("precedence.rs"))
        .expect("ADR-0002 must mention precedence.rs in its file tree");
    assert!(
        line.contains("CLI > env"),
        "ADR-0002 file-tree annotation for precedence.rs must lead with \
         'CLI > env'; ADR-0015, the impl, and the in-file tests all pin \
         CLI > env > file > default. Got:\n{line}",
    );
    assert!(
        !line.contains("env > CLI"),
        "ADR-0002 file-tree annotation for precedence.rs must not claim \
         'env > CLI'; ADR-0015, the impl, and the in-file tests all pin \
         CLI > env > file > default. Got:\n{line}",
    );
}
