//! ADR-0015 (CLI design) drift tests — the contracts that
//! live in the ADR prose and must stay in lockstep with the
//! `plugin3-cli/src/main.rs` clap impl, the `Command` enum,
//! the `precedence::resolve_config_path` chain, and the
//! `exit::exit_{config,usage}_err` helpers. Companion to the
//! in-file tests inside `main.rs` (which pin impl-side
//! behaviour via subprocess); this file pins the *spec
//! surface* — the § Top-level structure, § Subcommand
//! shapes, § Precedence chain, and § Implementation notes.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who re-pastes the
//! phantom `verbose`/`quiet`/`config_dir`/`Version` clap
//! shape documents a clap surface the binary does not have).

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

fn adr_0015() -> String {
    read(&repo_root().join("docs/adr/0015-cli-design.md"))
}

/// Read ADR-0015's § Top-level structure code block.
fn adr_0015_top_level_block() -> String {
    let adr = adr_0015();
    let section_start = adr
        .find("### Top-level structure")
        .expect("ADR-0015 must have a § Top-level structure subsection");
    let section_end = adr[section_start..]
        .find("### Subcommand shapes")
        .expect("ADR-0015 § Top-level structure must precede § Subcommand shapes");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0015 § Top-level structure must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0015 § Top-level structure rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0015's § Subcommand shapes code block.
fn adr_0015_subcommand_shapes_block() -> String {
    let adr = adr_0015();
    let section_start = adr
        .find("### Subcommand shapes")
        .expect("ADR-0015 must have a § Subcommand shapes subsection");
    let section_end = adr[section_start..]
        .find("### Precedence chain")
        .expect("ADR-0015 § Subcommand shapes must precede § Precedence chain");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0015 § Subcommand shapes must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0015 § Subcommand shapes rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0015's § Precedence chain code block.
fn adr_0015_precedence_block() -> String {
    let adr = adr_0015();
    let section_start = adr
        .find("### Precedence chain")
        .expect("ADR-0015 must have a § Precedence chain subsection");
    let section_end = adr[section_start..]
        .find("### `--json` for every subcommand")
        .expect("ADR-0015 § Precedence chain must precede § `--json`");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0015 § Precedence chain must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0015 § Precedence chain rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0015's § Implementation notes.
fn adr_0015_implementation_notes() -> String {
    let adr = adr_0015();
    let section_start = adr
        .find("## Implementation notes")
        .expect("ADR-0015 must have an Implementation notes section");
    adr[section_start..].to_string()
}

// ---- § Top-level structure: positive tests ----

// ponytail: pin the § Top-level structure example's
// `Command` enum to the five-variant shape the impl ships.
// The MVP declares `Hook { kind: HookKind }`, `Budget(BudgetCmd)`,
// `Report { ... }`, `SelfCheck`, `Config { ... }`. A contributor
// who re-pastes an older four-variant form (no `SelfCheck`,
// `Hook(HookArgs)`, `Config(ConfigArgs)`, `Version`)
// documents a clap surface the binary does not have.
#[test]
fn adr_0015_top_level_block_lists_five_command_variants() {
    let block = adr_0015_top_level_block();
    for v in ["Hook", "Budget", "Report", "SelfCheck", "Config"] {
        assert!(
            block.contains(v),
            "ADR-0015 § Top-level structure example must \
             declare Command variant `{v}` — the impl's \
             `Command` enum has five variants.",
        );
    }
}

// ponytail: pin the § Top-level structure example's
// `json: bool` global clap flag. The MVP ships only
// `json: bool` as a global flag. A contributor who removes
// the `#[arg(long, global = true)]` annotation would
// surface here.
#[test]
fn adr_0015_top_level_block_pins_json_global_flag() {
    let block = adr_0015_top_level_block();
    assert!(
        block.contains("pub json: bool") && block.contains("global = true"),
        "ADR-0015 § Top-level structure example must show \
         `pub json: bool` with `#[arg(long, global = true)]` \
         — the impl's only global clap flag. A contributor who \
         drops `global = true` breaks `--json` propagation to \
         subcommands.",
    );
}

// ponytail: pin the § Top-level structure example's
// source-file path comment. The impl's `Cli` and `Command`
// live in `crates/plugin3-cli/src/main.rs`, not in a
// separate `args.rs` module. The drift test
// `adr_0015_implementation_notes_no_args_rs_path`
// below pins the § Implementation notes path; this one
// pins the code-block path comment.
#[test]
fn adr_0015_top_level_block_points_to_main_rs() {
    let block = adr_0015_top_level_block();
    assert!(
        block.contains("crates/plugin3-cli/src/main.rs"),
        "ADR-0015 § Top-level structure example must \
         reference `crates/plugin3-cli/src/main.rs` — the \
         MVP's `Cli` and `Command` enum live in main.rs, not \
         in a phantom `args.rs` module.",
    );
}

// ---- § Top-level structure: negative tests ----

// ponytail: pin the absence of a `Version` unit variant
// in the § Top-level structure example. clap's built-in
// `--version` flag handles `plugin3 --version`; a custom
// `Version` variant would shadow it.
#[test]
fn adr_0015_top_level_block_has_no_version_variant() {
    let block = adr_0015_top_level_block();
    assert!(
        !block.contains("Version"),
        "ADR-0015 § Top-level structure example must not \
         declare a `Version` Command variant — clap's \
         built-in `--version` flag handles it. A contributor \
         who re-adds `Version` would shadow clap's flag.",
    );
}

// ponytail: pin the absence of phantom clap global flags
// (`verbose`, `quiet`, `config_dir`, `data_dir`,
// `runtime_dir`, `config`). The MVP's `Cli` struct carries
// only `json: bool` + `command: Command`; verbose/quiet are
// unused and the three `PLUGIN3_*_DIR` env vars are read
// directly by `Paths::resolve()` rather than threaded
// through clap.
#[test]
fn adr_0015_top_level_block_has_no_phantom_clap_flags() {
    let block = adr_0015_top_level_block();
    for phantom in [
        "verbose",
        "quiet",
        "config_dir",
        "data_dir",
        "runtime_dir",
        "pub config:",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0015 § Top-level structure example references \
             phantom clap flag `{phantom}` — the impl's `Cli` \
             struct carries only `json: bool` + `command: Command`. \
             `Paths::resolve()` reads the `PLUGIN3_*_DIR` env vars \
             directly without clap indirection.",
        );
    }
}

// ponytail: pin the absence of phantom struct wrappers
// (`HookArgs`, `BudgetArgs`, `ReportArgs`, `ConfigArgs`).
// The MVP inlines `Report { ... }` and `Config { ... }`
// directly into the `Command` enum as struct variants;
// `Hook { kind: HookKind }` takes the kind directly.
#[test]
fn adr_0015_top_level_block_has_no_phantom_struct_wrappers() {
    let block = adr_0015_top_level_block();
    for phantom in ["HookArgs", "BudgetArgs", "ReportArgs", "ConfigArgs"] {
        assert!(
            !block.contains(phantom),
            "ADR-0015 § Top-level structure example references \
             phantom struct wrapper `{phantom}` — the impl \
             inlines the variant fields directly into `Command` \
             (and uses `HookKind` directly for the hook kind).",
        );
    }
}

// ponytail: pin the absence of `tracing::*` in the
// § Top-level structure example. The MVP does not depend
// on `tracing` (ADR-0017 § Workspace Cargo.toml) and the
// clap module emits zero tracing events today.
#[test]
fn adr_0015_top_level_block_does_not_claim_tracing() {
    let block = adr_0015_top_level_block();
    for phantom in [
        "tracing::warn",
        "tracing::info",
        "tracing::error",
        "tracing::debug",
        "use tracing",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0015 § Top-level structure example claims \
             `{phantom}` but the workspace does not depend on \
             `tracing`. The clap module emits zero tracing events.",
        );
    }
}

// ---- § Subcommand shapes: positive tests ----

// ponytail: pin the § Subcommand shapes example's
// `HookKind` enum. The MVP uses `HookKind` directly
// (not a phantom `HookSubcommand` enum wrapped in
// `HookArgs { subcommand }`). Three variants:
// `PostToolUse`, `UserPromptSubmit`, `PreCompact`.
#[test]
fn adr_0015_subcommand_shapes_block_names_hookkind_enum() {
    let block = adr_0015_subcommand_shapes_block();
    assert!(
        block.contains("pub enum HookKind"),
        "ADR-0015 § Subcommand shapes example must declare \
         `pub enum HookKind` — the impl's clap value enum. \
         A contributor who re-pastes `pub enum HookSubcommand` \
         wrapped in `pub struct HookArgs` documents a clap \
         shape the impl does not have.",
    );
    for v in ["PostToolUse", "UserPromptSubmit", "PreCompact"] {
        assert!(
            block.contains(v),
            "ADR-0015 § Subcommand shapes example must \
             declare HookKind variant `{v}` — the impl's \
             three-variant enum.",
        );
    }
}

// ponytail: pin the § Subcommand shapes example's
// `BudgetCmd` struct wrapping a `BudgetSub` enum (the
// clap subcommand pattern). The same shape is pinned by
// `compaction_spec_drift.rs` against ADR-0008; this
// second pin keeps ADR-0015's prose in lockstep with
// ADR-0008's.
#[test]
fn adr_0015_subcommand_shapes_block_uses_struct_wrapping_enum() {
    let block = adr_0015_subcommand_shapes_block();
    assert!(
        block.contains("pub struct BudgetCmd"),
        "ADR-0015 § Subcommand shapes example must declare \
         `pub struct BudgetCmd` — the impl wraps a subcommand \
         enum in a struct so clap sees the nested subcommand.",
    );
    assert!(
        block.contains("pub enum BudgetSub"),
        "ADR-0015 § Subcommand shapes example must declare \
         `pub enum BudgetSub` — the actual three-variant \
         enum the clap subcommand dispatches on.",
    );
    for v in ["Status", "Set", "Compact"] {
        assert!(
            block.contains(v),
            "ADR-0015 § Subcommand shapes example must \
             declare BudgetSub variant `{v}` — the impl's \
             three-variant enum.",
        );
    }
}

// ponytail: pin the § Subcommand shapes example's
// `UsageKindArg` enum. The MVP's six-variant enum
// filters `plugin3 report --kind`. The drift test
// `report_kind_filter_selects_matching_lines` (in
// main.rs) pins the impl behaviour; this pins the ADR
// spec surface.
#[test]
fn adr_0015_subcommand_shapes_block_names_usage_kind_arg() {
    let block = adr_0015_subcommand_shapes_block();
    assert!(
        block.contains("pub enum UsageKindArg"),
        "ADR-0015 § Subcommand shapes example must declare \
         `pub enum UsageKindArg` — the impl's clap value enum.",
    );
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
            "ADR-0015 § Subcommand shapes example must \
             declare UsageKindArg variant `{v}` — the impl's \
             six-variant enum.",
        );
    }
}

// ---- § Subcommand shapes: negative tests ----

// ponytail: pin the absence of a phantom `ConfigArgs`
// enum in the § Subcommand shapes example. The MVP uses
// a `Config { show_sources: bool, validate: bool }` flag-
// based struct variant rather than a `ConfigArgs { Show,
// ShowSources, Validate }` subcommand enum. Two bool
// flags read cleaner than three unit variants in clap's
// help output.
#[test]
fn adr_0015_subcommand_shapes_block_has_no_configargs_enum() {
    let block = adr_0015_subcommand_shapes_block();
    assert!(
        !block.contains("ConfigArgs"),
        "ADR-0015 § Subcommand shapes example must not \
         reference `ConfigArgs` — the impl uses a \
         `Config {{ show_sources: bool, validate: bool }}` \
         flag-based struct variant rather than a \
         subcommand enum.",
    );
}

// ponytail: pin the absence of a phantom `HookSubcommand`
// enum in the § Subcommand shapes example. The MVP uses
// `HookKind` directly as a clap `value_enum` arg of `Hook`;
// no `HookArgs` wrapper struct and no `HookSubcommand` enum.
#[test]
fn adr_0015_subcommand_shapes_block_has_no_hooksubcommand_enum() {
    let block = adr_0015_subcommand_shapes_block();
    assert!(
        !block.contains("HookSubcommand"),
        "ADR-0015 § Subcommand shapes example must not \
         reference `HookSubcommand` — the impl uses \
         `HookKind` directly without a wrapper struct.",
    );
    assert!(
        !block.contains("HookArgs"),
        "ADR-0015 § Subcommand shapes example must not \
         reference `HookArgs` — the impl takes the kind \
         directly via `Hook {{ kind: HookKind }}`.",
    );
}

// ---- § Precedence chain: positive + negative tests ----

// ponytail: pin the § Precedence chain example's function
// signature. The MVP declares `pub(crate) fn
// resolve_config_path(cli_config: Option<&Path>, env: &dyn
// EnvSource, xdg: &Path) -> PathBuf` — three parameters
// rather than the `(cli: &Cli, env, xdg)` shape the
// earlier draft specified.
#[test]
fn adr_0015_precedence_block_signature_matches_impl() {
    let block = adr_0015_precedence_block();
    assert!(
        block.contains("pub(crate) fn resolve_config_path"),
        "ADR-0015 § Precedence chain example must declare \
         `pub(crate) fn resolve_config_path` — the impl's \
         actual visibility (private to the crate; the public \
         entry point is `commands::config::show`).",
    );
    assert!(
        block.contains("cli_config: Option<&std::path::Path>"),
        "ADR-0015 § Precedence chain example must declare \
         `cli_config: Option<&std::path::Path>` — the impl's \
         first parameter. A contributor who re-pastes the \
         `cli: &Cli` shape documents a function that takes \
         the now-phantom `Cli` struct.",
    );
    assert!(
        block.contains("env: &dyn EnvSource"),
        "ADR-0015 § Precedence chain example must declare \
         `env: &dyn EnvSource` — the impl's `EnvSource` \
         trait parameter that keeps tests hermetic.",
    );
}

// ponytail: pin the § Precedence chain example's body
// shape. The MVP checks `if let Some(p) = cli_config`
// first, then `env.get("PLUGIN3_CONFIG")`, then
// `xdg.join("config.toml")` — the same three-step chain
// the impl ships, with `env > XDG` only reached when CLI
// is `None`.
#[test]
fn adr_0015_precedence_block_body_uses_three_step_chain() {
    let block = adr_0015_precedence_block();
    assert!(
        block.contains("if let Some(p) = cli_config"),
        "ADR-0015 § Precedence chain example must show \
         `if let Some(p) = cli_config` first — matches \
         the impl's first arm.",
    );
    assert!(
        block.contains("env.get(\"PLUGIN3_CONFIG\")"),
        "ADR-0015 § Precedence chain example must show \
         `env.get(\"PLUGIN3_CONFIG\")` as the second arm — \
         matches the impl's env probe.",
    );
    assert!(
        block.contains("xdg.join(\"config.toml\")"),
        "ADR-0015 § Precedence chain example must show \
         `xdg.join(\"config.toml\")` as the XDG fallback — \
         matches the impl's default.",
    );
}

// ponytail: pin the § Precedence chain example's
// absence of a phantom `cli: &Cli` parameter. The impl's
// signature takes `cli_config: Option<&Path>` directly;
// a `cli: &Cli` shape would re-introduce the dependency
// on the `Cli` struct that was simplified away.
#[test]
fn adr_0015_precedence_block_has_no_cli_param() {
    let block = adr_0015_precedence_block();
    assert!(
        !block.contains("cli: &Cli"),
        "ADR-0015 § Precedence chain example must not \
         declare `cli: &Cli` — the impl takes the resolved \
         path directly via `cli_config: Option<&Path>`. A \
         `cli: &Cli` parameter would re-couple the function \
         to the now-minimal `Cli` struct.",
    );
}

// ---- § Implementation notes: clap features + path tests ----

// ponytail: pin the § Implementation notes clap features
// list. The MVP's `Cargo.toml` enables only `derive` and
// `env` for clap. The earlier draft specified `derive`,
// `env`, `string`, and `unicode` — the latter two are
// phantom features the workspace does not enable.
#[test]
fn adr_0015_implementation_notes_clap_features_minimal() {
    let section = adr_0015_implementation_notes();
    assert!(
        section.contains("features = [\"derive\", \"env\"]"),
        "ADR-0015 § Implementation notes must show \
         `features = [\"derive\", \"env\"]` — the MVP's \
         clap feature set per `Cargo.toml`. The earlier \
         draft's `string` and `unicode` features are \
         unused.",
    );
    // Negative: phantom feature flags must NOT appear.
    for phantom in ["\"string\"", "\"unicode\""] {
        assert!(
            !section.contains(phantom),
            "ADR-0015 § Implementation notes references \
             phantom clap feature `{phantom}` — the workspace \
             only enables `derive` and `env`. The `string` \
             and `unicode` features are clap defaults that \
             don't need explicit enabling.",
        );
    }
}

// ponytail: pin the impl-side CLI directory layout against
// the ADR-0015 § Implementation notes tree. The earlier
// draft prescribed per-hook files (`hooks/post_tool_use.rs`,
// `hooks/user_prompt_submit.rs`, `hooks/pre_compact.rs`)
// plus `args.rs` and `config_loader.rs` — all of which
// were folded into `hooks/mod.rs` / `main.rs` until a
// contributor finds a reason to split. A contributor who
// re-pastes the per-hook file tree into the ADR documents
// a layout the impl does not have.
#[test]
fn adr_0015_impl_directory_layout_matches_adr() {
    let cli_root = repo_root().join("crates").join("plugin3-cli").join("src");
    // Negative: per-hook files must NOT exist.
    for phantom in [
        "post_tool_use.rs",
        "user_prompt_submit.rs",
        "pre_compact.rs",
    ] {
        let p = cli_root.join("hooks").join(phantom);
        assert!(
            !p.exists(),
            "{} must not exist — all three hook handlers live in `hooks/mod.rs`. \
             Splitting hooks into per-handler files is a future ADR with a \
             line-count or test-surface rationale; update both the ADR tree \
             and this test together.",
            p.display(),
        );
    }
    // Negative: `args.rs` and `config_loader.rs` must NOT exist.
    for phantom in ["args.rs", "config_loader.rs"] {
        let p = cli_root.join(phantom);
        assert!(
            !p.exists(),
            "{} must not exist — the clap `Cli` / `Command` types are \
             defined in `main.rs`. Splitting into `args.rs` or \
             `config_loader.rs` is a future ADR with a line-count \
             rationale; update both the ADR tree and this test together.",
            p.display(),
        );
    }
    // Positive: the files that DO exist must be present so a
    // contributor who deletes one of them (thinking "it's
    // boilerplate, fold it into main.rs") surfaces here.
    for required in [
        "main.rs",
        "precedence.rs",
        "exit.rs",
        "hooks/mod.rs",
        "commands/mod.rs",
        "commands/budget.rs",
        "commands/report.rs",
        "commands/config.rs",
    ] {
        let p = cli_root.join(required);
        assert!(
            p.exists(),
            "{} must exist — ADR-0015 § Implementation notes pins this \
             file as part of the CLI layout. Deleting it is a future \
             ADR with a fold-rationale; update both the ADR tree and \
             this test together.",
            p.display(),
        );
    }
}
