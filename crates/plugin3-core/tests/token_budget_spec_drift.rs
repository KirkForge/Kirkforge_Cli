//! ADR-0005 (Token budget) drift tests — the contracts that
//! live in the ADR prose and must stay in lockstep with the
//! `plugin3-core/src/budget.rs` impl. Companion to the in-file
//! tests inside `budget.rs` (which pin impl-side behaviour);
//! this file pins the *spec surface* — the § `TokenBudget` struct
//! example, the § `UserPromptSubmit` hook flow example, the
//! absence of phantom `tracing` events, and the absence of the
//! `HookResponse` enum.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who re-pastes the
//! `HookResponse` enum back into the ADR documents a type the
//! impl does not declare, and the type drift lands on a fresh
//! checkout's compile, not on incremental — invisible until CI
//! runs).

use std::path::{Path, PathBuf};

use plugin3_core::budget::{BudgetConfig, ConfigFile, UsageConfig};

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

fn adr_0005() -> String {
    read(&repo_root().join("docs/adr/0005-token-budget.md"))
}

/// Read ADR-0005's § `TokenBudget` struct code block.
fn adr_0005_token_budget_block() -> String {
    let adr = adr_0005();
    let section_start = adr
        .find("### TokenBudget struct")
        .expect("ADR-0005 must have a § TokenBudget struct subsection");
    let section_end = adr[section_start..]
        .find("### Default ceiling")
        .expect("ADR-0005 § TokenBudget struct must precede § Default ceiling");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0005 § TokenBudget struct must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0005 § TokenBudget struct rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0005's § `UserPromptSubmit` hook flow code block.
fn adr_0005_user_prompt_submit_block() -> String {
    let adr = adr_0005();
    let section_start = adr
        .find("### UserPromptSubmit hook flow")
        .expect("ADR-0005 must have a § UserPromptSubmit hook flow subsection");
    let section_end = adr[section_start..]
        .find("### Token estimation")
        .expect("ADR-0005 § UserPromptSubmit hook flow must precede § Token estimation");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0005 § UserPromptSubmit hook flow must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0005 § UserPromptSubmit hook flow rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0005's § Implementation notes (the entire
/// section — short, no fenced code blocks for the prose).
fn adr_0005_implementation_notes() -> String {
    let adr = adr_0005();
    let section_start = adr
        .find("## Implementation notes")
        .expect("ADR-0005 must have an Implementation notes section");
    adr[section_start..].to_string()
}

// ---- § TokenBudget struct: positive-direction tests ----

// ponytail: pin the § TokenBudget struct example to the
// impl's actual type set. The MVP exports three additional
// structs (BudgetConfig, ConfigFile, UsageConfig) on top
// of BudgetState + TokenBudget. A contributor who removes
// any of them documents a missing type the impl declares.
#[test]
fn adr_0005_token_budget_block_names_actual_types() {
    let block = adr_0005_token_budget_block();
    for ty in [
        "BudgetState",
        "TokenBudget",
        "BudgetConfig",
        "ConfigFile",
        "UsageConfig",
    ] {
        assert!(
            block.contains(ty),
            "ADR-0005 § TokenBudget struct example must name `{ty}` \
             — it is the type the impl exports from `budget.rs`. \
             A contributor who removes it documents a type the \
             impl still declares.",
        );
    }
}

// ponytail: pin the § TokenBudget struct example's serde
// attributes. The MVP's `BudgetState` enum is
// `#[serde(rename_all = "snake_case")]` and `TokenBudget`
// derives `Serialize, Deserialize` — both are load-bearing
// because `BudgetConfig` (and `ConfigFile`) round-trip via
// TOML. A contributor who drops the rename attribute
// changes the on-disk TOML spelling.
#[test]
fn adr_0005_token_budget_block_pins_serde_attributes() {
    let block = adr_0005_token_budget_block();
    assert!(
        block.contains("#[serde(rename_all = \"snake_case\")]"),
        "ADR-0005 § TokenBudget struct example must show the \
         `#[serde(rename_all = \"snake_case\")]` attribute on \
         `BudgetState` — the on-disk TOML spelling for \
         `BudgetState::Approaching` etc. depends on it.",
    );
    assert!(
        block.contains("Serialize, Deserialize") || block.contains("Serialize,Deserialize"),
        "ADR-0005 § TokenBudget struct example must derive \
         `Serialize, Deserialize` on `TokenBudget` — the runtime \
         state is read back from the budget.toml file on each hook \
         invocation (ADR-0014 § Atomic flag file).",
    );
}

// ponytail: pin the § TokenBudget struct example's
// `BudgetConfig` shape. The user-editable subset must NOT
// carry `used` — that's a session-local runtime field, not
// part of the config. A contributor who re-pastes the
// 3-field `BudgetConfig { ceiling, approaching_ratio, used }`
// form documents a type that persists a session counter
// across sessions via `budget set --default`.
//
// Scoped to the `BudgetConfig { ... }` block — `used`
// legitimately appears on `TokenBudget` itself (the runtime
// struct). The drift test is about the *user-editable
// subset*, not the runtime struct.
#[test]
fn adr_0005_token_budget_block_budget_config_excludes_used() {
    let block = adr_0005_token_budget_block();
    // ponytail: extract the BudgetConfig struct literal
    // (the `{ ... }` body between `pub struct BudgetConfig`
    // and the next `impl` / `pub struct` / closing fence).
    let bc_start = block
        .find("pub struct BudgetConfig")
        .expect("ADR-0005 § TokenBudget struct example must declare `pub struct BudgetConfig`");
    let bc_after = &block[bc_start..];
    let bc_body_start_rel = bc_after.find('{').expect("BudgetConfig must have a body");
    let bc_body_after = &bc_after[bc_body_start_rel + 1..];
    let bc_body_end_rel = bc_body_after
        .find('}')
        .expect("BudgetConfig body must close");
    let bc_body = &bc_body_after[..bc_body_end_rel];
    // Negative: `used` must not appear in BudgetConfig's body.
    assert!(
        !bc_body.contains("used"),
        "ADR-0005 § TokenBudget struct example's `BudgetConfig` \
         must NOT declare a `used` field — `used` is the runtime \
         session-local counter and must not persist across \
         sessions via `config.toml`. The drift test \
         `budget_config_round_trips_via_toml` (in `budget.rs`) \
         pins the round-trip shape.",
    );
    // Positive: BudgetConfig's two-field shape must be visible.
    assert!(
        bc_body.contains("ceiling: usize") && bc_body.contains("approaching_ratio: f64"),
        "ADR-0005 § TokenBudget struct example must declare \
         `BudgetConfig`'s two-field shape (ceiling, approaching_ratio) \
         — matches the impl.",
    );
}

// ponytail: pin the ConfigFile two-section emission as a single
// load-bearing contract. The in-file tests
// `config_file_emits_budget_section_header` (in `budget.rs`) and
// `config_file_emits_usage_section_header` (in `cost.rs`) each pin
// one section header in isolation. A contributor who drops the
// `usage` field from `ConfigFile` (to "simplify") keeps the
// `[budget]` test green and only this combined test fails. The
// drift test runs the impl's actual `toml::to_string` so a
// contributor who swaps the field for a different shape (e.g. a
// free-standing `UsageConfig` flat-serialised) also surfaces.
#[test]
fn adr_0005_config_file_emits_both_section_headers_together() {
    let file = ConfigFile {
        budget: BudgetConfig {
            ceiling: 222_000,
            approaching_ratio: 0.8,
        },
        usage: UsageConfig { enabled: false },
    };
    let s = toml::to_string(&file).expect("serialise ConfigFile");
    // Both section headers must appear in the same TOML document.
    assert!(
        s.contains("[budget]") && s.contains("[usage]"),
        "ConfigFile must serialise with both `[budget]` and `[usage]` \
         section headers together — ADR-0005 § Default ceiling and \
         ADR-0010 § Privacy both depend on the wrapper emitting the \
         section headers. Dropping either field surfaces here. \
         Got: {s}",
    );
    // And the round-trip must preserve both section values.
    let back: ConfigFile = toml::from_str(&s).expect("parse ConfigFile");
    assert_eq!(back.budget.ceiling, 222_000);
    assert!((back.budget.approaching_ratio - 0.8).abs() < f64::EPSILON);
    assert!(!back.usage.enabled);
}

// ---- § UserPromptSubmit hook flow: negative-direction tests ----

// ponytail: pin the absence of `tracing` events in the
// § UserPromptSubmit hook flow example. The earlier draft
// specified `tracing::warn!` and `tracing::info!` calls in
// every intervention arm. The MVP does not depend on
// `tracing` (ADR-0017 § Workspace Cargo.toml). The drift
// test catches a contributor who re-pastes the older
// tracing-heavy example.
#[test]
fn adr_0005_user_prompt_submit_block_does_not_claim_tracing() {
    let block = adr_0005_user_prompt_submit_block();
    for phantom in [
        "tracing::warn",
        "tracing::info",
        "tracing::error",
        "tracing::debug",
        "use tracing",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0005 § UserPromptSubmit hook flow example claims \
             `{phantom}` but the workspace does not depend on \
             `tracing`. The handler emits zero tracing events \
             today — the host's hook envelope carries the \
             intervention kind. Adding tracing is a future ADR \
             with a `tracing = \"0.1\"` dep.",
        );
    }
}

// ponytail: pin the § UserPromptSubmit hook flow example
// against the local-`HookResponse` enum that doesn't exist.
// The earlier draft declared a `pub enum HookResponse`
// with `Ok` and `OkWithWarning(String)` variants. The MVP
// uses `UserPromptSubmitResponse` from `plugin3-hosts`
// (ADR-0013) — Claude Code's hook envelope has no separate
// "ok" vs "ok-with-warning" path (the warning is the `Warn`
// variant).
#[test]
fn adr_0005_user_prompt_submit_block_uses_usersubmit_response_not_hook_response() {
    let block = adr_0005_user_prompt_submit_block();
    // Negative: the local `HookResponse` enum must not
    // appear in the example.
    assert!(
        !block.contains("HookResponse"),
        "ADR-0005 § UserPromptSubmit hook flow example must not \
         reference a local `HookResponse` enum — the impl uses \
         `UserPromptSubmitResponse` from `plugin3-hosts` \
         (ADR-0013). Claude Code's hook envelope has no separate \
         `Ok` vs `OkWithWarning` path; the warning is the `Warn` \
         variant.",
    );
    // Positive: `UserPromptSubmitResponse` must be visible.
    assert!(
        block.contains("UserPromptSubmitResponse"),
        "ADR-0005 § UserPromptSubmit hook flow example must show \
         `UserPromptSubmitResponse` from `plugin3-hosts` — the \
         response type the impl maps `Intervention` to.",
    );
}

// ponytail: pin the § UserPromptSubmit hook flow example's
// serialisation shape. The MVP serialises `Intervention`
// from `plugin3-core` directly — the two tagged enums
// (`Intervention` and `UserPromptSubmitResponse`) are
// byte-equivalent on the wire (both `#[serde(tag = "kind",
// rename_all = "snake_case")]` over the same four-variant
// shape), so the hand-written 4-arm
// `Intervention → UserPromptSubmitResponse` match is gone
// from the impl (a contributor re-introducing the match
// would duplicate the variant list and have to track
// renames in two places). The drift test enforces three
// directions on the code block: the parse-failure path
// uses `Intervention::Allow` directly, the post-decide
// path serialises `intervention` directly, and the
// 4-arm conversion match-arm form does NOT appear.
#[test]
fn adr_0005_user_prompt_submit_block_serialises_intervention_directly() {
    let block = adr_0005_user_prompt_submit_block();
    // Positive: the parse-failure path serialises
    // `Intervention::Allow` (not the host-side
    // `UserPromptSubmitResponse::Allow`). A contributor
    // who re-pastes the pre-fix form documents a serialise
    // call against the host-side enum, which contradicts
    // the byte-equivalence contract and would force the
    // CLI to import `plugin3-hosts` again.
    assert!(
        block.contains("serde_json::to_string(&Intervention::Allow)"),
        "ADR-0005 § UserPromptSubmit hook flow example must \
         serialise `Intervention::Allow` directly on the \
         parse-failure path — the impl in \
         `crates/plugin3-cli/src/hooks/mod.rs::user_prompt_submit` \
         does not reach for the host-side \
         `UserPromptSubmitResponse::Allow` (the two enums \
         are wire-equivalent so the core enum is sufficient).",
    );
    // Positive: the post-decide path serialises
    // `intervention` directly (no match-arm conversion to a
    // second enum). The match-arm form
    // `let resp = match intervention { ... }` would force
    // the CLI to update the 4-arm conversion whenever a
    // 5th `Intervention` variant is added.
    assert!(
        block.contains("serde_json::to_string(&intervention)"),
        "ADR-0005 § UserPromptSubmit hook flow example must \
         serialise `intervention` directly on the post-decide \
         path — the impl drops the 4-arm \
         `Intervention → UserPromptSubmitResponse` match in \
         favour of the byte-equivalence contract. A contributor \
         who re-pastes the match documents a duplicate variant \
         list that drifts if either enum gains a 5th variant.",
    );
    // Negative: the 4-arm conversion match must NOT appear
    // in the code block. This is the load-bearing pin — the
    // pre-fix form documented a hand-written
    // `Intervention → UserPromptSubmitResponse` conversion
    // that the impl no longer performs.
    assert!(
        !block.contains("Intervention::Allow => UserPromptSubmitResponse::Allow"),
        "ADR-0005 § UserPromptSubmit hook flow example must \
         NOT contain the 4-arm match-arm conversion \
         `Intervention::Allow => UserPromptSubmitResponse::Allow` \
         — the impl serialises `Intervention` directly. The \
         pre-fix wording documented a match-arm form that the \
         byte-equivalent enums now make redundant; re-pasting \
         it would restore a duplicate variant list.",
    );
}

// ---- § Implementation notes: HookResponse absence ----

// ponytail: pin the absence of a local `HookResponse` enum
// in § Implementation notes. The earlier draft specified
// a local enum with `Ok` and `OkWithWarning(String)`
// variants. The MVP uses `UserPromptSubmitResponse` from
// `plugin3-hosts`. The prose can mention `HookResponse` in
// the context of "was in the earlier draft, but the MVP
// doesn't declare it" — the negative check is on the
// *declaration* `pub enum HookResponse` (the literal
// type-declaration form).
#[test]
fn adr_0005_implementation_notes_does_not_declare_hook_response() {
    let section = adr_0005_implementation_notes();
    assert!(
        !section.contains("pub enum HookResponse"),
        "ADR-0005 § Implementation notes must not declare a local \
         `HookResponse` enum — the impl uses \
         `UserPromptSubmitResponse` from `plugin3-hosts`. A \
         contributor who re-pastes the local enum declaration \
         documents a type the impl does not have.",
    );
    // ponytail: positive pin — the § Implementation notes
    // must show the actual response type from
    // `plugin3-hosts` with the four-variant shape.
    assert!(
        section.contains("UserPromptSubmitResponse"),
        "ADR-0005 § Implementation notes must reference \
         `UserPromptSubmitResponse` — the actual response type the \
         handler emits.",
    );
}

// ponytail: pin the § Implementation notes' recent-outputs
// bound. The earlier draft said "bounded at 32 entries".
// The actual bound is also 32 (per ADR-0014 § Recent
// outputs file). A contributor who tunes the bound (32 →
// 64 or 32 → 16) without updating the ADR surfaces here.
#[test]
fn adr_0005_implementation_notes_pins_recent_outputs_bound() {
    let section = adr_0005_implementation_notes();
    assert!(
        section.contains("32"),
        "ADR-0005 § Implementation notes must reference the \
         32-entry recent-outputs bound (ADR-0014 § Recent \
         outputs file). A contributor who tunes the bound \
         surfaces here.",
    );
}
