//! ADR-0010 (cost reporting) drift test — the `UsageRecord` schema
//! lives in two places: the ADR prose and the impl struct in
//! `cost.rs`. A contributor who adds a field to the struct (e.g.
//! `tokens_in` for per-prompt token counting) without updating
//! the ADR slips past the in-file tests. A contributor who
//! rewrites the ADR with extra fields without adding them to the
//! struct documents a phantom schema.
//!
//! ponytail: one literal-substring scan per direction. The ADR
//! owns the *negative* contract ("no `tokens_in`/`tokens_out`/
//! `model` fields yet") and the impl-side test below asserts the
//! struct still has only the documented fields.

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

// ponytail: pin the ADR-0010 negative contract on the struct
// schema block. The MVP defers per-prompt token counting, so the
// ADR's `UsageRecord` struct block must NOT declare
// `tokens_in`/`tokens_out`/`model` fields. A contributor who
// copies the older example JSON back into the ADR documents a
// shape nobody writes — readers will assume those fields exist
// and the drift compounds silently. The explanation paragraphs
// below the struct can still mention these names as "reserved"
// (the test scope is the fenced code block, not the prose).
#[test]
fn adr_0010_does_not_claim_unused_record_fields() {
    let adr = read(&repo_root().join("docs/adr/0010-cost-reporting.md"));
    let struct_section_start = adr
        .find("pub struct UsageRecord")
        .expect("ADR-0010 must describe UsageRecord");
    let struct_section_end = adr[struct_section_start..]
        .find("```")
        .expect("ADR-0010 UsageRecord section must close");
    let section = &adr[struct_section_start..struct_section_start + struct_section_end];
    for phantom in ["pub tokens_in", "pub tokens_out", "pub model"] {
        assert!(
            !section.contains(phantom),
            "ADR-0010 UsageRecord struct block must not declare `{phantom}` — \
             the MVP impl has no such field. If you are adding per-prompt \
             token counting, add the field to the struct in cost.rs first, \
             then update this ADR's struct block and this drift test.",
        );
    }
    // ponytail: pin the ADR's record-field enumeration. The
    // current schema lists exactly seven fields. A contributor
    // who adds or removes a field surfaces here.
    for expected in [
        "pub ts:",
        "pub kind:",
        "pub session_id:",
        "pub bytes_in:",
        "pub bytes_out:",
        "pub tokens_used:",
        "pub tokens_ceiling:",
        "pub tool:",
    ] {
        assert!(
            section.contains(expected),
            "ADR-0010 UsageRecord section must list `{expected}`; got:\n{section}",
        );
    }
}

// ponytail: pin the impl-side field set independently. The
// in-file tests cover the kind enum and round-tripping; this
// test guards the *struct schema* so a contributor adding a
// field surfaces here, not via a downstream parser break.
#[test]
fn usage_record_struct_field_set_is_pinned() {
    let body = read(&repo_root().join("crates/plugin3-core/src/cost.rs"));
    // Find the struct definition and assert each documented field
    // appears as a struct member (not just in a doc comment).
    let struct_start = body
        .find("pub struct UsageRecord")
        .expect("UsageRecord struct must exist in cost.rs");
    let struct_end = body[struct_start..]
        .find("\n}\n")
        .map_or(body.len(), |i| struct_start + i);
    let section = &body[struct_start..struct_end];
    for expected in [
        "pub ts:",
        "pub kind:",
        "pub session_id:",
        "pub bytes_in:",
        "pub bytes_out:",
        "pub tokens_used:",
        "pub tokens_ceiling:",
        "pub tool:",
    ] {
        assert!(
            section.contains(expected),
            "cost.rs UsageRecord must declare `{expected}`; got:\n{section}",
        );
    }
    // ponytail: pin the negative side on the impl too. A
    // contributor adding `tokens_in` without updating the ADR
    // fails *both* directions of this drift test.
    for phantom in ["pub tokens_in:", "pub tokens_out:", "pub model:"] {
        assert!(
            !section.contains(phantom),
            "cost.rs UsageRecord must not have `{phantom}` — ADR-0010 has \
             not been updated to describe this field. If you are adding \
             per-prompt token counting, update ADR-0010 and the drift test.",
        );
    }
}
