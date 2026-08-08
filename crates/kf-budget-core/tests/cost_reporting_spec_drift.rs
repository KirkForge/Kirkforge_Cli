//! ADR-0010 (Cost reporting) drift tests — the contracts that
//! live in the ADR prose and must stay in lockstep with the
//! `kf-budget-core/src/cost.rs` impl. Companion to the in-file
//! tests inside `cost.rs` (which pin impl-side serde shapes and
//! `classify_kind` behaviour); this file pins the *spec surface*
//! — the § `UsageKind` enum, the § Emission site code block, the
//! § File location code block, the § Privacy gate, and the new
//! § Intervention → `UsageKind` mapping subsection.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who re-pastes the
//! `tracing::error!` serialise event back into the ADR
//! documents a `tracing` dep the impl does not wire).

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

fn adr_0010() -> String {
    read(&repo_root().join("docs/adr/0010-cost-reporting.md"))
}

/// Read ADR-0010's § Emission site code block.
fn adr_0010_emission_site_block() -> String {
    let adr = adr_0010();
    let section_start = adr
        .find("### Emission site")
        .expect("ADR-0010 must have a § Emission site subsection");
    let section_end = adr[section_start..]
        .find("### File location")
        .expect("ADR-0010 § Emission site must precede § File location");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0010 § Emission site must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0010 § Emission site rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0010's § File location code block.
fn adr_0010_file_location_block() -> String {
    let adr = adr_0010();
    let section_start = adr
        .find("### File location")
        .expect("ADR-0010 must have a § File location subsection");
    let section_end = adr[section_start..]
        .find("### Report subcommand")
        .expect("ADR-0010 § File location must precede § Report subcommand");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0010 § File location must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0010 § File location rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0010's § `UsageKind` enum code block.
fn adr_0010_usage_kind_block() -> String {
    let adr = adr_0010();
    let section_start = adr
        .find("### UsageKind enum")
        .expect("ADR-0010 must have a § UsageKind enum subsection");
    let section_end = adr[section_start..]
        .find("### Intervention")
        .expect("ADR-0010 § UsageKind enum must precede § Intervention → UsageKind mapping");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0010 § UsageKind enum must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0010 § UsageKind enum rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0010's § Intervention → `UsageKind` mapping code block.
fn adr_0010_classify_kind_block() -> String {
    let adr = adr_0010();
    let section_start = adr
        .find("### Intervention")
        .expect("ADR-0010 must have a § Intervention → UsageKind mapping subsection");
    let section_end = adr[section_start..]
        .find("### Emission site")
        .expect("ADR-0010 § Intervention mapping must precede § Emission site");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0010 § Intervention mapping must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0010 § Intervention mapping rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0010's § Privacy subsection (short, no fenced code
/// block for the prose — the TOML example is fenced).
fn adr_0010_privacy_subsection() -> String {
    let adr = adr_0010();
    let section_start = adr
        .find("### Privacy")
        .expect("ADR-0010 must have a § Privacy subsection");
    let section_end = adr[section_start..]
        .find("## Consequences")
        .expect("ADR-0010 § Privacy must precede § Consequences");
    adr[section_start..section_start + section_end].to_string()
}

// ---- § UsageKind enum: positive-direction tests ----

// ponytail: pin the § UsageKind enum example to the impl's
// actual variant set. The MVP declares six variants; a
// contributor who re-pastes the older four-variant
// (Slice/BudgetWarn/BudgetOver/CompactHint) shape documents
// a smaller enum than the impl ships.
#[test]
fn adr_0010_usage_kind_block_names_all_six_variants() {
    let block = adr_0010_usage_kind_block();
    for v in [
        "Slice",
        "BudgetWarn",
        "BudgetOver",
        "CompactHint",
        "Prompt",
        "Response",
    ] {
        assert!(
            block.contains(v),
            "ADR-0010 § UsageKind enum example must declare \
             variant `{v}` — matches the impl's enum in \
             `cost.rs`. A contributor who removes it documents \
             a smaller enum than the impl ships.",
        );
    }
}

// ponytail: pin the § UsageKind enum example's serde
// attribute. The MVP's `UsageKind` is
// `#[serde(rename_all = "snake_case")]` — the on-disk
// JSONL spelling for `UsageKind::BudgetWarn` is
// `"budget_warn"`. A contributor who drops the attribute
// breaks the wire format that `report --kind budget_warn`
// and the JSONL aggregator in `report::aggregate_sessions`
// both depend on.
#[test]
fn adr_0010_usage_kind_block_pins_serde_rename() {
    let block = adr_0010_usage_kind_block();
    assert!(
        block.contains("#[serde(rename_all = \"snake_case\")]"),
        "ADR-0010 § UsageKind enum example must show the \
         `#[serde(rename_all = \"snake_case\")]` attribute — \
         the on-disk JSONL spelling for `UsageKind::BudgetWarn` \
         is `\"budget_warn\"`. A contributor who drops the \
         attribute breaks the wire format that `report --kind` \
         and `report::aggregate_sessions` depend on.",
    );
}

// ponytail: pin the § UsageKind enum example's `UsageConfig`
// type. The `[usage] enabled = false` TOML section (ADR-0010
// § Privacy) is backed by `UsageConfig` — a one-field struct
// (`enabled: bool`). A contributor who re-pastes the older
// shape without `UsageConfig` documents a backing type the
// impl declares.
#[test]
fn adr_0010_usage_kind_block_declares_usage_config() {
    let block = adr_0010_usage_kind_block();
    assert!(
        block.contains("pub struct UsageConfig"),
        "ADR-0010 § UsageKind enum example must declare \
         `pub struct UsageConfig` — the backing type for the \
         `[usage]` TOML section (ADR-0010 § Privacy). The impl \
         declares `UsageConfig` in `cost.rs` (alongside `UsageKind` \
         so the matching module owns it). A contributor who \
         removes it documents a missing type the impl exports.",
    );
    assert!(
        block.contains("pub enabled: bool"),
        "ADR-0010 § UsageKind enum example must declare \
         `pub enabled: bool` on `UsageConfig` — the on/off \
         flag the § Privacy gate reads from config.toml.",
    );
}

// ---- § Emission site: negative-direction tests ----

// ponytail: pin the absence of `tracing` events in the
// § Emission site example. The earlier draft specified
// `tracing::error!` on serialise failure and `tracing::warn!`
// on file-open failure. The MVP does not depend on `tracing`
// (ADR-0017 § Workspace Cargo.toml) — both error paths emit
// ponytail: pin the § Emission site example's negative
// shape — the block must NOT contain `eprintln!` or the old
// `plugin3:` prefix. The impl migrated from `eprintln!` to
// `tracing::warn!` when `kf-budget-core` added tracing support.
#[test]
fn adr_0010_emission_site_uses_tracing_not_eprintln() {
    let block = adr_0010_emission_site_block();
    for phantom in ["eprintln!", "plugin3:"] {
        assert!(
            !block.contains(phantom),
            "ADR-0010 § Emission site code block must not contain \
             `{phantom}` — the impl uses `tracing::warn!` with \
             `kf-budget:` prefix.",
        );
    }
}

// ponytail: pin the § Emission site example's positive
// `tracing::warn!` shape. The impl's serialise-failure path emits
// `tracing::warn!("kf-budget: failed to serialise usage record: {e}")`
// and the open-failure path emits
// `tracing::warn!("kf-budget: usage.jsonl open failed ({e}); ...")`.
#[test]
fn adr_0010_emission_site_block_uses_tracing_for_errors() {
    let block = adr_0010_emission_site_block();
    assert!(
        block.contains("tracing::warn!(\"kf-budget: failed to serialise usage record"),
        "ADR-0010 § Emission site example must show the \
         `tracing::warn!(\"kf-budget: failed to serialise usage record: ...\")` \
         call on serialise failure — matches the impl's \
         serialise-error path.",
    );
    assert!(
        block.contains("tracing::warn!(\"kf-budget: usage.jsonl open failed"),
        "ADR-0010 § Emission site example must show the \
          `tracing::warn!(\"kf-budget: usage.jsonl open failed ...\")` \
          call on file-open failure — matches the impl's \
          open-error path.",
    );
}

// ponytail: pin the § Emission site example's
// `emit_usage` signature. The MVP's public function takes
// `record: &UsageRecord` (by reference, not by value). The
// impl's path-parameterised `emit_usage_at` does the real
// work; `emit_usage` is a thin wrapper. A contributor who
// re-pastes the older `record: UsageRecord` by-value
// signature documents a signature the impl does not have.
#[test]
fn adr_0010_emission_site_block_passes_record_by_reference() {
    let block = adr_0010_emission_site_block();
    // Positive: the `&UsageRecord` signature must be visible
    // on the public `emit_usage` function.
    assert!(
        block.contains("fn emit_usage(record: &UsageRecord)"),
        "ADR-0010 § Emission site example must declare \
         `fn emit_usage(record: &UsageRecord)` — the impl \
         takes by reference so `emit_usage_at` can be called \
         without moving the caller's record.",
    );
    // Positive: the path-parameterised `emit_usage_at` core
    // must be visible (the test in `cost.rs` targets it).
    assert!(
        block.contains("fn emit_usage_at(record: &UsageRecord, path: &std::path::Path)"),
        "ADR-0010 § Emission site example must declare \
         `fn emit_usage_at(record: &UsageRecord, path: &std::path::Path)` \
         — the path-parameterised core that tests target via tempdir.",
    );
}

// ---- § File location: positive + negative tests ----

// ponytail: pin the § File location example's path
// resolution. The MVP delegates to `Paths::resolve().usage_log()`
// (ADR-0014) — no inline `std::env::var("PLUGIN3_DATA_DIR")`
// + `directories::ProjectDirs` chain. A contributor who
// re-pastes the older inline-resolution form documents a
// path-resolution code path that doesn't match `Paths::resolve`.
#[test]
fn adr_0010_file_location_block_delegates_to_paths_resolve() {
    let block = adr_0010_file_location_block();
    // Positive: the `Paths::resolve().usage_log()` delegation
    // must be visible.
    assert!(
        block.contains("Paths::resolve().usage_log()"),
        "ADR-0010 § File location example must show \
         `Paths::resolve().usage_log()` — the MVP delegates \
         env-var + XDG resolution to ADR-0014's `Paths::resolve`. \
         An inline resolution chain documents a path-resolution \
         code path that diverges from `Paths::resolve`.",
    );
    // Negative: the inline `std::env::var("PLUGIN3_DATA_DIR")`
    // + `directories::ProjectDirs` chain must not appear in the
    // § File location example. The `directories` crate IS wired
    // (ADR-0017 § Workspace Cargo.toml: `directories = "5"`) and
    // IS consumed by `Paths::resolve()` (ADR-0014 § Path resolver)
    // — but only there. Inlining a second `directories::ProjectDirs`
    // call in `cost.rs` would create two XDG-resolution sites that
    // drift apart when ADR-0014's precedence chain changes.
    assert!(
        !block.contains("directories::ProjectDirs"),
        "ADR-0010 § File location example must not reference \
         `directories::ProjectDirs` directly — `cost.rs` delegates \
         to `Paths::resolve()` (ADR-0014) so the XDG resolution site \
         lives in exactly one place. The `directories` crate is wired \
         (ADR-0017 § Workspace Cargo.toml: `directories = \"5\"`); an \
         inline second call here creates a duplicate resolution site \
         that drifts when ADR-0014's chain changes.",
    );
    assert!(
        !block.contains("std::env::var(\"PLUGIN3_DATA_DIR\")"),
        "ADR-0010 § File location example must not inline the \
         `PLUGIN3_DATA_DIR` env-var lookup — `Paths::resolve` \
         owns the precedence chain (ADR-0014). Inline lookups \
         create drift when ADR-0014's chain changes.",
    );
}

// ---- § Intervention → UsageKind mapping: positive tests ----

// ponytail: pin the § Intervention mapping example's
// four-arm match. The MVP's `classify_kind` returns
// `Option<UsageKind>` mapping the four `Intervention`
// variants to the four reachable kinds. A contributor who
// adds a fifth `Intervention` variant but forgets to update
// this match fails to compile (good) — but a contributor
// who re-pastes a four-arm `classify_kind` and forgets to
// add the `Allow → None` arm documents a different bug
// (every healthy turn inflates the warnings count).
#[test]
fn adr_0010_classify_kind_block_lists_four_arms() {
    let block = adr_0010_classify_kind_block();
    // ponytail: each Intervention variant must be visible
    // in the match. The `Allow` arm is the load-bearing one
    // (it returns `None`); the other three are positive
    // mappings to BudgetWarn/Slice/BudgetOver.
    for arm in [
        "Intervention::Allow => None",
        "Intervention::Warn",
        "Intervention::Slice",
        "Intervention::Compact",
    ] {
        assert!(
            block.contains(arm),
            "ADR-0010 § Intervention mapping example must \
             contain arm `{arm}` — the impl's `classify_kind` \
             match has all four arms. A contributor who \
             re-pastes an older 3-arm match (no Allow → None) \
             documents a regression that would inflate the \
             warnings count.",
        );
    }
    // ponytail: the `Allow → None` arm must be the literal
    // form (returns `None`, not `Some(...)`). The earlier
    // draft's buggy form was `Allow => Some(UsageKind::Slice)`.
    assert!(
        block.contains("Intervention::Allow => None"),
        "ADR-0010 § Intervention mapping example must show \
         `Intervention::Allow => None` — a healthy turn at \
         `Under` state is not a 'significant event' and must \
         not inflate the warnings count.",
    );
    // ponytail: the `Compact → BudgetOver` arm must be visible
    // (the ADR's earlier draft mapped Compact to CompactHint;
    // the impl's call site maps it to BudgetOver because
    // both mean "the budget couldn't hold").
    assert!(
        block.contains("Intervention::Compact") && block.contains("UsageKind::BudgetOver"),
        "ADR-0010 § Intervention mapping example must show \
         `Intervention::Compact => Some(UsageKind::BudgetOver)` \
         — the impl treats a Compact suggestion and a \
         BudgetOver turn as the same kind (a single filter \
         catches both pressures).",
    );
}

// ---- § Privacy: gate field reference ----

// ponytail: pin the § Privacy prose's positive gate
// reference. The MVP reads `ConfigFile.usage.enabled`
// (not a free-standing `enabled` field) — the path from
// the § Privacy prose to the impl goes through `ConfigFile`.
// A contributor who re-pastes the older "set the flag and
// emit_usage checks it" without naming the type documents
// a gate that has no concrete shape.
#[test]
fn adr_0010_privacy_section_references_config_file_usage_enabled() {
    let section = adr_0010_privacy_subsection();
    assert!(
        section.contains("ConfigFile.usage.enabled"),
        "ADR-0010 § Privacy must reference `ConfigFile.usage.enabled` \
         — the actual gate the impl reads. A contributor who \
         describes the gate as a free-standing field documents \
         a path the impl does not take.",
    );
    // ponytail: the § Privacy prose must mention that
    // *malformed* config defaults to enabled — the in-file
    // test `is_usage_enabled_tolerates_malformed_config`
    // pins this. A contributor who removes the malformed
    // clause documents a regression where a typo silently
    // disables reporting.
    assert!(
        section.contains("malformed") && section.contains("enabled"),
        "ADR-0010 § Privacy must mention the malformed-config \
         defaults-to-enabled behaviour — matches the impl's \
         `.unwrap_or(true)` and the in-file test \
         `is_usage_enabled_tolerates_malformed_config`.",
    );
}
