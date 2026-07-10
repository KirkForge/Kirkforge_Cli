//! ADR-0007 (Slicing orchestrator) drift tests — the contracts
//! that live in the ADR prose and must stay in lockstep with
//! the `plugin3-core/src/orchestrator.rs` impl.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who re-pastes the
//! rayon `par_iter` example back into the ADR documents a
//! parallel-fan-out design the impl does not ship, and the
//! resulting `cargo build` breakage lands on a fresh
//! checkout, not on incremental — invisible until CI runs).

use std::path::{Path, PathBuf};

use plugin3_core::orchestrator::{self, DetectorCache, SliceDecision, SlicingOrchestrator};
use plugin3_core::slicing::HeadTailSlicer;
use plugin3_core::store::{InMemoryOffloadStore, SLICE_MARKER_PREFIX};
use plugin3_core::ToolOutputKind;

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

/// Read ADR-0007's § Orchestrator API code block (the only
/// fenced `rust` block in that subsection). Scoped so the
/// explanatory paragraphs around the block can mention
/// phantom names ("rayon was deferred...") without tripping
/// the drift test.
fn adr_0007_orchestrator_api_block() -> String {
    let adr = read(&repo_root().join("docs/adr/0007-slicing-orchestrator.md"));
    let section_start = adr
        .find("### Orchestrator API")
        .expect("ADR-0007 must have a § Orchestrator API subsection");
    let section_end = adr[section_start..]
        .find("### Why serial")
        .expect("ADR-0007 § Orchestrator API must precede § Why serial");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0007 § Orchestrator API must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0007 § Orchestrator API rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0007's § Logging block (the entire subsection —
/// it's a short paragraph + a code block, not a fenced
/// region we can scope to). The "no `tracing::info`!" check
/// is on the whole subsection; the explanatory paragraph is
/// allowed to mention `tracing` in the context of "the
/// earlier draft specified ... but the MVP doesn't".
fn adr_0007_logging_subsection() -> String {
    let adr = read(&repo_root().join("docs/adr/0007-slicing-orchestrator.md"));
    let section_start = adr
        .find("### Logging")
        .expect("ADR-0007 must have a § Logging subsection");
    let section_end = adr[section_start..]
        .find("## Consequences")
        .expect("ADR-0007 § Logging must precede § Consequences");
    adr[section_start..section_start + section_end].to_string()
}

/// Read ADR-0007's § Implementation notes (the entire section
/// — short, no fenced code blocks).
fn adr_0007_implementation_notes() -> String {
    let adr = read(&repo_root().join("docs/adr/0007-slicing-orchestrator.md"));
    let section_start = adr
        .find("## Implementation notes")
        .expect("ADR-0007 must have an Implementation notes section");
    adr[section_start..].to_string()
}

// ---- Negative-direction tests (phantom deps / shapes) ----

// ponytail: pin the absence of rayon in the § Orchestrator
// API example block. The MVP serial loop doesn't import
// `rayon::prelude::*` and doesn't call `par_iter` — a
// contributor who copy-pastes the older parallel example
// back documents a parallel-fan-out design the impl does
// not ship. The drift test guards the actual shape.
#[test]
fn adr_0007_orchestrator_api_block_does_not_claim_rayon() {
    let block = adr_0007_orchestrator_api_block();
    for phantom in [
        "use rayon::prelude::*",
        "rayon::prelude",
        ".par_iter()",
        "par_iter",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0007 § Orchestrator API example block claims `{phantom}` \
             but the MVP is serial. Adding rayon is a future ADR with a \
             binary-size budget to negotiate — update both the ADR and \
             the impl, then update this drift test to expect the new \
             shape.",
        );
    }
}

// ponytail: pin the absence of `tracing::warn!` in the
// § Orchestrator API example block. The MVP routes through
// `slice_or_skip` (ADR-0003), which uses `eprintln!` for the
// non-Skipped error path. The orchestrator itself emits zero
// events today; the workspace does not depend on `tracing`
// (ADR-0017 § Workspace Cargo.toml).
#[test]
fn adr_0007_orchestrator_api_block_does_not_claim_tracing() {
    let block = adr_0007_orchestrator_api_block();
    for phantom in [
        "tracing::warn",
        "tracing::info",
        "tracing::error",
        "tracing::debug",
        "use tracing",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0007 § Orchestrator API example block claims `{phantom}` \
             but the workspace does not depend on `tracing`. The MVP \
             routes errors through `slice_or_skip` (ADR-0003) whose \
             non-Skipped path emits one `eprintln!` to stderr. Adding \
             tracing is a future ADR with a `tracing = \"0.1\"` dep.",
        );
    }
}

// ponytail: pin the absence of the missing-field example
// shape. The earlier draft's `SliceDecision::Sliced` had
// `{ marker, bytes_kept, bytes_offloaded }` (no head/tail).
// The impl carries `head` and `tail` so the caller can pass
// the kept bytes through without re-fetching them from the
// store. A contributor who re-pastes the older 3-field
// example documents a return shape the impl does not have.
//
// The check uses the type-declaration form (`head: String`)
// rather than the bare field name (`head:`) so the test
// doesn't get a false positive from the field-shorthand
// syntax in the destructuring site (`head: out.head`).
#[test]
fn adr_0007_orchestrator_api_block_pins_sliced_variant_fields() {
    let block = adr_0007_orchestrator_api_block();
    // Positive: head and tail fields must be declared as
    // `String` types in the `Sliced` variant — not just
    // referenced in the destructuring site.
    assert!(
        block.contains("head: String,") && block.contains("tail: String,"),
        "ADR-0007 § Orchestrator API example block must declare `head: \
         String` and `tail: String` fields in the `Sliced` variant — the \
         orchestrator returns the kept bytes alongside the marker so the \
         caller can pass them through without a store re-fetch.",
    );
    assert!(
        block.contains("marker: String,"),
        "ADR-0007 § Orchestrator API example block must declare \
         `marker: String` field — the cost reporter reads this directly.",
    );
    assert!(
        block.contains("bytes_kept: usize,") && block.contains("bytes_offloaded: usize,"),
        "ADR-0007 § Orchestrator API example block must declare \
         `bytes_kept: usize` and `bytes_offloaded: usize` fields — the \
         cost reporter sums `bytes_offloaded` across all Sliced rows.",
    );
}

// ponytail: pin the API shape (free function, not a method).
// The earlier draft showed `SlicingOrchestrator::run` as a
// method (`impl<'a> SlicingOrchestrator<'a> { pub fn run ... }`).
// The actual impl is a free function `pub fn run(orch:
// &SlicingOrchestrator<'_>, outputs: ...)`. A contributor
// who re-pastes the method form surfaces here.
#[test]
fn adr_0007_orchestrator_api_block_uses_free_run_function() {
    let block = adr_0007_orchestrator_api_block();
    // Negative: no `impl` block containing the `run` method.
    assert!(
        !block.contains("impl<'a> SlicingOrchestrator<'a>"),
        "ADR-0007 § Orchestrator API must not show `SlicingOrchestrator::run` \
         as a method — the impl is a free `fn run(orch, outputs)` so the \
         caller can pass an orchestrator by reference without taking \
         `&mut` (which would conflict with the orchestrator's shared \
         DetectorCache via `&self`).",
    );
    // Positive: the free function signature must be visible.
    assert!(
        block.contains("pub fn run("),
        "ADR-0007 § Orchestrator API must show the free `pub fn run(...)` \
         signature.",
    );
}

// ponytail: pin the absence of the `tracing::info!` event in
// § Logging. The earlier draft specified a `tracing::info!`
// event per orchestrator run, consumed by the cost
// reporter. The MVP does not depend on `tracing`. The
// explanatory paragraph can mention "tracing" in the
// context of "was specified, but the MVP doesn't ship it" —
// the negative check is scoped to the **code block** (which
// must not contain `tracing::info!`).
#[test]
fn adr_0007_logging_section_omits_tracing_event() {
    let section = adr_0007_logging_subsection();
    // Find the fenced rust code block in § Logging (if any)
    // and assert it doesn't contain tracing::info!.
    if let Some(fence_start) = section.find("```rust\n") {
        let fence_after = &section[fence_start + "```rust\n".len()..];
        if let Some(fence_end_rel) = fence_after.find("```") {
            let block = &fence_after[..fence_end_rel];
            assert!(
                !block.contains("tracing::info!"),
                "ADR-0007 § Logging code block must not contain \
                 `tracing::info!` — the workspace does not depend on \
                 `tracing`. The orchestrator returns \
                 `OrchestratorResult` to the caller; the cost reporter \
                 reads `bytes_saved` from there. Adding tracing is a \
                 future ADR with a `tracing = \"0.1\"` dep.",
            );
        }
    }
    // ponytail: also pin the absence of a phantom
    // `tracing` dep in the § Logging prose. The prose
    // paragraph can mention "the MVP does **not** depend on
    // `tracing`" — that's fine. The negative check is on
    // the positive claim ("depends on `tracing`",
    // "requires `tracing`", "the workspace includes
    // `tracing`"), which would document a dep the impl
    // doesn't wire.
    for positive_claim in [
        "depends on `tracing`",
        "the workspace includes `tracing`",
        "`tracing` dep",
    ] {
        assert!(
            !section.contains(positive_claim),
            "ADR-0007 § Logging must not assert `{positive_claim}` — the \
             workspace does not depend on `tracing`. The MVP routes \
             errors via `eprintln!` (ADR-0003 § slice_or_skip).",
        );
    }
}

// ponytail: pin the § Implementation notes dep list. The
// earlier draft listed `rayon` in the orchestrator's
// dependencies. The MVP has no `rayon` dep. The positive
// direction is locked: store + slicing + detector only.
// Scope the negative check to the deps-sentence (the line
// that reads "It depends only on ...") so the explanatory
// prose can mention phantom names ("no rayon") without
// tripping the test.
#[test]
fn adr_0007_implementation_notes_lists_no_rayon_or_tracing_dep() {
    let section = adr_0007_implementation_notes();
    // Find the deps sentence — the line that begins with
    // "It depends only on" (or similar). Scoped substring
    // scan: only the deps line is asserted; the explanatory
    // paragraph below can name phantom deps as long as it
    // negates them ("no rayon", "no tracing").
    let deps_line = section
        .lines()
        .find(|line| line.contains("depends only on") || line.contains("It depends on"))
        .unwrap_or_else(|| {
            panic!(
                "ADR-0007 § Implementation notes must contain a deps line \
             beginning with 'It depends only on' (or 'It depends on'); \
             got section:\n{section}",
            )
        });
    for phantom in ["rayon", "tracing"] {
        assert!(
            !deps_line.contains(phantom),
            "ADR-0007 § Implementation notes deps line must not list \
             `{phantom}` as an orchestrator dep — the workspace does not \
             depend on it. Adding `{phantom}` is a future ADR; update \
             both the ADR and the impl together. Got line:\n{deps_line}",
        );
    }
}

// ponytail: pin the § Implementation notes' positive
// dep list. The orchestrator must explicitly name `store`,
// `slicing`, and `detector` as its dependencies — these are
// the modules the impl imports.
#[test]
fn adr_0007_implementation_notes_names_actual_module_deps() {
    let section = adr_0007_implementation_notes();
    for dep in ["store", "slicing", "detector"] {
        assert!(
            section.contains(dep),
            "ADR-0007 § Implementation notes must name `{dep}` as an \
             orchestrator dep — the impl imports from `crate::{dep}`.",
        );
    }
}

// ponytail: pin the drift-test path. ADR-0007 § Implementation
// notes (after this round's reconciliation) must reference
// the new `slicing_orchestrator_spec_drift.rs` test file.
// A contributor who moves or renames the test surfaces here.
#[test]
fn adr_0007_implementation_notes_references_drift_test_path() {
    let section = adr_0007_implementation_notes();
    assert!(
        section.contains("slicing_orchestrator_spec_drift.rs"),
        "ADR-0007 § Implementation notes must reference the drift-test \
         file by its concrete name `slicing_orchestrator_spec_drift.rs` \
         — the file is the load-bearing drift contract per ADR-0016 § \
         Drift tests.",
    );
}

// ---- Positive-direction tests (impl-side pinning) ----

// ponytail: pin the impl's `SliceDecision::Sliced` shape so
// a contributor who drops the `head` or `tail` field from
// the actual Rust enum surfaces in the impl-side test (the
// ADR-side test above catches the spec drift). The two
// tests are independent surfaces.
#[test]
fn orchestrator_impl_sliced_variant_has_head_and_tail_fields() {
    // ponytail: structural assertion via Debug. The
    // `Sliced` variant must destructure `{ kind, marker,
    // head, tail, bytes_kept, bytes_offloaded }` — a
    // contributor who drops `head` or `tail` from the
    // enum (returning them in a separate tuple, say)
    // surfaces here. `kind` carries the ToolOutputKind the
    // orchestrator already computed so the CLI can render
    // the note without a redundant `detector::detect`
    // call (ADR-0007 § Orchestrator API).
    let store = InMemoryOffloadStore::new();
    let slicer = HeadTailSlicer::default();
    let detector = DetectorCache::new();
    let o = SlicingOrchestrator {
        store: &store,
        slicer: &slicer,
        detector,
    };
    // 50 KB test fixture triggers TestRunner detection
    // and crosses the 8 KB threshold.
    let mut input = String::from("running 5 tests\ntest foo ... ok\n");
    input.push_str(&"x".repeat(50_000));
    input.push_str("\ntest bar ... FAILED\n");
    let r = orchestrator::run(&o, &[("k".into(), input.clone(), None)]);
    assert_eq!(r.decisions.len(), 1);
    match &r.decisions[0].1 {
        SliceDecision::Sliced {
            kind,
            marker,
            head,
            tail,
            bytes_kept,
            bytes_offloaded,
        } => {
            // ponytail: marker is the wire-format contract.
            assert!(marker.starts_with(SLICE_MARKER_PREFIX));
            // ponytail: kind carries the classification the
            // orchestrator already computed (TestRunner
            // because the input starts with `running N
            // tests\n`). A contributor who drops `kind` (or
            // falls back to Unknown) breaks the CLI's
            // ability to render the note without a redundant
            // detect call.
            assert_eq!(
                *kind,
                ToolOutputKind::TestRunner,
                "kind on SliceDecision must be the orchestrator's classified kind"
            );
            // ponytail: head + tail == bytes_kept (ADR-0003 § HeadTailSlicer
            // returns the kept bytes in head + tail fields).
            assert_eq!(
                head.len() + tail.len(),
                *bytes_kept,
                "head.len() + tail.len() must equal bytes_kept"
            );
            // ponytail: bytes_kept + bytes_offloaded == input.len().
            assert_eq!(
                *bytes_kept + *bytes_offloaded,
                input.len(),
                "bytes_kept + bytes_offloaded must equal input length"
            );
            // ponytail: head is non-empty (the orchestrator returns
            // the kept head bytes so the caller can pass them through).
            assert!(
                !head.is_empty(),
                "head must be non-empty when a slice decision is made"
            );
        }
        SliceDecision::Keep { .. } => panic!("expected Sliced, got Keep"),
    }
}

// ponytail: pin the impl's `SliceDecision::Keep` shape —
// the new `kind` field must surface on Keep too so the
// CLI can read it from either variant without re-detecting.
// A contributor who adds `kind` only to the Sliced variant
// (and leaves Keep as `Keep { bytes }`) breaks the match
// in `hooks::post_tool_use` which destructures both
// variants — compile-time drift catches it; this attribute
// pin catches a clean shape-only change.
#[test]
fn orchestrator_impl_keep_variant_carries_kind_field() {
    let store = InMemoryOffloadStore::new();
    let slicer = HeadTailSlicer::default();
    let o = SlicingOrchestrator {
        store: &store,
        slicer: &slicer,
        detector: DetectorCache::new(),
    };
    let r = orchestrator::run(&o, &[("k".into(), "tiny".to_string(), None)]);
    assert_eq!(r.decisions.len(), 1);
    // ponytail: assert the destructure compiles AND that
    // the kind is the orchestrator's classified kind
    // (Unknown for a body with no shape signal). A
    // contributor who drops `kind` from Keep fails to
    // compile here; a contributor who sets it to a
    // sentinel surfaces here.
    match &r.decisions[0].1 {
        SliceDecision::Keep { kind, bytes } => {
            assert_eq!(
                *kind,
                ToolOutputKind::Unknown,
                "Keep.kind must be the orchestrator's classified kind"
            );
            assert_eq!(*bytes, 4);
        }
        SliceDecision::Sliced { .. } => panic!("expected Keep, got Sliced"),
    }
}
