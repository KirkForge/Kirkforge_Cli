//! ADR-0006 (Tool-output detector) drift tests — the contracts
//! that live in the ADR prose and must stay in lockstep with the
//! `plugin3-core/src/detector.rs` impl and the `DetectorCache`
//! in `orchestrator.rs`. Companion to the in-file tests inside
//! `detector.rs` and `orchestrator.rs` (which pin impl-side
//! behaviour); this file pins the *spec surface* — the
//! § Layered detection code block (with its UTF-8-safe
//! `floor_char_boundary` head slice), the § Detector caching
//! code block (RefCell-not-parking_lot, content_len-not-blake3),
//! and the § Slicing rules per kind threshold table.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who re-pastes the
//! `parking_lot::Mutex<HashMap<(String, blake3::Hash), _>>`
//! cache form back into the ADR documents a `parking_lot` and
//! `blake3`-as-key dependency the impl does not have).

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

fn adr_0006() -> String {
    read(&repo_root().join("docs/adr/0006-tool-output-detector.md"))
}

/// Read ADR-0006's § Layered detection code block.
fn adr_0006_layered_detection_block() -> String {
    let adr = adr_0006();
    let section_start = adr
        .find("### Layered detection")
        .expect("ADR-0006 must have a § Layered detection subsection");
    let section_end = adr[section_start..]
        .find("### Slicing rules per kind")
        .expect("ADR-0006 § Layered detection must precede § Slicing rules per kind");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0006 § Layered detection must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0006 § Layered detection rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0006's § Detector caching code block.
fn adr_0006_detector_caching_block() -> String {
    let adr = adr_0006();
    let section_start = adr
        .find("### Detector caching")
        .expect("ADR-0006 must have a § Detector caching subsection");
    let section_end = adr[section_start..]
        .find("## Consequences")
        .expect("ADR-0006 § Detector caching must precede § Consequences");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0006 § Detector caching must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0006 § Detector caching rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0006's § Slicing rules per kind code block.
fn adr_0006_slicing_rules_block() -> String {
    let adr = adr_0006();
    let section_start = adr
        .find("### Slicing rules per kind")
        .expect("ADR-0006 must have a § Slicing rules per kind subsection");
    let section_end = adr[section_start..]
        .find("### Why FileContent is excluded")
        .expect("ADR-0006 § Slicing rules must precede § Why FileContent is excluded");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0006 § Slicing rules must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0006 § Slicing rules rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0006's § Implementation notes.
fn adr_0006_implementation_notes() -> String {
    let adr = adr_0006();
    let section_start = adr
        .find("## Implementation notes")
        .expect("ADR-0006 must have an Implementation notes section");
    adr[section_start..].to_string()
}

// ---- § Layered detection: positive + negative tests ----

// ponytail: pin the § Layered detection example's
// UTF-8-safe head slice. The MVP uses
// `floor_char_boundary(input, 1024.min(input.len()))` to
// avoid panicking on multi-byte UTF-8 (CJK/emoji tool
// output). The earlier draft's `&input[..input.len().min(1024)]`
// byte slice panics on CJK input. The in-file test
// `from_shape_does_not_panic_on_utf8_boundary` pins the
// behaviour for a 1024-ASCII + 2000-CJK input.
#[test]
fn adr_0006_layered_detection_block_uses_floor_char_boundary() {
    let block = adr_0006_layered_detection_block();
    assert!(
        block.contains("floor_char_boundary(input, 1024"),
        "ADR-0006 § Layered detection example must use \
         `floor_char_boundary(input, 1024.min(input.len()))` \
         — the UTF-8-safe head slice the impl uses to avoid \
         panicking on CJK/emoji tool output.",
    );
    // Negative: the naive byte-slice form must NOT appear.
    assert!(
        !block.contains("&input[..input.len().min(1024)]"),
        "ADR-0006 § Layered detection example must NOT use \
         `&input[..input.len().min(1024)]` — the naive byte \
         slice panics on multi-byte UTF-8 (the input's byte \
         1024 may land inside a CJK codepoint).",
    );
}

// ponytail: pin the § Layered detection example's
// TestRunner line-iterator shape. The MVP uses
// `head.lines().any(|l| l.starts_with(\"running \") || l.starts_with(\"test result:\"))`
// — per-line iteration rather than a substring check on
// `head.contains(" running ")`. The line-iterator form
// catches a `running 5 tests` line at column 0 without
// false positives on prose that happens to contain
// " running " mid-sentence.
#[test]
fn adr_0006_layered_detection_block_iterates_lines_for_testrunner() {
    let block = adr_0006_layered_detection_block();
    assert!(
        block.contains("head.lines().any(|l|"),
        "ADR-0006 § Layered detection example must iterate \
         lines for the TestRunner check — matches the impl's \
         `head.lines().any(|l| l.starts_with(\"running \") || \
         l.starts_with(\"test result:\"))`.",
    );
}

// ponytail: pin the § Layered detection example's
// short-circuit SearchResults check. The MVP iterates
// manually and breaks on the first line ≥ 200 bytes —
// no `Vec<&str>` materialisation. The earlier draft
// used `head.lines().all(|l| l.len() < 200)` which
// allocates a `Vec<&str>` per detect call.
#[test]
fn adr_0006_layered_detection_block_short_circuits_search_results() {
    let block = adr_0006_layered_detection_block();
    assert!(
        block.contains("if l.len() >= 200") && block.contains("all_short = false; break"),
        "ADR-0006 § Layered detection example must show the \
         manual `for l in &mut lines` short-circuit on \
         `l.len() >= 200` — matches the impl's no-allocation \
         SearchResults check.",
    );
}

// ponytail: pin the absence of `tracing` events in the
// § Layered detection example. The MVP does not depend on
// `tracing` (ADR-0017 § Workspace Cargo.toml) and the
// detector emits zero tracing events today.
#[test]
fn adr_0006_layered_detection_block_does_not_claim_tracing() {
    let block = adr_0006_layered_detection_block();
    for phantom in [
        "tracing::warn",
        "tracing::info",
        "tracing::error",
        "tracing::debug",
        "use tracing",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0006 § Layered detection example claims \
             `{phantom}` but the workspace does not depend on \
             `tracing`. The detector emits zero tracing events.",
        );
    }
}

// ---- § Detector caching: positive + negative tests ----

// ponytail: pin the § Detector caching example's
// `RefCell`-not-`parking_lot::Mutex` shape. The MVP uses
// `std::cell::RefCell` because the orchestrator is
// per-call single-threaded (ADR-0007: `pub fn run(orch,
// outputs)`). The workspace does not depend on
// `parking_lot` (ADR-0017 § Workspace Cargo.toml).
#[test]
fn adr_0006_detector_caching_block_uses_refcell_not_parking_lot() {
    let block = adr_0006_detector_caching_block();
    // Positive: the RefCell must be visible.
    assert!(
        block.contains("RefCell<std::collections::HashMap"),
        "ADR-0006 § Detector caching example must show \
         `RefCell<std::collections::HashMap<...>>` — the \
         per-call single-threaded cache shape the impl uses. \
         A contributor who swaps in `parking_lot::Mutex` \
         documents a dep the workspace does not have.",
    );
    // Negative: parking_lot must NOT appear in the code block.
    assert!(
        !block.contains("parking_lot"),
        "ADR-0006 § Detector caching example must not reference \
         `parking_lot` — the workspace does not depend on \
         `parking_lot` (ADR-0017 § Workspace Cargo.toml). The \
         orchestrator is single-threaded (`pub fn run(orch, \
         outputs)`); `RefCell` is the right primitive.",
    );
}

// ponytail: pin the § Detector caching example's
// `blake3::Hash`-key shape. The MVP keys by
// `(Option<String>, blake3::Hash)` — the tool name + a
// BLAKE3 hash of the head (first 1024 char-boundary bytes).
// The earlier length-only key `(Option<String>, usize)`
// collided on two equally-sized outputs with different
// shapes (e.g. an 8 KB cargo-test body vs an 8 KB compiler
// body) and the second call returned the cached kind of
// the first. The behavioural test
// `detector_cache_distinguishes_same_length_different_shape`
// (in `orchestrator.rs`) pins the post-fix cache size of 2;
// this drift test pins the ADR-side documentation of the
// BLAKE3 key so a contributor who simplifies the key back
// to `content.len()` surfaces here.
#[test]
fn adr_0006_detector_caching_block_keys_by_blake3_head_hash() {
    let block = adr_0006_detector_caching_block();
    // Positive: the BLAKE3 head-hash key component must be visible.
    assert!(
        block.contains("blake3::Hash"),
        "ADR-0006 § Detector caching example must show \
         `(Option<String>, blake3::Hash)` — the cache key \
         the impl uses. A contributor who simplifies the key \
         back to `content.len()` re-introduces the same-length \
         collision the BLAKE3 head hash was added to fix.",
    );
    // Negative: the length-only cache key shape must NOT appear
    // in the § Detector caching example. `content.len()`
    // legitimately appears in the head-boundary calculation
    // (`floor_char_boundary(content, 1024.min(content.len()))`)
    // and elsewhere in the codebase, so the negative pin is
    // narrowed to the *cache key tuple* — the length-only key
    // is `(tool_name.map(str::to_owned), content.len())`.
    assert!(
        !block.contains("tool_name.map(str::to_owned), content.len()"),
        "ADR-0006 § Detector caching example must not present \
         `(tool_name.map(str::to_owned), content.len())` as \
         the cache key — the length-only key collides on \
         equally-sized distinct-shape inputs. The impl uses \
         `(Option<String>, blake3::Hash)` where `blake3::Hash` \
         is a BLAKE3 hash of the first 1024 char-boundary bytes.",
    );
}

// ponytail: pin the § Detector caching example's
// `DETECTOR_CACHE_CAP: usize = 64` constant. The MVP
// caps the cache at 64 entries with clear-on-evict
// semantics. A contributor who tunes the cap
// (64 → 128) without updating the ADR surfaces here.
#[test]
fn adr_0006_detector_caching_block_pins_64_entry_cap() {
    let block = adr_0006_detector_caching_block();
    assert!(
        block.contains("DETECTOR_CACHE_CAP: usize = 64"),
        "ADR-0006 § Detector caching example must show \
         `DETECTOR_CACHE_CAP: usize = 64` — the 64-entry \
         cache cap. The drift test `detector_cache_clears_at_cap_boundary` \
         (in `orchestrator.rs`) pins the clear-on-evict \
         behaviour.",
    );
}

// ponytail: pin the § Detector caching example's
// source-file reference. The MVP's `DetectorCache` lives
// in `crates/plugin3-core/src/orchestrator.rs` (not
// `crates/plugin3-core/src/detector/cache.rs`). A
// contributor who splits the cache into its own module
// surfaces here.
#[test]
fn adr_0006_detector_caching_block_points_to_orchestrator_module() {
    let block = adr_0006_detector_caching_block();
    assert!(
        block.contains("crates/plugin3-core/src/orchestrator.rs"),
        "ADR-0006 § Detector caching example must point to \
         `crates/plugin3-core/src/orchestrator.rs` — the \
         MVP's `DetectorCache` lives in the orchestrator \
         module, not in a `detector/cache.rs` submodule.",
    );
    // Negative: the detector/cache.rs path must NOT appear.
    assert!(
        !block.contains("detector/cache.rs"),
        "ADR-0006 § Detector caching example must not \
         reference `detector/cache.rs` — the cache is in \
         the orchestrator module.",
    );
}

// ---- § Slicing rules per kind: positive-direction tests ----

// ponytail: pin the § Slicing rules per kind example's
// threshold table. The MVP declares seven kinds with
// specific byte thresholds (TestRunner 8K, BuildLog 4K,
// Compiler 8K, GenericShell 2K, SearchResults 16K,
// FileContent never, Json 4K, Unknown 8K). The earlier
// draft's hard-coded `8 * 1024` literals in the match
// arms are extracted in the impl to `THRESHOLD_VERBOSE`,
// `THRESHOLD_MEDIUM`, etc. — both forms agree on the
// numbers.
#[test]
fn adr_0006_slicing_rules_block_pins_thresholds() {
    let block = adr_0006_slicing_rules_block();
    // Each threshold must appear at least once (either
    // as a literal `8 * 1024` or as a constant reference).
    assert!(
        block.contains("8 * 1024"),
        "ADR-0006 § Slicing rules per kind example must show \
         the 8 KB threshold (TestRunner / Compiler / Unknown).",
    );
    assert!(
        block.contains("4 * 1024"),
        "ADR-0006 § Slicing rules per kind example must show \
         the 4 KB threshold (BuildLog / Json).",
    );
    assert!(
        block.contains("2 * 1024"),
        "ADR-0006 § Slicing rules per kind example must show \
         the 2 KB threshold (GenericShell).",
    );
    assert!(
        block.contains("16 * 1024"),
        "ADR-0006 § Slicing rules per kind example must show \
         the 16 KB threshold (SearchResults).",
    );
    assert!(
        block.contains("usize::MAX"),
        "ADR-0006 § Slicing rules per kind example must show \
         `usize::MAX` as the FileContent threshold — the \
         never-auto-slice exception.",
    );
}

// ponytail: pin the § Slicing rules per kind example's
// Slice head/tail constants. The MVP's `Decision::Slice`
// carries `keep_head: 4096, keep_tail: 4096`. The drift
// test `slice_shape_constants_are_pinned` (in `detector.rs`)
// pins both constants.
#[test]
fn adr_0006_slicing_rules_block_pins_head_tail_constants() {
    let block = adr_0006_slicing_rules_block();
    assert!(
        block.contains("keep_head: 4096") && block.contains("keep_tail: 4096"),
        "ADR-0006 § Slicing rules per kind example must show \
         `Decision::Slice {{ keep_head: 4096, keep_tail: 4096 }}` \
         — matches the impl's `SLICE_HEAD_BYTES` and \
         `SLICE_TAIL_BYTES` constants and the in-file test \
         `slice_shape_constants_are_pinned`.",
    );
}

// ---- § Implementation notes: cache location ----

// ponytail: pin the § Implementation notes' cache
// location. The prose must say the cache lives in the
// orchestrator module, not in a `detector/cache.rs`
// submodule.
#[test]
fn adr_0006_implementation_notes_point_to_orchestrator_module() {
    let section = adr_0006_implementation_notes();
    assert!(
        section.contains("orchestrator") && section.contains("DetectorCache"),
        "ADR-0006 § Implementation notes must reference the \
         orchestrator module as the cache's home — the MVP's \
         `DetectorCache` is in `crates/plugin3-core/src/orchestrator.rs`.",
    );
    // Negative: the detector/cache.rs path must NOT appear.
    assert!(
        !section.contains("detector/cache.rs"),
        "ADR-0006 § Implementation notes must not reference \
         `detector/cache.rs` — the cache is in the orchestrator \
         module. The drift test `adr_0004_*` (in `offload_store_spec_drift.rs`) \
         already pins ADR-0004's no-submodule claim; ADR-0006's \
         cache follows the same convention.",
    );
}
