//! ADR-0003 (`SlicingTransform` + `CompactionTransform`) drift
//! tests — pin the § `SlicingTransform` trait, § `HeadTailSlicer`,
//! § `CompactionTransform` trait, and § Implementation notes
//! `slice_or_skip` prose against the actual impl in
//! `crates/plugin3-core/src/slicing.rs` and `compaction.rs`.
//! Companion to the unit tests inside those modules (which
//! pin the runtime behaviour); this file pins the *spec
//! surface* — the documented code blocks, the `Default`
//! impl for `HeadTailSlicer`, the `format_slice_marker`
//! helper, the UTF-8 char-boundary alignment, and the
//! absence of `tracing::warn!` in `slice_or_skip`.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who re-pastes the
//! `tracing::warn!` event back into the ADR documents a
//! `tracing` dep the impl does not wire, and the resulting
//! `cargo build` breakage lands on a fresh checkout, not
//! on incremental — invisible until CI runs).

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

fn adr_0003() -> String {
    read(&repo_root().join("docs/adr/0003-output-split.md"))
}

/// Read ADR-0003's § `SlicingTransform` trait code block.
fn adr_0003_slicing_trait_block() -> String {
    let adr = adr_0003();
    let section_start = adr
        .find("### SlicingTransform trait")
        .expect("ADR-0003 must have a § SlicingTransform trait subsection");
    let section_end = adr[section_start..]
        .find("### HeadTailSlicer")
        .expect("ADR-0003 § SlicingTransform trait must precede § HeadTailSlicer");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0003 § SlicingTransform trait must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0003 § SlicingTransform trait rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0003's § `HeadTailSlicer` code block. Scoped to
/// the fenced `rust` block only — the explanatory paragraphs
/// around the block mention the older inline format
/// (`<<plugin3:slice:{}>>`) in negation context, and the
/// negative-pin drift test scans the block body so those
/// paragraphs don't trip it.
fn adr_0003_head_tail_slicer_block() -> String {
    let adr = adr_0003();
    let section_start = adr
        .find("### HeadTailSlicer")
        .expect("ADR-0003 must have a § HeadTailSlicer subsection");
    let section_end = adr[section_start..]
        .find("### CompactionTransform trait")
        .expect("ADR-0003 § HeadTailSlicer must precede § CompactionTransform trait");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0003 § HeadTailSlicer must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0003 § HeadTailSlicer rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0003's § Implementation notes `slice_or_skip`
/// code block (the only fenced `rust` block in that
/// section).
fn adr_0003_slice_or_skip_block() -> String {
    let adr = adr_0003();
    let section_start = adr
        .find("## Implementation notes")
        .expect("ADR-0003 must have an Implementation notes section");
    let section = &adr[section_start..];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0003 § Implementation notes must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0003 § Implementation notes rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

// ponytail: pin the § SlicingTransform trait code block's
// method signatures. The MVP exposes a `name` accessor and
// an `apply` method that takes `(input, store) -> Result<SlicedOutput,
// TransformError>`. A contributor who re-pastes an older
// `(input, &OffloadStore)` signature or who drops the
// `name` accessor drifts the trait shape.
#[test]
fn adr_0003_slicing_trait_block_pins_method_signatures() {
    let block = adr_0003_slicing_trait_block();
    assert!(
        block.contains("fn name(&self) -> &'static str"),
        "ADR-0003 § SlicingTransform trait code block must show \
         `fn name(&self) -> &'static str` — the human-readable \
         name accessor the trait exposes for dashboard filtering.",
    );
    assert!(
        block.contains("fn apply("),
        "ADR-0003 § SlicingTransform trait code block must show \
         `fn apply(...)` — the transform entry point.",
    );
    assert!(
        block.contains("Result<SlicedOutput, TransformError>"),
        "ADR-0003 § SlicingTransform trait code block must show \
         `Result<SlicedOutput, TransformError>` — the impl's return \
         type, with `TransformError::Skipped(_)` as the explicit \
         no-op signal (ADR-0003 § Error contract).",
    );
}

// ---- Positive-direction tests (impl surfaces) ----

// ponytail: pin the § HeadTailSlicer code block's
// `Default` impl. The MVP exposes
// `impl Default for HeadTailSlicer { fn default() -> Self { Self { head_bytes: 4096, tail_bytes: 4096 } } }`
// so a future contributor instantiating
// `HeadTailSlicer::default()` directly (without passing
// explicit head/tail bytes) gets the detector-aligned
// 4096/4096 defaults. The earlier ADR only documented the
// defaults as `// default: 4096` field comments; a
// contributor who re-collapses the `Default` impl back
// to a comment drifts the public API.
#[test]
fn adr_0003_head_tail_slicer_block_documents_default_impl() {
    let block = adr_0003_head_tail_slicer_block();
    assert!(
        block.contains("impl Default for HeadTailSlicer"),
        "ADR-0003 § HeadTailSlicer code block must show \
         `impl Default for HeadTailSlicer` — the MVP exposes a \
         Default impl so callers without explicit head/tail bytes \
         get the detector-aligned 4096/4096 numbers. The earlier \
         draft documented the defaults as `// default: 4096` field \
         comments; re-collapsing the Default impl back to a \
         comment drifts the public API.",
    );
    assert!(
        block.contains("Self { head_bytes: 4096, tail_bytes: 4096 }"),
        "ADR-0003 § HeadTailSlicer code block must show \
         `Self {{ head_bytes: 4096, tail_bytes: 4096 }}` inside \
         the Default impl — matches the detector's \
         `SLICE_HEAD_BYTES` / `SLICE_TAIL_BYTES` constants. \
         Tuning the default without updating the detector \
         surfaces via the in-file `head_tail_slicer_default_matches_adr` \
         drift test.",
    );
}

// ponytail: pin the § HeadTailSlicer code block's UTF-8
// char-boundary alignment. The MVP's `apply` aligns the
// head/tail byte offsets to the nearest
// `floor_char_boundary` / `ceil_char_boundary` so a
// multi-byte CJK or emoji tool output doesn't panic at a
// mid-codepoint boundary. A contributor who re-pastes the
// older `&input[..self.head_bytes]` direct slice form
// documents a panicking API on non-ASCII inputs.
#[test]
fn adr_0003_head_tail_slicer_block_aligns_utf8_boundaries() {
    let block = adr_0003_head_tail_slicer_block();
    assert!(
        block.contains("floor_char_boundary(input, self.head_bytes)"),
        "ADR-0003 § HeadTailSlicer code block must show \
         `floor_char_boundary(input, self.head_bytes)` — the MVP \
         aligns the head byte offset to a valid char boundary so \
         non-ASCII tool output doesn't panic at a mid-codepoint \
         slice. The earlier draft used `&input[..self.head_bytes]` \
         directly, which panics on multi-byte UTF-8.",
    );
    assert!(
        block.contains("ceil_char_boundary(input,"),
        "ADR-0003 § HeadTailSlicer code block must show \
         `ceil_char_boundary(input, ...)` — same rationale as \
         `floor_char_boundary`. The drift test \
         `utf8_boundary_alignment_preserves_chars` (in \
         `slicing.rs`) pins the behaviour.",
    );
    // Negative pin: the older `&input[..self.head_bytes]` form
    // would panic on CJK. The new code reads `&input[..head_end]`
    // where `head_end` is the boundary-aligned offset.
    assert!(
        !block.contains("&input[..self.head_bytes]"),
        "ADR-0003 § HeadTailSlicer code block must not slice \
         `&input[..self.head_bytes]` directly — that form panics \
         on multi-byte UTF-8. The MVP uses `&input[..head_end]` \
         where `head_end = floor_char_boundary(input, self.head_bytes)`.",
    );
}

// ponytail: pin the § HeadTailSlicer code block's use of
// the `format_slice_marker` helper. The MVP builds the
// marker via `format_slice_marker(&key)` (defined in
// ADR-0004 § OffloadStore with the
// `SLICE_MARKER_PREFIX` / `SLICE_MARKER_SUFFIX`
// constants). The earlier draft used an inline
// `format!("<<plugin3:slice:{}>>", key)` — a contributor
// who re-pastes the inline format drifts the wire shape
// (the prefix/suffix pair is load-bearing for the
// `parse_slice_marker` retriever).
#[test]
fn adr_0003_head_tail_slicer_block_uses_format_slice_marker() {
    let block = adr_0003_head_tail_slicer_block();
    assert!(
        block.contains("format_slice_marker(&key)"),
        "ADR-0003 § HeadTailSlicer code block must call \
         `format_slice_marker(&key)` — the MVP delegates to the \
         ADR-0004 helper so the marker shape stays in lockstep \
         with `SLICE_MARKER_PREFIX` / `SLICE_MARKER_SUFFIX`. \
         The earlier draft used an inline format string; \
         re-pasting it drifts the wire shape and breaks \
         `parse_slice_marker`.",
    );
    assert!(
        !block.contains("<<plugin3:slice:{}>>"),
        "ADR-0003 § HeadTailSlicer code block must not contain \
         the inline `<<plugin3:slice:{{}}>>` format string — the \
         marker shape is owned by `format_slice_marker`. Re-pasting \
         the inline form drifts the wire shape away from \
         `SLICE_MARKER_PREFIX` / `SLICE_MARKER_SUFFIX`.",
    );
}

// ---- Negative-direction tests (phantom deps / shapes) ----

// ponytail: pin the § Implementation notes' absence of
// `tracing::warn!` in `slice_or_skip`. The earlier draft
// specified a `tracing::warn!(error = %e, "slicer failed;
// passing input through")` event. The MVP does not depend
// on `tracing` (ADR-0017 § Workspace Cargo.toml); the
// helper emits one `eprintln!` line tagged `plugin3:` and
// falls back to the no-op shape. A contributor who
// re-pastes the `tracing::warn!` event documents a dep
// the impl does not wire.
#[test]
fn adr_0003_slice_or_skip_omits_tracing_warn() {
    let block = adr_0003_slice_or_skip_block();
    assert!(
        !block.contains("tracing::warn"),
        "ADR-0003 § Implementation notes `slice_or_skip` code \
         block must not contain `tracing::warn!` — the workspace \
         does not depend on `tracing` (ADR-0017). The MVP's \
         non-Skipped error path emits one `eprintln!` line tagged \
         `plugin3:` so the host's stderr captures the regression.",
    );
    // Positive: the `eprintln!` form must be visible.
    assert!(
        block.contains("eprintln!"),
        "ADR-0003 § Implementation notes `slice_or_skip` code \
         block must show the `eprintln!` fallback log — the MVP's \
         non-Skipped error path prints \
         `plugin3: slicer failed; passing input through: {{e}}` \
         to the host's stderr.",
    );
}
