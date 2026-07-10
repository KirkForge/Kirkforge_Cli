//! ADR-0008 (Compaction) drift tests — the contracts that
//! live in the ADR prose and must stay in lockstep with the
//! `plugin3-core/src/compaction.rs` impl and the
//! `plugin3-cli/src/main.rs` clap subcommand. Companion to the
//! in-file tests inside `compaction.rs` (which pin impl-side
//! behaviour); this file pins the *spec surface* — the
//! § `CompactHint` payload, the § Local summary, the
//! § Compact subcommand, and the `LocalSummaryCompactor`
//! default.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who re-pastes the
//! `BudgetCmd` enum-without-subcommand form back into the ADR
//! documents a clap structure the impl does not have).

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

fn adr_0008() -> String {
    read(&repo_root().join("docs/adr/0008-compaction-strategy.md"))
}

/// Read ADR-0008's § `CompactHint` payload code block.
fn adr_0008_compact_hint_block() -> String {
    let adr = adr_0008();
    let section_start = adr
        .find("### CompactHint payload")
        .expect("ADR-0008 must have a § CompactHint payload subsection");
    let section_end = adr[section_start..]
        .find("### Local summary")
        .expect("ADR-0008 § CompactHint payload must precede § Local summary");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0008 § CompactHint payload must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0008 § CompactHint payload rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0008's § Compact subcommand code block.
fn adr_0008_compact_subcommand_block() -> String {
    let adr = adr_0008();
    let section_start = adr
        .find("### Compact subcommand")
        .expect("ADR-0008 must have a § Compact subcommand subsection");
    let section_end = adr[section_start..]
        .find("### Conversation history")
        .expect("ADR-0008 § Compact subcommand must precede § Conversation history");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0008 § Compact subcommand must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0008 § Compact subcommand rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0008's § Local summary code block.
fn adr_0008_local_summary_block() -> String {
    let adr = adr_0008();
    let section_start = adr
        .find("### Local summary")
        .expect("ADR-0008 must have a § Local summary subsection");
    let section_end = adr[section_start..]
        .find("### Host integration")
        .expect("ADR-0008 § Local summary must precede § Host integration");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0008 § Local summary must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0008 § Local summary rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

// ---- § CompactHint payload: positive + negative tests ----

// ponytail: pin the § CompactHint payload example to the
// types the section actually contains. The MVP ships
// five types in this section: `CompactHint`, `Turn`,
// `CompactedOutput`, `CompactionTransform` (trait), and
// `LocalSummaryCompactor`. (`local_summarise` lives in
// § Local summary, not here.) A contributor who re-pastes
// the older two-type form (`CompactHint`, `build_hint`)
// documents a smaller surface than the impl ships.
#[test]
fn adr_0008_compact_hint_block_names_all_five_types() {
    let block = adr_0008_compact_hint_block();
    for ty in [
        "CompactHint",
        "Turn",
        "CompactedOutput",
        "CompactionTransform",
        "LocalSummaryCompactor",
    ] {
        assert!(
            block.contains(ty),
            "ADR-0008 § CompactHint payload example must \
             reference `{ty}` — the impl exports this type \
             from `compaction.rs`. A contributor who removes \
             it documents a smaller surface than the impl ships.",
        );
    }
}

// ponytail: pin the § CompactHint payload example's
// `CompactHint` field set. The MVP's `CompactHint` carries
// five fields: `reason`, `tokens_used`, `tokens_ceiling`,
// `oldest_turn`, `newest_turn`. The in-file test
// `compact_hint_serialises_expected_fields` pins the JSON
// wire shape with the same five keys — drift here breaks
// the host's parser before the impl can even compile.
#[test]
fn adr_0008_compact_hint_block_pins_field_set() {
    let block = adr_0008_compact_hint_block();
    for f in [
        "reason: String",
        "tokens_used: usize",
        "tokens_ceiling: usize",
        "oldest_turn: Option<usize>",
        "newest_turn: Option<usize>",
    ] {
        assert!(
            block.contains(f),
            "ADR-0008 § CompactHint payload example must \
             declare field `{f}` on `CompactHint` — matches \
             the impl's struct and the in-file test \
             `compact_hint_serialises_expected_fields`.",
        );
    }
}

// ponytail: pin the § CompactHint payload example's
// `LocalSummaryCompactor.max_output_bytes` default. The
// MVP defaults to `8192`; the in-file test
// `local_summary_compactor_default_matches_adr` enforces
// it. A contributor who tunes the default (8192 → 4096)
// without updating the ADR surfaces here.
#[test]
fn adr_0008_compact_hint_block_pins_max_output_bytes_default() {
    let block = adr_0008_compact_hint_block();
    // Positive: the default-8192 annotation must be visible.
    assert!(
        block.contains("default: 8192")
            || block.contains("max_output_bytes: usize,  // default: 8192"),
        "ADR-0008 § CompactHint payload example must show \
         the `max_output_bytes` default of 8192 — the impl's \
         `LocalSummaryCompactor::default()` returns \
         `Self {{ max_output_bytes: 8192 }}`. A contributor \
         who tunes the default surfaces here.",
    );
}

// ponytail: pin the § CompactHint payload example's
// `LocalSummaryCompactor.name()` return value. The MVP
// returns `"local_summary"`; the in-file test
// `local_summary_compactor_name_is_pinned` enforces it.
// Dashboards filter by name, so a contributor who shortens
// it to `"ls"` silently breaks the report subcommand.
#[test]
fn adr_0008_compact_hint_block_pins_compactor_name() {
    let block = adr_0008_compact_hint_block();
    // The trait declaration must be visible.
    assert!(
        block.contains("fn name(&self) -> &'static str"),
        "ADR-0008 § CompactHint payload example must declare \
         the `name(&self) -> &'static str` trait method — \
         `LocalSummaryCompactor` implements `CompactionTransform` \
         and `report --kind local_summary` filters on this name.",
    );
}

// ponytail: pin the absence of `tracing` events in the
// § CompactHint payload example. The MVP does not depend
// on `tracing` (ADR-0017 § Workspace Cargo.toml) and the
// compaction module emits zero tracing events today.
#[test]
fn adr_0008_compact_hint_block_does_not_claim_tracing() {
    let block = adr_0008_compact_hint_block();
    for phantom in [
        "tracing::warn",
        "tracing::info",
        "tracing::error",
        "tracing::debug",
        "use tracing",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0008 § CompactHint payload example claims \
             `{phantom}` but the workspace does not depend on \
             `tracing`. The compaction module emits zero \
             tracing events — the host's hook envelope carries \
             the compaction kind.",
        );
    }
}

// ---- § Local summary: positive-direction test ----

// ponytail: pin the § Local summary example's 500-char line
// limit. The MVP's `local_summarise` skips lines whose
// `.len()` exceeds 500; the in-file test
// `local_summarise_skips_empty_and_long_lines` enforces it.
// A contributor who tunes the limit (500 → 1000) without
// updating the ADR surfaces here.
#[test]
fn adr_0008_local_summary_block_pins_500_char_limit() {
    let block = adr_0008_local_summary_block();
    assert!(
        block.contains("500"),
        "ADR-0008 § Local summary example must show the \
         500-char line length limit — matches the impl's \
         `if line.len() > 500 {{ continue; }}` and the in-file \
         test `local_summarise_skips_empty_and_long_lines`. \
         A contributor who tunes the limit surfaces here.",
    );
}

// ponytail: pin the § Local summary example's pre-check bound
// shape. The MVP's `local_summarise` checks the bound BEFORE
// pushing the line — `if out.len() + line.len() + 1 > max_bytes
// { break; }` — so a single line longer than `max_bytes`
// cannot blow past the cap (the pre-fix push-then-check shape
// ignored the cap for callers with `max_bytes < line.len()`,
// producing line-sized output instead of a capped one). The
// in-file test `local_summarise_single_line_over_cap_does_not_blow_bound`
// pins the behaviour; this drift test pins the ADR's matching
// code shape so a contributor who reverts the ADR to the
// pre-fix form (push, then check) fails CI for review alongside
// the behaviour test.
#[test]
fn adr_0008_local_summary_block_uses_pre_check_bound() {
    let block = adr_0008_local_summary_block();
    assert!(
        block.contains("out.len() + line.len() + 1 > max_bytes"),
        "ADR-0008 § Local summary example must show the \
         pre-check bound `out.len() + line.len() + 1 > max_bytes` \
         — matches the impl's pre-check before the append and \
         the in-file test `local_summarise_single_line_over_cap_does_not_blow_bound`. \
         A contributor who reverts the ADR to the pre-fix \
         push-then-check shape documents a regression that lets \
         single lines longer than `max_bytes` blow past the cap.",
    );
}

// ---- § Compact subcommand: positive + negative tests ----

// ponytail: pin the § Compact subcommand example's clap
// structure. The MVP uses a `BudgetCmd` *struct* wrapping
// a `BudgetSub` *enum* (clap subcommand pattern). The
// earlier draft used a single `pub enum BudgetCmd` with
// three variants. A contributor who re-pastes the
// single-enum form documents a clap structure that does
// not match the binary's actual `--help` output.
#[test]
fn adr_0008_compact_subcommand_block_uses_struct_wrapping_enum() {
    let block = adr_0008_compact_subcommand_block();
    // Positive: the struct + sub-enum pattern must be visible.
    assert!(
        block.contains("struct BudgetCmd"),
        "ADR-0008 § Compact subcommand example must declare \
         `struct BudgetCmd` — the impl wraps a subcommand \
         enum (`BudgetSub`) in a struct so clap sees the \
         nested subcommand. A flat `enum BudgetCmd` \
         documents a clap shape the impl does not have.",
    );
    assert!(
        block.contains("enum BudgetSub"),
        "ADR-0008 § Compact subcommand example must declare \
         `enum BudgetSub` — the actual three-variant enum \
         the clap subcommand dispatches on.",
    );
    // Negative: the flat-enum shape must not appear.
    assert!(
        !block.contains("pub enum BudgetCmd"),
        "ADR-0008 § Compact subcommand example must not declare \
         `pub enum BudgetCmd` — the impl uses a struct wrapping \
         a sub-enum. A flat `pub enum BudgetCmd` documents a \
         clap shape the impl does not have.",
    );
}

// ponytail: pin the § Compact subcommand example's three
// `BudgetSub` variants. The MVP declares `Status`,
// `Set { ceiling, default }`, and `Compact { json }`. The
// earlier draft's `Set { ceiling: usize }` lacked the
// `default: bool` flag — adding the flag would have been
// a future-ADR retrofit for the `plugin3 budget set
// --default` persistence path (ADR-0015).
#[test]
fn adr_0008_compact_subcommand_block_lists_three_variants() {
    let block = adr_0008_compact_subcommand_block();
    for v in ["Status", "Set", "Compact"] {
        assert!(
            block.contains(v),
            "ADR-0008 § Compact subcommand example must \
             declare variant `{v}` on `BudgetSub` — the impl \
             has three variants. A contributor who re-pastes \
             an older two-variant form surfaces here.",
        );
    }
    // ponytail: the `Set` variant must carry the
    // `--default: bool` flag. The earlier draft had
    // `Set { ceiling: usize }` (no `default` flag) — that
    // form would force a future ADR to retrofit the
    // persistence path that ADR-0015 prescribes today.
    assert!(
        block.contains("default: bool"),
        "ADR-0008 § Compact subcommand example must declare \
         `default: bool` on the `Set` variant — the impl's \
         `BudgetSub::Set {{ ceiling, default }}` arms `plugin3 \
         budget set --default` (ADR-0015). A contributor who \
         re-pastes the older `Set {{ ceiling: usize }}` \
         documents a clap shape that drops persistence.",
    );
}

// ponytail: pin the § Compact subcommand example's
// `Compact { json }` variant. The MVP's `--json` flag
// switches the output between human-readable and JSON
// (ADR-0015 § JSON output). A contributor who re-pastes
// the older form without `json: bool` documents a
// subcommand without the `--json` opt-in.
#[test]
fn adr_0008_compact_subcommand_block_pins_compact_json_flag() {
    let block = adr_0008_compact_subcommand_block();
    assert!(
        block.contains("Compact") && block.contains("json: bool"),
        "ADR-0008 § Compact subcommand example must declare \
         `Compact {{ json: bool }}` — the impl's `BudgetSub::Compact` \
         arm switches to JSON output with `--json` (ADR-0015). \
         A contributor who drops the flag documents a \
         subcommand without the `--json` opt-in.",
    );
}
