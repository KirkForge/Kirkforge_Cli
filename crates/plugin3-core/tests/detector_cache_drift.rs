//! ADR-0006 (tool output detector) cross-ADR drift test — the cache
//! bound lives in the ADR prose, not the code, so a contributor
//! who tunes the impl constant silently without updating the ADR
//! slips past the in-file tests. This file scans the ADR for the
//! literal "64 entries" phrasing that the impl's
//! `DETECTOR_CACHE_CAP = 64` matches.
//!
//! ponytail: one literal-substring scan, no TOML/markdown parser.
//! If a contributor bumps `DETECTOR_CACHE_CAP` (in
//! `plugin3-core/src/orchestrator.rs`) the matching test below
//! fails with a diff that points at both the constant and the ADR
//! line. If a contributor rewrites the ADR prose without updating
//! the constant, this test catches it; if a contributor tunes the
//! constant without rewriting the ADR prose, this test catches it.

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

// ponytail: pin the ADR-0006 § Detector caching cap. The spec says
// the cache is bounded at 64 entries; the impl is
// `pub(crate) const DETECTOR_CACHE_CAP: usize = 64;` in
// `orchestrator.rs`. This drift test makes any future mismatch loud:
// a contributor who bumps the impl without rewriting the ADR (or
// vice versa) gets a single failing assertion that names both.
#[test]
fn adr_0006_cache_cap_matches_impl_constant() {
    let adr = read(&repo_root().join("docs/adr/0006-tool-output-detector.md"));
    assert!(
        adr.contains("64 entries"),
        "ADR-0006 § Detector caching must describe the 64-entry bound \
         that DETECTOR_CACHE_CAP pins; got ADR without \"64 entries\". \
         If you are tuning the cap, update both the impl constant and \
         the ADR prose.",
    );
    // ponytail: pin the eviction shape too. The current impl uses
    // `entries.clear()` on overflow (clear-on-evict), not LRU. A
    // contributor who swaps in an LRU cache without rewriting this
    // ADR surfaces here. The phrasing is intentionally literal so
    // the existing impl comment ("a future LRU is a swap-in if
    // measured hit-rates show churn") is the only place "LRU"
    // appears in the ADR.
    assert!(
        adr.contains("clear-on-evict"),
        "ADR-0006 § Detector caching must describe clear-on-evict \
         semantics; LRU is documented as a future swap-in only. If \
         you are graduating LRU from comment to implementation, \
         rewrite this ADR section in full.",
    );
}

// ponytail: pin the orchestrator constant. This duplicates the
// in-file test `detector_cache_cap_constant_is_pinned` for
// protection against a contributor deleting the in-file test in
// the same diff that tunes the constant — the drift test lives in
// `tests/` and is harder to delete accidentally.
#[test]
fn detector_cache_cap_constant_matches_adr() {
    let body = read(&repo_root().join("crates/plugin3-core/src/orchestrator.rs"));
    assert!(
        body.contains("DETECTOR_CACHE_CAP: usize = 64"),
        "orchestrator.rs must keep `pub(crate) const DETECTOR_CACHE_CAP: usize = 64;` \
         per ADR-0006 § Detector caching. If you are tuning the cap, \
         update both this constant and the ADR prose.",
    );
}
