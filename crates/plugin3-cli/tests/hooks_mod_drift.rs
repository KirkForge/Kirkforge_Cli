//! ADR-0009 (Hook surface) drift tests — the contracts that
//! live in the ADR prose and must stay in lockstep with the
//! `plugin3-cli/src/hooks/mod.rs` impl. Companion to the
//! `register_hooks_*` tests inside `hooks/mod.rs` (which pin
//! the impl-side serde shape); this file pins the *spec
//! surface* — the § Hook registry code block, the absence of
//! phantom `tracing` events, and the absence of the
//! `"matcher": "*"` JSON claim.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who re-pastes the
//! `tracing::warn!` timeout event back into the ADR documents
//! a `tracing` dep the impl does not wire, and the resulting
//! `cargo build` breakage lands on a fresh checkout, not on
//! incremental — invisible until CI runs).

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

fn adr_0009() -> String {
    read(&repo_root().join("docs/adr/0009-hooks-model.md"))
}

/// Read ADR-0009's § Hook registry code block (the only fenced
/// `rust` block in that subsection).
fn adr_0009_hook_registry_block() -> String {
    let adr = adr_0009();
    let section_start = adr
        .find("### Hook registry")
        .expect("ADR-0009 must have a § Hook registry subsection");
    let section_end = adr[section_start..]
        .find("### PostToolUse flow")
        .expect("ADR-0009 § Hook registry must precede § PostToolUse flow");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0009 § Hook registry must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0009 § Hook registry rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0009's § Timeout discipline subsection (short,
/// no fenced code block — the negative check scans the whole
/// subsection for the phantom `tracing::warn!` event).
fn adr_0009_timeout_subsection() -> String {
    let adr = adr_0009();
    let section_start = adr
        .find("### Timeout discipline")
        .expect("ADR-0009 must have a § Timeout discipline subsection");
    let section_end = adr[section_start..]
        .find("### Concurrency")
        .expect("ADR-0009 § Timeout discipline must precede § Concurrency");
    adr[section_start..section_start + section_end].to_string()
}

/// Read ADR-0009's § Error contract subsection.
fn adr_0009_error_contract_subsection() -> String {
    let adr = adr_0009();
    let section_start = adr
        .find("### Error contract")
        .expect("ADR-0009 must have a § Error contract subsection");
    let section_end = adr[section_start..]
        .find("## Consequences")
        .expect("ADR-0009 § Error contract must precede § Consequences");
    adr[section_start..section_start + section_end].to_string()
}

/// Read ADR-0009's § Implementation notes (the entire
/// section).
fn adr_0009_implementation_notes() -> String {
    let adr = adr_0009();
    let section_start = adr
        .find("## Implementation notes")
        .expect("ADR-0009 must have an Implementation notes section");
    adr[section_start..].to_string()
}

/// Read ADR-0009's § `PostToolUse` flow code block (the
/// first fenced `rust` block in that subsection).
fn adr_0009_post_tool_use_flow_block() -> String {
    let adr = adr_0009();
    let section_start = adr
        .find("### PostToolUse flow")
        .expect("ADR-0009 must have a § PostToolUse flow subsection");
    let section_end = adr[section_start..]
        .find("### UserPromptSubmit flow")
        .expect("ADR-0009 § PostToolUse flow must precede § UserPromptSubmit flow");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0009 § PostToolUse flow must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0009 § PostToolUse flow rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0009's § `UserPromptSubmit` flow code block.
fn adr_0009_user_prompt_submit_flow_block() -> String {
    let adr = adr_0009();
    let section_start = adr
        .find("### UserPromptSubmit flow")
        .expect("ADR-0009 must have a § UserPromptSubmit flow subsection");
    let section_end = adr[section_start..]
        .find("### PreCompact flow")
        .expect("ADR-0009 § UserPromptSubmit flow must precede § PreCompact flow");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0009 § UserPromptSubmit flow must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0009 § UserPromptSubmit flow rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0009's § `PreCompact` flow code block.
fn adr_0009_pre_compact_flow_block() -> String {
    let adr = adr_0009();
    let section_start = adr
        .find("### PreCompact flow")
        .expect("ADR-0009 must have a § PreCompact flow subsection");
    let section_end = adr[section_start..]
        .find("### Timeout discipline")
        .expect("ADR-0009 § PreCompact flow must precede § Timeout discipline");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0009 § PreCompact flow must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0009 § PreCompact flow rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

// ---- Negative-direction tests (phantom shapes / deps) ----

// ponytail: pin the absence of the singular-Option shape.
// The earlier draft declared
// `pub post_tool_use: Option<CommandHook>`. The actual
// type uses `Option<Vec<CommandHook>>` to match Claude
// Code's array-of-hooks schema. A contributor who re-pastes
// the singular shape documents a wire format the impl does
// not emit.
#[test]
fn adr_0009_hook_registry_block_uses_vec_not_singular() {
    let block = adr_0009_hook_registry_block();
    // Negative: the singular shape (no Vec wrapper) must
    // not appear in the field declarations.
    assert!(
        !block.contains("Option<CommandHook>"),
        "ADR-0009 § Hook registry must use `Option<Vec<CommandHook>>` \
         for each hook slot — Claude Code's settings.json schema is an \
         array of hook entries per slot (some hosts run multiple \
         commands per hook). The singular `Option<CommandHook>` form \
         documented a wire format the impl does not emit.",
    );
    // Positive: the Vec wrapper must be visible.
    assert!(
        block.contains("Option<Vec<CommandHook>>"),
        "ADR-0009 § Hook registry must declare each hook slot as \
         `Option<Vec<CommandHook>>` — matches Claude Code's array \
         schema and the impl's `register_hooks` Claude Code arm.",
    );
}

// ponytail: pin the absence of the `timeout_seconds` field
// name. The earlier draft used `timeout_seconds: u64`. The
// actual field is `timeout: u64` because Claude Code's
// settings.json reads `"timeout": 5` (no `_seconds`
// suffix). A contributor who re-pastes the older field name
// documents a JSON wire format the impl does not emit.
#[test]
fn adr_0009_hook_registry_block_uses_timeout_not_timeout_seconds() {
    let block = adr_0009_hook_registry_block();
    assert!(
        !block.contains("timeout_seconds"),
        "ADR-0009 § Hook registry must not declare a `timeout_seconds` \
         field — Claude Code's settings.json reads `\"timeout\": 5`. \
         The impl's CommandHook struct uses `timeout: u64` to match \
         the wire format. The drift test \
         `register_hooks_claude_code_matches_adr_shape` (inside \
         `hooks/mod.rs`) pins the actual JSON field name.",
    );
    // Positive: the `timeout` field must be visible.
    assert!(
        block.contains("timeout: u64"),
        "ADR-0009 § Hook registry must declare `timeout: u64` on \
         `CommandHook` — matches the impl's field and Claude Code's \
         wire format.",
    );
}

// ponytail: pin the `kind` field with `#[serde(rename = "type")]`
// attribute. The earlier draft omitted the `kind` field (Claude
// Code's schema requires `"type": "command"` as a discriminator
// per hook entry). The impl declares
// `#[serde(rename = "type")] pub kind: &'static str`. A
// contributor who re-pastes the older 2-field shape documents
// a wire format Claude Code would reject.
#[test]
fn adr_0009_hook_registry_block_includes_kind_with_serde_rename() {
    let block = adr_0009_hook_registry_block();
    assert!(
        block.contains("#[serde(rename = \"type\")]"),
        "ADR-0009 § Hook registry must show the `#[serde(rename = \
         \"type\")]` attribute on the `kind` field — Claude Code's \
         settings.json requires `\"type\": \"command\"` as a \
         discriminator per hook entry, and the rename keeps the Rust \
         field as `kind` while the wire field reads `type`.",
    );
    assert!(
        block.contains("kind: &'static str"),
        "ADR-0009 § Hook registry must declare `kind: &'static str` \
         on `CommandHook` — the impl's serde shape requires this \
         discriminator field for the JSON wire format.",
    );
}

// ponytail: pin the absence of the tracing::warn! event in
// § Timeout discipline. The earlier draft specified a
// synchronous `std::time::Instant` + thread guard with a
// `tracing::warn!(hook = "PostToolUse", elapsed_ms, ...)`
// event on timeout. The MVP does not depend on `tracing`
// (ADR-0017 § Workspace Cargo.toml), and the host itself
// enforces the timeout via the settings.json `timeout`
// field.
#[test]
fn adr_0009_timeout_section_omits_tracing_warn() {
    let section = adr_0009_timeout_subsection();
    // Scoped to the code block (the explanatory paragraph can
    // mention `tracing` in the context of "the earlier draft
    // specified ... but the MVP doesn't ship it").
    if let Some(fence_start) = section.find("```rust\n") {
        let fence_after = &section[fence_start + "```rust\n".len()..];
        if let Some(fence_end_rel) = fence_after.find("```") {
            let block = &fence_after[..fence_end_rel];
            assert!(
                !block.contains("tracing::warn"),
                "ADR-0009 § Timeout discipline code block must not \
                 contain `tracing::warn!` — the workspace does not \
                 depend on `tracing`. The host's settings.json \
                 `timeout` field is the load-bearing guard.",
            );
        }
    }
    // ponytail: also pin the positive-claim direction. The
    // § Timeout discipline prose can say "the MVP does NOT
    // depend on `tracing`" — that's the negation pattern.
    // The negative check is on positive claims ("depends on
    // `tracing`", "requires `tracing`") that would document a
    // dep the impl doesn't wire.
    for positive_claim in [
        "depends on `tracing`",
        "requires `tracing`",
        "uses `tracing::warn`",
    ] {
        assert!(
            !section.contains(positive_claim),
            "ADR-0009 § Timeout discipline must not assert \
             `{positive_claim}` — the workspace does not depend on \
             `tracing`. The host enforces the timeout via the \
             `timeout` field in settings.json.",
        );
    }
}

// ponytail: pin the absence of the tracing::error! event in
// § Error contract. The earlier draft specified a
// `tracing::error!(error = %e, "hook handler failed; ...")`
// event on handler failure. The MVP does not depend on
// `tracing`; handlers short-circuit to passthrough on parse
// failure with one `eprintln!` to stderr.
#[test]
fn adr_0009_error_contract_section_omits_tracing_error() {
    let section = adr_0009_error_contract_subsection();
    if let Some(fence_start) = section.find("```rust\n") {
        let fence_after = &section[fence_start + "```rust\n".len()..];
        if let Some(fence_end_rel) = fence_after.find("```") {
            let block = &fence_after[..fence_end_rel];
            assert!(
                !block.contains("tracing::error"),
                "ADR-0009 § Error contract code block must not \
                 contain `tracing::error!` — the workspace does not \
                 depend on `tracing`. The MVP's handler-error path \
                 emits one `eprintln!` line tagged `plugin3:` to \
                 the host's stderr.",
            );
        }
    }
    for positive_claim in [
        "depends on `tracing`",
        "requires `tracing`",
        "uses `tracing::error`",
    ] {
        assert!(
            !section.contains(positive_claim),
            "ADR-0009 § Error contract must not assert `{positive_claim}` \
             — the workspace does not depend on `tracing`. Handler \
             errors short-circuit to passthrough with one `eprintln!`.",
        );
    }
}

// ponytail: pin the absence of the `"matcher": "*"` claim in
// § Implementation notes. The earlier draft's JSON example
// included `"matcher": "*"` on the PostToolUse entry. The
// MVP's serde-emitted JSON omits the `matcher` field — Claude
// Code treats its absence as "match all". A contributor who
// re-pastes the matcher entry documents a JSON wire shape the
// impl does not emit.
//
// Scoped to the fenced `json` block only — the explanatory
// paragraph below the block needs to mention "matcher" in
// the context of "the earlier draft showed `matcher`, but the
// MVP omits it".
#[test]
fn adr_0009_implementation_notes_omits_matcher_field() {
    let section = adr_0009_implementation_notes();
    let fence_start = section
        .find("```json\n")
        .expect("ADR-0009 § Implementation notes must contain a json code block");
    let fence_after = &section[fence_start + "```json\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0009 § Implementation notes json code block must close");
    let block = &fence_after[..fence_end_rel];
    assert!(
        !block.contains("matcher"),
        "ADR-0009 § Implementation notes JSON example must not \
         contain the `matcher` key — the MVP's serde output \
         omits it (Claude Code treats absence as 'match all').",
    );
}

// ponytail: pin the § Implementation notes' positive
// JSON-shape claim. The drift test inside `hooks/mod.rs`
// pins the impl's actual emitted JSON; this test pins the
// ADR's documented JSON shape so the spec and the impl stay
// in lockstep.
#[test]
fn adr_0009_implementation_notes_json_example_pins_wire_fields() {
    let section = adr_0009_implementation_notes();
    let fence_start = section
        .find("```json\n")
        .expect("ADR-0009 § Implementation notes must contain a json code block");
    let fence_after = &section[fence_start + "```json\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0009 § Implementation notes json code block must close");
    let block = &fence_after[..fence_end_rel];

    // ponytail: pin the three wire fields the impl actually
    // emits. A contributor who adds a stray field
    // (`"matcher": "*"`, `"cwd": "..."`, etc.) or renames
    // `type` → `kind` or `timeout` → `timeout_seconds`
    // surfaces here.
    assert!(
        block.contains("\"type\": \"command\""),
        "ADR-0009 § Implementation notes JSON example must show \
         `\"type\": \"command\"` per hook entry — Claude Code's \
         discriminator field. The drift test \
         `register_hooks_claude_code_matches_adr_shape` pins the \
         same field on the impl side.",
    );
    assert!(
        block.contains("\"timeout\": 5")
            && block.contains("\"timeout\": 2")
            && block.contains("\"timeout\": 10"),
        "ADR-0009 § Implementation notes JSON example must show \
         the three timeout values 5/2/10 (PostToolUse/UserPromptSubmit/PreCompact) — \
         matches the impl's `register_hooks` Claude Code arm.",
    );
    assert!(
        block.contains("\"plugin3 hook post-tool-use\"")
            && block.contains("\"plugin3 hook user-prompt-submit\"")
            && block.contains("\"plugin3 hook pre-compact\""),
        "ADR-0009 § Implementation notes JSON example must show the \
         three plugin3 hook command strings.",
    );
}

// ponytail: pin the § Hook registry code block's
// `register_hooks` shape. The Claude Code arm must
// explicitly construct all three slots (no
// `claude_code::hooks()` indirection — the impl inlines
// the construction). A contributor who re-pastes the
// `Host::ClaudeCode => claude_code::hooks()` form documents
// an indirection the impl does not have.
#[test]
fn adr_0009_hook_registry_block_inlines_claude_code_arm() {
    let block = adr_0009_hook_registry_block();
    assert!(
        !block.contains("claude_code::hooks()"),
        "ADR-0009 § Hook registry must not reference a \
         `claude_code::hooks()` helper — the impl inlines the Claude \
         Code arm's three hook slots directly. A future refactor that \
         extracts a `claude_code::hooks()` helper must update both \
         the ADR and the impl together.",
    );
    // Positive: the Cursor/Aider collapse-to-default arm
    // must be visible.
    assert!(
        block.contains("HookConfig::default()"),
        "ADR-0009 § Hook registry must show Cursor/Aider collapsing \
         to `HookConfig::default()` — the impl returns an empty \
         config for non-ClaudeCode hosts until a future ADR wires \
         their settings formats.",
    );
}

// ponytail: pin the § PostToolUse flow code block's
// call to the `run_orchestrator` free function. The
// earlier draft documented `orchestrator.run(&[...])`
// (a method call on the `SlicingOrchestrator` value) —
// the impl calls `run_orchestrator(&orch, &[...])`, a
// free function exported from `plugin3_core` as
// `orchestrator::run`. A contributor who re-pastes the
// method-call form documents an API the impl does not
// exercise.
#[test]
fn adr_0009_post_tool_use_flow_uses_run_orchestrator_free_fn() {
    let block = adr_0009_post_tool_use_flow_block();
    assert!(
        block.contains("run_orchestrator(&orch,"),
        "ADR-0009 § PostToolUse flow code block must call \
         `run_orchestrator(&orch, &[...])` — the free function \
         exported as `plugin3_core::orchestrator::run`. The impl \
         does not expose a `SlicingOrchestrator::run` method.",
    );
    assert!(
        !block.contains("orchestrator.run("),
        "ADR-0009 § PostToolUse flow code block must not call \
         `orchestrator.run(...)` — the impl routes through the \
         `run_orchestrator` free function, not a method on the \
         orchestrator value. Re-pasting the method form documents \
         an API the impl does not exercise.",
    );
}

// ponytail: pin the § PostToolUse flow code block's
// `UsageRecord` field shape. The `bytes_in` and
// `bytes_out` fields are `Option<usize>` in the struct
// (ADR-0010 § Wire shape), so the impl writes them as
// `Some(bytes_in)` / `Some(bytes_out)` and fills the
// remaining fields via `..empty_record()`. A
// contributor who re-pastes the bare `usize` form
// (`bytes_in: payload.content.len()`) documents a
// wire shape the impl does not emit.
#[test]
fn adr_0009_post_tool_use_flow_uses_optional_byte_counts() {
    let block = adr_0009_post_tool_use_flow_block();
    assert!(
        block.contains("bytes_in: Some("),
        "ADR-0009 § PostToolUse flow code block must write \
         `bytes_in: Some(bytes_in)` — `UsageRecord::bytes_in` is \
         `Option<usize>` (ADR-0010 § Wire shape), so the value is \
         always wrapped in `Some(...)`.",
    );
    assert!(
        block.contains("bytes_out: Some("),
        "ADR-0009 § PostToolUse flow code block must write \
         `bytes_out: Some(bytes_out)` — same rationale as \
         `bytes_in`. The earlier draft summed the decisions \
         inline; the impl extracts `bytes_out` from the match \
         arm and wraps it in `Some(...)`.",
    );
    assert!(
        block.contains("..empty_record()"),
        "ADR-0009 § PostToolUse flow code block must spread \
         `..empty_record()` to fill the unset fields (`ts`, \
         `session_id` is set explicitly). The impl does not \
         write `ts: now()` by hand — `empty_record()` handles it.",
    );
}

// ponytail: pin the § UserPromptSubmit flow code block's
// call to `decide` (not `budget_handle`). The earlier
// draft named the budget helper `budget_handle`; the impl
// uses `decide` from `plugin3_core::budget`. A contributor
// who re-pastes the older name documents a function the
// impl does not export.
#[test]
fn adr_0009_user_prompt_submit_flow_calls_decide() {
    let block = adr_0009_user_prompt_submit_flow_block();
    assert!(
        block.contains("decide(&b, incoming, &recent)"),
        "ADR-0009 § UserPromptSubmit flow code block must call \
         `decide(&b, incoming, &recent)` — the budget guard \
         exported as `plugin3_core::budget::decide`. The impl \
         threads the typed `BudgetState`, the estimated incoming \
         tokens, and the recent-outputs list.",
    );
    assert!(
        !block.contains("budget_handle("),
        "ADR-0009 § UserPromptSubmit flow code block must not \
         call `budget_handle(...)` — the impl uses `decide`, not \
         `budget_handle`. Re-pasting the older name documents a \
         function the impl does not export.",
    );
}

// ponytail: pin the § PreCompact flow code block's
// emit shape. The earlier draft documented
// `PreCompactResponse::compact(hint)`; the impl emits a
// raw `json!({ \"hint\": ..., \"summary\": ... })` shape
// (the host shim's `PreCompactResponse` does not have a
// `compact` constructor). A contributor who re-pastes
// the `compact(hint)` form documents an API the host
// shim does not expose.
#[test]
fn adr_0009_pre_compact_flow_emits_json_hint_and_summary() {
    let block = adr_0009_pre_compact_flow_block();
    assert!(
        block.contains("\"hint\": hint"),
        "ADR-0009 § PreCompact flow code block must emit a JSON \
         object with a `\"hint\"` key set to the `CompactHint` \
         value — the impl uses `json!({{ \"hint\": hint, \"summary\": ... }})`. \
         `PreCompactResponse::compact` is not the impl's emit shape.",
    );
    assert!(
        block.contains("\"summary\": summary_text"),
        "ADR-0009 § PreCompact flow code block must emit a JSON \
         object with a `\"summary\"` key — the impl runs the \
         `LocalSummaryCompactor` over the joined turn previews \
         and emits the result alongside the hint.",
    );
    assert!(
        !block.contains("PreCompactResponse::compact("),
        "ADR-0009 § PreCompact flow code block must not call \
         `PreCompactResponse::compact(hint)` — the host shim's \
         `PreCompactResponse` enum does not have a `compact` \
         constructor. The impl emits the raw JSON shape directly \
         via `serde_json::json!({{ ... }})`.",
    );
}

// ponytail: pin the § PostToolUse flow code block's
// `emit_usage` gate. The MVP emits a `UsageKind::Slice`
// record only when the orchestrator decided to slice —
// the match tuple binds a `sliced: bool` (`true` from
// the `Sliced` arm, `false` from the `Keep` arm) and
// `emit_usage(...)` is wrapped in `if sliced { ... }`.
// Without the gate, every PostToolUse (including Keep
// decisions with `bytes_in == bytes_out`) would emit a
// slice record, inflating `records` and the
// `plugin3 report --kind slice` count. The aggregator's
// `bytes_saved` roll-up uses `saturating_sub(bytes_in,
// bytes_out)`, so it stays at 0 for Keep rows — but
// the record itself counted as a slice event. The
// orchestrator invariant
// (`total_bytes_saved_sums_only_sliced_offloaded`) treats
// Keep rows as no-ops; the CLI's emit gate matches.
#[test]
fn adr_0009_post_tool_use_flow_gates_emit_on_sliced_decision() {
    let block = adr_0009_post_tool_use_flow_block();

    // Positive: the gate's `if sliced` must wrap the emit.
    // We assert on the binding name (`sliced`) and the
    // guard pattern (`if sliced {`) — together they pin
    // the match-tuple + gate structure.
    assert!(
        block.contains("sliced) = match decision"),
        "ADR-0009 § PostToolUse flow code block must destructure \
         `(content, note, bytes_out, recent_key, sliced)` from the \
         `match decision` — the `sliced: bool` binding is the gate \
         the impl uses to skip emitting a slice record on Keep.",
    );
    assert!(
        block.contains("if sliced {"),
        "ADR-0009 § PostToolUse flow code block must wrap the \
         `emit_usage(...)` call in `if sliced {{ ... }}` — the gate \
         that prevents Keep decisions from inflating \
         `plugin3 report --kind slice`. The orchestrator invariant \
         `total_bytes_saved_sums_only_sliced_offloaded` is matched \
         on the CLI side by skipping the record entirely for Keep.",
    );
    assert!(
        block.contains("emit_usage(&UsageRecord"),
        "ADR-0009 § PostToolUse flow code block must still call \
         `emit_usage(&UsageRecord {{ ... }})` — but only inside \
         the `if sliced` arm.",
    );

    // Negative: the unconditional top-level emit (no
    // gate) must NOT appear. The pre-fix form had
    // `append_recent(...); emit_usage(...)` with no
    // gate between them.
    //
    // ponytail: narrow the negative pin to the
    // `emit_usage` call directly preceded by
    // `append_recent` with no intervening gate — that's
    // the pre-fix shape. The gated emit inside
    // `if sliced { emit_usage(...) }` is preceded by
    // the ponytail comment block, not by `append_recent`,
    // so the substring scan stays unambiguous.
    assert!(
        !block.contains("append_recent(&recent_key, bytes_in);\n    emit_usage(&UsageRecord {"),
        "ADR-0009 § PostToolUse flow code block must not present \
         the pre-fix shape (unconditional `emit_usage` directly \
         after `append_recent`). The impl gates the emit on the \
         `sliced: bool` so Keep decisions don't inflate \
         `plugin3 report --kind slice`.",
    );
}

// ponytail: pin the § Implementation notes' absence of
// speculative Cursor/Aider settings file paths. The
// earlier draft claimed Cursor reads `~/.cursor/hooks.json`
// and Aider reads `.aider.conf.yml` — neither format is
// wired (the impl returns `HookConfig::default()` for
// both hosts per § Hook registry). A contributor who
// re-pastes those paths documents a host format the
// impl does not consume.
#[test]
fn adr_0009_implementation_notes_has_no_speculative_cursor_aider_paths() {
    let section = adr_0009_implementation_notes();
    assert!(
        !section.contains("~/.cursor/hooks.json"),
        "ADR-0009 § Implementation notes must not reference \
         `~/.cursor/hooks.json` — Cursor's settings format is not \
         yet wired (the impl returns HookConfig::default() for \
         Cursor per § Hook registry). Adding the path is a future \
         per-host ADR.",
    );
    assert!(
        !section.contains(".aider.conf.yml"),
        "ADR-0009 § Implementation notes must not reference \
         `.aider.conf.yml` — Aider's settings format is not yet \
         wired (the impl returns HookConfig::default() for Aider \
         per § Hook registry). Adding the path is a future \
         per-host ADR.",
    );
}
