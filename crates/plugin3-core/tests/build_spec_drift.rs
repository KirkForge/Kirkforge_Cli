//! ADR-0017 (build features + reproducible builds) and ADR-0002
//! (workspace layout) drift tests — the contracts that live outside
//! any single Rust file but silently break CI if a contributor tunes
//! them without realising.
//!
//! ponytail: one literal-substring scan per contract. No TOML parser
//! — the ADR owns the exact strings, and `contains` catches the
//! silent regressions (e.g. a contributor switches `panic = "abort"`
//! to `panic = "unwind"` to chase a panic-message that wasn't
//! reproducible; binary size budget blows up silently until CI runs
//! `size_budget.rs`).

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

// ponytail: pin the toolchain pin. ADR-0017 § Toolchain pin says the
// channel is load-bearing — a contributor who bumps `channel = "1.85.0"`
// to `channel = "stable"` silently shifts MSRV and may pick up a
// compiler that breaks the existing crate graph (e.g. edition2024
// changes). The existing crate reads
// `indexmap 2.14 -> toml_edit -> needs edition2024`, so a downgrade
// to 1.75 silently breaks the build — but only on a clean checkout
// without the existing target/. This drift test makes the bump loud.
#[test]
fn toolchain_channel_pin_is_locked() {
    let body = read(&repo_root().join("rust-toolchain.toml"));
    assert!(
        body.contains("channel = \"1.85.0\""),
        "rust-toolchain.toml must pin channel = \"1.85.0\" per ADR-0017 § Toolchain pin; \
         got:\n{body}",
    );
    // ponytail: components list is part of the contract — a contributor
    // who removes `rustfmt` to shave CI time makes local `cargo fmt`
    // impossible without a toolchain install.
    for needle in ["rustfmt", "clippy", "rust-src"] {
        assert!(
            body.contains(needle),
            "rust-toolchain.toml must include component `{needle}` per ADR-0017",
        );
    }
}

// ponytail: pin workspace resolver = "2" (ADR-0002 § Workspace
// Cargo.toml). Resolver 1 vs 2 changes feature unification and
// target-trip resolution; a contributor who drops it (or hits
// `cargo add --workspace` which leaves it alone) gets the wrong
// resolver only on a clean build, not on incremental.
#[test]
fn workspace_resolver_is_two() {
    let body = read(&repo_root().join("Cargo.toml"));
    assert!(
        body.contains("resolver = \"2\""),
        "workspace Cargo.toml must set resolver = \"2\" per ADR-0002; got:\n{body}",
    );
}

// ponytail: pin the workspace members list. The three crates are
// the entire codebase; a contributor who adds a new crate under
// `crates/` but forgets the `members` entry silently breaks CI on
// the next `cargo build --workspace`. Pin by exact substring.
#[test]
fn workspace_members_are_the_three_crates() {
    let body = read(&repo_root().join("Cargo.toml"));
    for needle in [
        "\"crates/plugin3-core\"",
        "\"crates/plugin3-hosts\"",
        "\"crates/plugin3-cli\"",
    ] {
        assert!(
            body.contains(needle),
            "workspace Cargo.toml missing member `{needle}` per ADR-0002 § Workspace Cargo.toml",
        );
    }
}

// ponytail: pin the workspace MSRV. ADR-0017 § Toolchain pin says
// the toolchain channel and the workspace MSRV must move together;
// if either drifts without the other, a contributor's local toolchain
// can be newer (silently tolerates code that breaks for downstream
// consumers on the older MSRV) or older (silently refuses to build
// a sibling crate that requires the newer floor).
#[test]
fn workspace_msrv_matches_toolchain_pin() {
    let cargo = read(&repo_root().join("Cargo.toml"));
    let toolchain = read(&repo_root().join("rust-toolchain.toml"));
    // Both must declare 1.85; the exact value is pinned in
    // toolchain_channel_pin_is_locked, this test guards the
    // *agreement* between the two files.
    let m = cargo.find("rust-version = \"").expect("MSRV present");
    let after = &cargo[m..];
    let msrv = after.split('"').nth(1).expect("MSRV value");
    let t = toolchain.find("channel = \"").expect("channel present");
    let tch = toolchain[t..].split('"').nth(1).expect("channel value");
    assert!(
        tch.starts_with(msrv),
        "workspace MSRV ({msrv}) and toolchain channel ({tch}) disagree — \
         ADR-0017 says they must move together",
    );
}

// ponytail: pin the dev profile settings. ADR-0017 § Workspace
// Cargo.toml says `incremental = true` is the contract (fast local
// builds); a contributor who flips it to false to chase a
// reproducibility bug locally silently slows every dev build by ~2x.
#[test]
fn dev_profile_is_incremental_with_full_debug() {
    let body = read(&repo_root().join("Cargo.toml"));
    assert!(
        body.contains("[profile.dev]"),
        "workspace Cargo.toml must declare [profile.dev] per ADR-0017",
    );
    for needle in ["opt-level = 0", "debug = true", "incremental = true"] {
        assert!(
            body.contains(needle),
            "[profile.dev] missing ADR-0017 setting `{needle}`",
        );
    }
}

// ponytail: pin the ci profile settings. ADR-0017 § Reproducible
// builds says `incremental = false` + `debug = "line-tables-only"`
// is the contract — a contributor who softens `debug` to `true`
// bloats CI artefacts by ~10x; a contributor who flips
// `incremental = true` silently re-introduces the byte-difference
// between consecutive CI runs that the ADR is trying to eliminate.
#[test]
fn ci_profile_is_reproducible_and_lightweight() {
    let body = read(&repo_root().join("Cargo.toml"));
    assert!(
        body.contains("[profile.ci]"),
        "workspace Cargo.toml must declare [profile.ci] per ADR-0017 § Reproducible builds",
    );
    for needle in [
        "inherits = \"dev\"",
        "debug = \"line-tables-only\"",
        "incremental = false",
    ] {
        assert!(
            body.contains(needle),
            "[profile.ci] missing ADR-0017 setting `{needle}`",
        );
    }
}

// ponytail: pin the clippy.toml discipline. ADR-0002 (and ADR-0017
// § Implementation notes) specifies a `disallowed-methods` list
// forbidding `std::panic::panic` so contributors route panics
// through `Result` and `TransformError`. The current file is
// absent (Ponytail: the discipline is enforced by code review, not
// clippy lint) — pin the *absence* as the documented state so a
// future contributor who adds the lint file without updating the
// spec surfaces here, and so the next ADR revision that does
// enable the lint has a known-good fixture to migrate to.
#[test]
fn clippy_toml_absence_is_documented_spec_state() {
    let p = repo_root().join("clippy.toml");
    let exists = p.is_file();
    // Ponytail: explicit boolean in the assertion message — a
    // contributor removing the lint file later (or adding it
    // back) sees a single source of truth rather than inferring
    // intent from a missing-or-present assertion.
    assert!(
        !exists,
        "clippy.toml now exists at {} — if the ADR has been updated to enable \
         the lint, also update this test to assert the disallowed-methods content \
         (see ADR-0002 § Implementation notes for the expected entry).",
        p.display(),
    );
}

// ponytail: pin ADR-0017 § Implementation notes against
// re-introducing a `clippy.toml` code block. The earlier
// draft showed the `disallowed-methods` TOML block in the
// prose; the MVP's prose explicitly notes the file is
// absent (per ADR-0002's same-rationale reconciliation).
// A contributor who re-pastes the lint-config code block
// back into ADR-0017 documents a file the workspace does
// not have — caught here before the drift cascades into
// the implementation.
#[test]
fn adr_0017_implementation_notes_omits_clippy_toml_block() {
    let adr = read(&repo_root().join("docs/adr/0017-build-features.md"));
    let section_start = adr
        .find("## Implementation notes")
        .expect("ADR-0017 must have an Implementation notes section");
    let section = &adr[section_start..];
    // Negative: a fenced TOML block at the start of §
    // Implementation notes that begins with `# clippy.toml`
    // (the exact form the earlier draft used) must not
    // appear. Subsequent prose paragraphs can mention
    // "the clippy.toml file is absent" — the drift test
    // scopes the negative to a fenced block opener to
    // avoid false positives.
    assert!(
        !section.contains("```toml\n# clippy.toml"),
        "ADR-0017 § Implementation notes must not show a \
         `# clippy.toml` code block — the file is absent in \
         this workspace (per the ponytail paragraph that \
         replaces the earlier draft). Adding the lint is a \
         future ADR with a CI-noise-vs-discipline rationale; \
         update ADR-0017 + ADR-0002 + the drift test together.",
    );
    // Positive: the absence-paragraph must be present so a
    // contributor who deletes the ponytail paragraph (forgetting
    // that the absence is the documented state) surfaces here.
    assert!(
        section.contains("clippy.toml")
            && (section.contains("absent") || section.contains("does not exist")),
        "ADR-0017 § Implementation notes must explicitly note \
         that `clippy.toml` is absent — the absence is the \
         documented state (mirroring ADR-0002). A contributor \
         who removes this paragraph loses the spec rationale.",
    );
}

// ponytail: pin the .cargo/config.toml aliases. ADR-0017 §
// Implementation notes specifies `bloat = "bloat --release --crates"` —
// a contributor who renames the alias to `size-audit` breaks every
// contributor muscle-memory and every README "Building from source"
// line that references `cargo bloat`.
#[test]
fn cargo_config_aliases_match_adr() {
    let body = read(&repo_root().join(".cargo").join("config.toml"));
    assert!(
        body.contains("bloat = \"bloat --release --crates\""),
        ".cargo/config.toml must define the `bloat` alias per ADR-0017 § Implementation notes; \
         got:\n{body}",
    );
}

// ponytail: pin ADR-0002 § Implementation notes `.cargo/config.toml`
// example block against phantom aliases. The earlier draft listed
// `xtask = "run --bin xtask --"` but no `xtask` binary exists in the
// workspace — the auto-generation tool was aspirational and the
// alias was dropped from `.cargo/config.toml` (the file ships only
// the `bloat` alias). A contributor who re-pastes the `xtask` alias
// into the ADR documents a binary the workspace does not have.
#[test]
fn adr_0002_cargo_config_block_has_no_phantom_xtask_alias() {
    let adr = read(&repo_root().join("docs/adr/0002-workspace.md"));
    let block_start = adr
        .find("```toml\n# .cargo/config.toml")
        .expect("ADR-0002 must have a § Implementation notes .cargo/config.toml code block");
    let block_end_rel = adr[block_start..]
        .find("```\n")
        .expect("ADR-0002 .cargo/config.toml code block must close");
    let block = &adr[block_start..block_start + block_end_rel];
    assert!(
        !block.contains("xtask"),
        "ADR-0002 § Implementation notes .cargo/config.toml example block \
         references phantom `xtask` alias — no `xtask` binary exists in \
         the workspace. The MVP's `.cargo/config.toml` ships only the \
         `bloat` alias. ADR-0017 § Implementation notes reconciles the \
         same drift; this test catches the same regression on ADR-0002's \
         copy of the example.",
    );
}

// ponytail: pin the ADR-0017 § Workspace Cargo.toml example block
// against the actual `Cargo.toml`. The example block in the ADR is
// the load-bearing spec — a contributor who copy-pastes `rayon =
// "1"` back into the ADR claims a dep the impl doesn't wire, and
// the resulting binary-size hit is invisible until CI runs
// `size_budget.rs`. The drift test asserts each `name = "version"`
// line in the ADR's example block matches a line in the real
// Cargo.toml — this catches the negative direction (phantom deps
// in the ADR) without forcing a positive direction (a dep added
// to Cargo.toml but not yet to the ADR is fine during the
// commit, since the ADR update follows).
#[test]
fn workspace_dependencies_match_impl() {
    let adr = read(&repo_root().join("docs/adr/0017-build-features.md"));
    let cargo = read(&repo_root().join("Cargo.toml"));

    // ponytail: scan only the ADR's § Workspace Cargo.toml example
    // block, not the prose. The prose may legitimately mention
    // `tracing`/`rayon`/etc. as policy (e.g. "What this forbids")
    // without those deps being wired.
    let block_start = adr
        .find("```toml\n# Cargo.toml (workspace root)")
        .expect("ADR-0017 must have a Workspace Cargo.toml code block");
    let block_end_marker = "```\n\n";
    let block_end_rel = adr[block_start..]
        .find(block_end_marker)
        .expect("ADR-0017 Workspace Cargo.toml code block must close");
    let block = &adr[block_start..block_start + block_end_rel];

    // ponytail: extract every `name = "version"` or `name = {
    // ... }` line within the example block, scoped to the
    // `[workspace.dependencies]` section only.
    let deps_start = block
        .find("[workspace.dependencies]")
        .expect("ADR example block must contain [workspace.dependencies]");
    let deps_end = block[deps_start..]
        .find("\n[")
        .unwrap_or(block.len() - deps_start);
    let deps_section = &block[deps_start..deps_start + deps_end];

    // ponytail: each dep line in the ADR's example block must
    // also appear in the actual Cargo.toml. The match is on the
    // dep *name* (`serde =`), not the version — version-string
    // pinning belongs in Cargo.toml, not the ADR.
    for line in deps_section.lines() {
        let trimmed = line.trim_start();
        // Skip blanks and comments.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Expect `name = ...` shape.
        let Some(eq) = trimmed.find(" = ") else {
            continue;
        };
        let name = trimmed[..eq].trim();
        // Skip the bare `[workspace.dependencies]` header (no `=`).
        if name.is_empty() || name.starts_with('[') {
            continue;
        }
        assert!(
            cargo.contains(&format!("{name} ")) || cargo.contains(&format!("{name}=")),
            "ADR-0017 § Workspace Cargo.toml lists dep `{name}` but the \
             actual workspace Cargo.toml does not declare it. If you are \
             wiring a new dep, update Cargo.toml AND keep the ADR's \
             example block in sync — this drift test reads both.",
        );
    }
}

// ponytail: pin the negative direction. The earlier ADR draft
// listed `tracing`, `rayon`, `rusqlite`, `parking_lot`, `fs2`,
// `uuid`, `walkdir`, `clap_complete`, `proptest`, `assert_cmd`,
// `predicates`, `tracing-subscriber`, `anyhow` — none of which
// are in the actual `Cargo.toml`. The trimmed ADR's example block
// must not re-introduce them. The scan is scoped to the §
// Workspace Cargo.toml block only, so the prose's "What this
// forbids" / "What this allows" sections can still mention
// these as policy without tripping the test.
#[test]
fn adr_0017_workspace_block_does_not_claim_phantom_deps() {
    let adr = read(&repo_root().join("docs/adr/0017-build-features.md"));
    let block_start = adr
        .find("```toml\n# Cargo.toml (workspace root)")
        .expect("ADR-0017 must have a Workspace Cargo.toml code block");
    let block_end_marker = "```\n\n";
    let block_end_rel = adr[block_start..]
        .find(block_end_marker)
        .expect("ADR-0017 Workspace Cargo.toml code block must close");
    let block = &adr[block_start..block_start + block_end_rel];

    for phantom in [
        "tracing",
        "tracing-subscriber",
        "rayon",
        "rusqlite",
        "parking_lot",
        "fs2",
        "uuid",
        "walkdir",
        "clap_complete",
        "proptest",
        "assert_cmd",
        "predicates",
        "anyhow",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0017 § Workspace Cargo.toml example block claims dep \
             `{phantom}` but the impl does not wire it. If you are adding \
             this dep, update both the ADR example block AND Cargo.toml; \
             the example block in the ADR must match the actual dep set.",
        );
    }
}

// ponytail: pin the negative direction on the § Toolchain pin
// block. The earlier ADR draft specified `channel = "1.75.0"`;
// the impl pins `1.85.0` (the transitive dep tree requires it).
// A contributor who reverts the example to `1.75.0` documents
// a toolchain that won't build the workspace — drift catches here.
#[test]
fn adr_0017_toolchain_block_uses_1_85() {
    let adr = read(&repo_root().join("docs/adr/0017-build-features.md"));
    let block_start = adr
        .find("```toml\n[toolchain]")
        .expect("ADR-0017 must have a § Toolchain pin code block");
    let block_end_rel = adr[block_start..]
        .find("```\n")
        .expect("ADR-0017 Toolchain pin code block must close");
    let block = &adr[block_start..block_start + block_end_rel];
    assert!(
        block.contains("channel = \"1.85.0\""),
        "ADR-0017 § Toolchain pin example must pin channel = \"1.85.0\" to match \
         rust-toolchain.toml. The earlier 1.75 draft does not build the workspace.",
    );
    assert!(
        !block.contains("channel = \"1.75"),
        "ADR-0017 § Toolchain pin example must not claim channel = \"1.75...\"; \
         the impl pins 1.85.0 and the transitive dep tree requires it.",
    );
}

// ponytail: pin the absence of `[features]` sections in the
// per-crate Cargo.toml files. The earlier ADR draft described
// `sqlite` features in both `plugin3-core` and `plugin3-cli`;
// neither crate has a `[features]` section today (the MVP
// builds with the lean dep set). Adding a feature is a future
// ADR; until then the absence is the spec.
#[test]
fn per_crate_cargo_tomls_have_no_features_section() {
    for crate_name in ["plugin3-core", "plugin3-cli", "plugin3-hosts"] {
        let path = repo_root()
            .join("crates")
            .join(crate_name)
            .join("Cargo.toml");
        let body = read(&path);
        assert!(
            !body.contains("[features]"),
            "crates/{crate_name}/Cargo.toml must not declare a [features] section \
             per ADR-0017 § Feature gates (none today). If you are adding an \
             optional feature, write a new ADR describing the dep + size impact, \
             then add the [features] block in this crate's Cargo.toml.",
        );
    }
}

// ADR-0002 drift coverage — the workspace-layout ADR is the
// older sibling of ADR-0017 and historically carried the same
// phantom-dep list. These tests pin the same surface on the
// ADR-0002 code blocks.

// ponytail: pin the negative direction on ADR-0002's
// § Workspace Cargo.toml example block. The earlier draft
// listed `tracing`, `rayon`, `rusqlite`, `parking_lot`,
// `clap_complete`, `proptest`, `assert_cmd`, `predicates`,
// `tracing-subscriber`, `anyhow`, `walkdir`, `uuid`, `fs2` —
// the same phantom set as ADR-0017. A contributor who
// re-pastes one back into ADR-0002 surfaces here, mirroring
// the ADR-0017 coverage so the two ADRs cannot drift apart.
#[test]
fn adr_0002_workspace_block_does_not_claim_phantom_deps() {
    let adr = read(&repo_root().join("docs/adr/0002-workspace.md"));
    let block_start = adr
        .find("```toml\n# Cargo.toml (workspace root)")
        .expect("ADR-0002 must have a Workspace Cargo.toml code block");
    let block_end_marker = "```\n\n";
    let block_end_rel = adr[block_start..]
        .find(block_end_marker)
        .expect("ADR-0002 Workspace Cargo.toml code block must close");
    let block = &adr[block_start..block_start + block_end_rel];

    for phantom in [
        "tracing",
        "tracing-subscriber",
        "rayon",
        "rusqlite",
        "parking_lot",
        "fs2",
        "uuid",
        "walkdir",
        "clap_complete",
        "proptest",
        "assert_cmd",
        "predicates",
        "anyhow",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0002 § Workspace Cargo.toml example block claims dep \
             `{phantom}` but the impl does not wire it. ADR-0002 and ADR-0017 \
             must agree on the lean dep set; update both when adding a dep.",
        );
    }
}

// ponytail: pin the positive direction. Each `name = "version"`
// line in ADR-0002's example block must also appear in the
// real Cargo.toml. The match is on the dep *name* — version
// pinning belongs in Cargo.toml. A contributor who trims the
// ADR's example without trimming the real Cargo.toml breaks
// this test; the right move is to add the new dep to the
// actual workspace Cargo.toml AND keep the ADR block in sync.
#[test]
fn adr_0002_workspace_dependencies_match_impl() {
    let adr = read(&repo_root().join("docs/adr/0002-workspace.md"));
    let cargo = read(&repo_root().join("Cargo.toml"));

    let block_start = adr
        .find("```toml\n# Cargo.toml (workspace root)")
        .expect("ADR-0002 must have a Workspace Cargo.toml code block");
    let block_end_marker = "```\n\n";
    let block_end_rel = adr[block_start..]
        .find(block_end_marker)
        .expect("ADR-0002 Workspace Cargo.toml code block must close");
    let block = &adr[block_start..block_start + block_end_rel];

    let deps_start = block
        .find("[workspace.dependencies]")
        .expect("ADR-0002 example block must contain [workspace.dependencies]");
    let deps_end = block[deps_start..]
        .find("\n[")
        .unwrap_or(block.len() - deps_start);
    let deps_section = &block[deps_start..deps_start + deps_end];

    for line in deps_section.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(eq) = trimmed.find(" = ") else {
            continue;
        };
        let name = trimmed[..eq].trim();
        if name.is_empty() || name.starts_with('[') {
            continue;
        }
        assert!(
            cargo.contains(&format!("{name} ")) || cargo.contains(&format!("{name}=")),
            "ADR-0002 § Workspace Cargo.toml lists dep `{name}` but the \
             actual workspace Cargo.toml does not declare it.",
        );
    }
}

// ponytail: pin the § Toolchain pin block in ADR-0002. The
// earlier draft said `channel = "1.75.0"`; the impl pins
// 1.85.0 (the transitive dep tree requires it). A contributor
// who reverts the example to 1.75 documents a toolchain that
// won't build the workspace.
#[test]
fn adr_0002_toolchain_block_uses_1_85() {
    let adr = read(&repo_root().join("docs/adr/0002-workspace.md"));
    let block_start = adr
        .find("```toml\n# rust-toolchain.toml")
        .expect("ADR-0002 must have a § Toolchain pin code block");
    let block_end_rel = adr[block_start..]
        .find("```\n")
        .expect("ADR-0002 Toolchain pin code block must close");
    let block = &adr[block_start..block_start + block_end_rel];
    assert!(
        block.contains("channel = \"1.85.0\""),
        "ADR-0002 § Toolchain pin example must pin channel = \"1.85.0\" to \
         match rust-toolchain.toml and the workspace MSRV.",
    );
    assert!(
        !block.contains("channel = \"1.75"),
        "ADR-0002 § Toolchain pin example must not claim channel = \"1.75...\"; \
         the impl pins 1.85.0 and the transitive dep tree requires it.",
    );
}

// ponytail: pin the § Workspace Cargo.toml example block's
// MSRV. The earlier draft said `rust-version = "1.75"`; the
// impl says `1.85`. A contributor who reverts the MSRV in
// the ADR documents a build that fails on a clean checkout.
#[test]
fn adr_0002_workspace_block_msrv_is_1_85() {
    let adr = read(&repo_root().join("docs/adr/0002-workspace.md"));
    let block_start = adr
        .find("```toml\n# Cargo.toml (workspace root)")
        .expect("ADR-0002 must have a Workspace Cargo.toml code block");
    let block_end_marker = "```\n\n";
    let block_end_rel = adr[block_start..]
        .find(block_end_marker)
        .expect("ADR-0002 Workspace Cargo.toml code block must close");
    let block = &adr[block_start..block_start + block_end_rel];
    assert!(
        block.contains("rust-version = \"1.85\""),
        "ADR-0002 § Workspace Cargo.toml example must declare rust-version = \"1.85\"; \
         the earlier 1.75 draft does not build the workspace.",
    );
    assert!(
        !block.contains("rust-version = \"1.75"),
        "ADR-0002 § Workspace Cargo.toml example must not claim rust-version = \"1.75\"; \
         the impl is 1.85 and the transitive dep tree requires it.",
    );
}

// ponytail: pin the § Crate layout tree against the actual
// filesystem. The earlier ADR listed `args.rs`, `state.rs`,
// and per-hook subfiles (`hooks/post_tool_use.rs`,
// `hooks/user_prompt_submit.rs`, `hooks/pre_compact.rs`)
// that don't exist — they're consolidated into `main.rs` and
// `hooks/mod.rs`. A contributor who re-creates those files
// without updating the ADR surfaces here; a contributor who
// re-lists them in the ADR after they were deleted surfaces
// here too.
#[test]
fn adr_0002_crate_layout_matches_actual_files() {
    use std::path::Path;
    let root = repo_root();

    // Files ADR-0002 § Crate layout claims exist. If any of
    // these are absent, the ADR is documenting a phantom split.
    let expected_files = [
        "crates/plugin3-core/src/lib.rs",
        "crates/plugin3-core/src/slicing.rs",
        "crates/plugin3-core/src/compaction.rs",
        "crates/plugin3-core/src/budget.rs",
        "crates/plugin3-core/src/detector.rs",
        "crates/plugin3-core/src/orchestrator.rs",
        "crates/plugin3-core/src/store.rs",
        "crates/plugin3-core/src/cost.rs",
        "crates/plugin3-core/src/report.rs",
        "crates/plugin3-core/src/atomic_write.rs",
        "crates/plugin3-core/src/error.rs",
        "crates/plugin3-core/src/paths.rs",
        "crates/plugin3-core/src/text.rs",
        "crates/plugin3-cli/src/main.rs",
        "crates/plugin3-cli/src/precedence.rs",
        "crates/plugin3-cli/src/exit.rs",
        "crates/plugin3-cli/src/hooks/mod.rs",
        "crates/plugin3-cli/src/commands/mod.rs",
        "crates/plugin3-cli/src/commands/budget.rs",
        "crates/plugin3-cli/src/commands/config.rs",
        "crates/plugin3-cli/src/commands/report.rs",
        "crates/plugin3-hosts/src/lib.rs",
        "crates/plugin3-hosts/src/canonical.rs",
        "crates/plugin3-hosts/src/claude_code.rs",
        "crates/plugin3-hosts/src/cursor.rs",
        "crates/plugin3-hosts/src/aider.rs",
    ];
    for rel in expected_files {
        assert!(
            root.join(rel).is_file(),
            "ADR-0002 § Crate layout lists `{rel}` but the file does not exist. \
             If you are reverting the consolidated layout (e.g. splitting \
             `hooks/mod.rs` into per-hook subfiles), also update the ADR's \
             § Crate layout tree to match the new filesystem.",
        );
    }

    // Files the earlier ADR listed that have been *removed*
    // (consolidated into main.rs / hooks/mod.rs). They must
    // not be silently re-created without an ADR update.
    let expected_absent = [
        "crates/plugin3-cli/src/args.rs",
        "crates/plugin3-cli/src/state.rs",
        "crates/plugin3-cli/src/hooks/post_tool_use.rs",
        "crates/plugin3-cli/src/hooks/user_prompt_submit.rs",
        "crates/plugin3-cli/src/hooks/pre_compact.rs",
    ];
    for rel in expected_absent {
        assert!(
            !Path::new(rel).exists() || !root.join(rel).is_file(),
            "ADR-0002 § Crate layout does not list `{rel}` but the file \
             exists on disk. If you are re-creating the per-hook subfiles or \
             splitting `args.rs` / `state.rs` out of `main.rs`, update ADR-0002 \
             § Crate layout to list the new file before committing.",
        );
    }
}

// ponytail: pin the § Implementation notes `.cargo/config.toml`
// example block's absence of a phantom `xtask` alias. The
// earlier draft specified `xtask = "run --bin xtask --"` but
// no `xtask` binary exists in the workspace — the auto-
// generation tooling was aspirational. The MVP's
// `.cargo/config.toml` ships only the `bloat` alias.
#[test]
fn adr_0017_cargo_config_block_has_no_phantom_xtask_alias() {
    let adr = read(&repo_root().join("docs/adr/0017-build-features.md"));
    let block_start = adr
        .find("```toml\n# .cargo/config.toml")
        .expect("ADR-0017 must have a § Implementation notes .cargo/config.toml code block");
    let block_end_rel = adr[block_start..]
        .find("```\n")
        .expect("ADR-0017 .cargo/config.toml code block must close");
    let block = &adr[block_start..block_start + block_end_rel];
    assert!(
        !block.contains("xtask"),
        "ADR-0017 § Implementation notes .cargo/config.toml example \
         references phantom `xtask` alias — no `xtask` binary exists \
         in the workspace. The MVP's `.cargo/config.toml` ships only \
         the `bloat` alias.",
    );
}

// ponytail: pin the § Implementation notes README "Building"
// code block's absence of SQLite references. The earlier
// draft showed comments like `# Default build (with SQLite)`
// and `# Minimum build (no SQLite)` — the MVP has no
// `sqlite` feature (ADR-0017 § Feature gates (none today))
// and the README's "Building" section uses ADR-0017
// references rather than SQLite-specific notes.
#[test]
fn adr_0017_readme_build_block_does_not_claim_sqlite() {
    let adr = read(&repo_root().join("docs/adr/0017-build-features.md"));
    let block_start = adr
        .find("```bash\n# Default build")
        .expect("ADR-0017 must have a § Implementation notes README Building code block");
    let block_end_rel = adr[block_start..]
        .find("```\n")
        .expect("ADR-0017 README Building code block must close");
    let block = &adr[block_start..block_start + block_end_rel];
    for phantom in ["with SQLite", "no SQLite", "rusqlite"] {
        assert!(
            !block.contains(phantom),
            "ADR-0017 § Implementation notes README Building code block \
             references phantom SQLite phrase `{phantom}` — the MVP has \
             no `sqlite` feature (ADR-0017 § Feature gates (none today)). \
             The README Building section uses ADR-0017 references, not \
             SQLite-specific notes.",
        );
    }
}

// ponytail: pin the absence of an auto-generated feature
// matrix in the README. The earlier draft specified a
// `| sqlite | yes (CLI only) | ... |` table auto-generated
// by `xtask/src/feature_matrix.rs`. Neither the `xtask`
// binary nor the README feature matrix exists — no crate
// ships a `[features]` section today (ADR-0017 § Feature
// gates), so there is nothing to render.
#[test]
fn adr_0017_no_phantom_feature_matrix_or_xtask_generator() {
    let adr = read(&repo_root().join("docs/adr/0017-build-features.md"));
    for phantom in ["xtask/src/feature_matrix.rs", "| `sqlite`", "| sqlite"] {
        assert!(
            !adr.contains(phantom),
            "ADR-0017 references phantom `{phantom}` — no `xtask` \
             binary exists and no crate ships a `[features]` section \
             today (ADR-0017 § Feature gates (none today)), so there \
             is no auto-generated feature matrix to render.",
        );
    }
    // ponytail: also pin the absence on the impl side — the
    // `xtask/` directory and the README feature-matrix table
    // must not exist.
    assert!(
        !repo_root().join("xtask").exists(),
        "xtask/ directory must not exist — no `xtask` binary is \
         wired. Adding one is a future ADR with a generator-fixture \
         rationale.",
    );
    let readme = read(&repo_root().join("README.md"));
    assert!(
        !readme.contains("| `sqlite`") && !readme.contains("| Feature   | Default"),
        "README must not contain a feature matrix — no crate ships \
         a `[features]` section today.",
    );
}

// ponytail: pin ADR-0016 § Implementation notes' test-file
// tree against the actual `crates/` layout. The earlier
// draft prescribed `tests/property.rs`, `tests/golden.rs`,
// `tests/golden/{head_tail_slice,detector_classify,budget_state}/`
// and `crates/plugin3-cli/tests/cli_smoke.rs` — the MVP
// inlines property tests as `mod tests` blocks in each
// source file, uses TSV fixtures under `tests/fixtures/`
// rather than `golden/`, and splits per-ADR drift tests
// across `tests/cli_design_spec_drift.rs`,
// `tests/hooks_mod_drift.rs`, etc. instead of a single
// `cli_smoke.rs`. A contributor who re-pastes the
// earlier file tree documents a layout the impl does not
// have.
#[test]
fn adr_0016_test_files_match_adr_layout() {
    let core_tests = repo_root()
        .join("crates")
        .join("plugin3-core")
        .join("tests");
    let cli_tests = repo_root().join("crates").join("plugin3-cli").join("tests");
    // Negative: phantom files must NOT exist.
    for phantom in [
        core_tests.join("property.rs"),
        core_tests.join("golden.rs"),
        core_tests.join("golden"),
    ] {
        assert!(
            !phantom.exists(),
            "{} must not exist — ADR-0016 § Implementation notes \
             pins the MVP test layout. The earlier draft prescribed \
             `tests/property.rs` (replaced by inline `mod tests` LCG \
             fixtures) and `tests/golden.rs` + `tests/golden/` \
             (replaced by `tests/fixtures/*.tsv`). Restoring them is a \
             future ADR with a proptest/golden-rationale; update the \
             ADR tree and this test together.",
            phantom.display(),
        );
    }
    assert!(
        !cli_tests.join("cli_smoke.rs").exists(),
        "crates/plugin3-cli/tests/cli_smoke.rs must not exist — the \
         CLI's end-to-end surface is exercised by the per-ADR drift \
         tests in `cli_design_spec_drift.rs`, `hooks_mod_drift.rs`, \
         and `compaction_spec_drift.rs`. A monolithic `cli_smoke.rs` \
         is a future ADR with a smoke-vs-drift rationale.",
    );
    // Positive: the tests/ files that DO exist must be present
    // so a contributor who deletes one (thinking "this is
    // covered elsewhere") surfaces here.
    for required in [
        core_tests.join("adr_xref_drift.rs"),
        core_tests.join("build_spec_drift.rs"),
        core_tests
            .join("compaction_spec_drift.rs")
            .exists()
            .then(|| core_tests.join("compaction_spec_drift.rs"))
            .unwrap_or(core_tests.join("compaction_fixtures.rs")),
        core_tests.join("fixtures"),
    ] {
        assert!(
            required.exists(),
            "{} must exist — ADR-0016 § Implementation notes pins \
             this test file as part of the drift-test surface.",
            required.display(),
        );
    }
}

// ponytail: pin ADR-0016 § Property tests' "no `proptest` dep"
// claim against the actual workspace Cargo.toml. The
// earlier draft prescribed `proptest` as a workspace dep;
// the MVP uses inline LCG fixtures instead (ADR-0017 §
// Workspace Cargo.toml). A contributor who adds `proptest =
// "1"` without updating the ADR documents a dep the binary
// doesn't need (`proptest` adds ~150 KB).
#[test]
fn adr_0016_workspace_has_no_proptest_or_assert_cmd_dep() {
    let workspace = read(&repo_root().join("Cargo.toml"));
    let core = read(
        &repo_root()
            .join("crates")
            .join("plugin3-core")
            .join("Cargo.toml"),
    );
    let cli = read(
        &repo_root()
            .join("crates")
            .join("plugin3-cli")
            .join("Cargo.toml"),
    );
    let hosts = read(
        &repo_root()
            .join("crates")
            .join("plugin3-hosts")
            .join("Cargo.toml"),
    );
    for (label, body) in [
        ("workspace", &workspace),
        ("plugin3-core", &core),
        ("plugin3-cli", &cli),
        ("plugin3-hosts", &hosts),
    ] {
        for phantom in ["proptest", "assert_cmd", "predicate"] {
            assert!(
                !body.contains(phantom),
                "[{label}] Cargo.toml references phantom dep `{phantom}` — \
                 ADR-0016 § Property tests and § Implementation notes \
                 pin the LCG-fixture + `std::process::Command` pattern \
                 instead. Adding `{phantom}` is a future ADR with a \
                 fixture-vs-proptest rationale; update ADR-0016 and \
                 ADR-0017 together.",
            );
        }
    }
}

// ponytail: pin ADR-0011 (Persistent knowledge) Deferred status.
// ADR-0011 is prose-only — no code lands in the MVP. A contributor
// who pastes the `Finding` struct or creates a `.plugin3/knowledge/`
// directory without promoting ADR-0011 to Accepted documents a
// feature the impl does not ship. The drift tests below pin: (a) no
// `knowledge/` dir at workspace root, (b) no `findings.jsonl` file,
// (c) no `Finding` / `FindingKind` symbols in source, (d) the ADR
// itself still reads `Deferred`, (e) the README "2 Deferred (0011,
// 0012)" line still names both ADRs.
#[test]
fn adr_0011_deferred_status_persists() {
    let adr = read(
        &repo_root()
            .join("docs")
            .join("adr")
            .join("0011-persistent-knowledge.md"),
    );
    assert!(
        adr.contains("- **Status:** Deferred"),
        "ADR-0011 header drifted away from Deferred — a contributor \
         promoted the design to Accepted but did not remove the ADR's \
         prose-only Deferred framing. Update § Implementation notes \
         first; the prose is the spec until then."
    );
    assert!(
        adr.contains("No code lands in the MVP"),
        "ADR-0011 § Implementation notes drifted — the deferred-framing \
         paragraph is load-bearing for downstream contributors.",
    );
}

#[test]
fn adr_0011_no_knowledge_directory_or_findings_file() {
    let root = repo_root();
    assert!(
        !root.join("knowledge").exists(),
        "workspace `knowledge/` directory was created — ADR-0011 is \
         Deferred. Promoting the design requires (1) flipping status \
         to Accepted, (2) writing the extraction pipeline ADR, and \
         (3) adding the `knowledge` feature gate to plugin3-core. \
         Removing the directory without updating ADR-0011 first is \
         a silent reversal.",
    );
    assert!(
        !root.join(".plugin3").exists(),
        "workspace `.plugin3/` directory was created — ADR-0011 § \
         Implementation notes: \"The `.plugin3/knowledge/` directory \
         is *not* created by the MVP's `init` flow.\" The MVP's \
         `init` must not create `.plugin3/knowledge/`.",
    );
    assert!(
        !root
            .join("crates")
            .join("plugin3-core")
            .join("src")
            .join("knowledge.rs")
            .exists(),
        "crates/plugin3-core/src/knowledge.rs exists — ADR-0011 is \
         Deferred; no Finding/FindingKind struct ships today.",
    );
}

#[test]
fn adr_0011_no_finding_or_findingkind_in_source() {
    let root = repo_root();
    let core_src = root.join("crates").join("plugin3-core").join("src");
    let cli_src = root.join("crates").join("plugin3-cli").join("src");
    let hosts_src = root.join("crates").join("plugin3-hosts").join("src");
    for dir in [&core_src, &cli_src, &hosts_src] {
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                    let body = read(&p);
                    for phantom in ["FindingKind", "pub struct Finding", "findings.jsonl"] {
                        assert!(
                            !body.contains(phantom),
                            "{} references `{phantom}` — ADR-0011 is \
                             Deferred; the `Finding` schema is prose-only. \
                             Promoting the design adds the extraction \
                             pipeline (a separate ADR) and flips 0011 \
                             to Accepted.",
                            p.display(),
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn adr_0011_readme_documents_two_deferred_adrs() {
    let readme = read(&repo_root().join("README.md"));
    assert!(
        readme.contains("2 Deferred (0011, 0012)"),
        "README State table drifted away from \"2 Deferred (0011, 0012)\" \
         — either an ADR was promoted to Accepted (update README to \
         \"1 Deferred\") or a third ADR was deferred (update both README \
         and ADR-0011 to reflect the new count)."
    );
}

// ponytail: pin ADR-0012 (Speculative priming) Deferred status.
// ADR-0012 is prose-only — the `PreWarmCache` struct is not
// implemented, no prediction pipeline lands in the MVP. A
// contributor who pastes the cache struct or creates a
// `priming/` directory without promoting ADR-0012 to Accepted
// documents a feature the impl does not ship. The drift tests
// below pin: (a) no `priming/` dir, (b) no `PreWarmCache`
// symbol in source, (c) ADR-0012 status remains Deferred,
// (d) the README "2 Deferred (0011, 0012)" line still names 0012.
#[test]
fn adr_0012_deferred_status_persists() {
    let adr = read(
        &repo_root()
            .join("docs")
            .join("adr")
            .join("0012-speculative-priming.md"),
    );
    assert!(
        adr.contains("- **Status:** Deferred"),
        "ADR-0012 header drifted away from Deferred — a contributor \
         promoted the design to Accepted but did not remove the ADR's \
         prose-only Deferred framing. Update § Implementation notes \
         first; the prose is the spec until then."
    );
    assert!(
        adr.contains("No code lands in the MVP"),
        "ADR-0012 § Implementation notes drifted — the deferred-framing \
         paragraph is load-bearing for downstream contributors.",
    );
}

#[test]
fn adr_0012_no_priming_directory() {
    let root = repo_root();
    assert!(
        !root.join("priming").exists(),
        "workspace `priming/` directory was created — ADR-0012 is \
         Deferred. Promoting the design requires (1) flipping status \
         to Accepted, (2) picking a prediction source (Markov / \
         embedding / LLM — separate ADR), and (3) adding the \
         `speculative` feature gate. Removing the directory without \
         updating ADR-0012 first is a silent reversal.",
    );
    assert!(
        !root
            .join("crates")
            .join("plugin3-core")
            .join("src")
            .join("priming.rs")
            .exists(),
        "crates/plugin3-core/src/priming.rs exists — ADR-0012 is \
         Deferred; no PreWarmCache/PrimingHint struct ships today.",
    );
}

#[test]
fn adr_0012_no_prewarm_cache_or_priming_hint_in_source() {
    let root = repo_root();
    let core_src = root.join("crates").join("plugin3-core").join("src");
    let cli_src = root.join("crates").join("plugin3-cli").join("src");
    let hosts_src = root.join("crates").join("plugin3-hosts").join("src");
    for dir in [&core_src, &cli_src, &hosts_src] {
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                    let body = read(&p);
                    for phantom in [
                        "PreWarmCache",
                        "PrimingHint",
                        "prewarm_cache",
                        "priming_state",
                    ] {
                        assert!(
                            !body.contains(phantom),
                            "{} references `{phantom}` — ADR-0012 is \
                             Deferred; the pre-warm cache is prose-only. \
                             Promoting the design adds the prediction \
                             pipeline (a separate ADR) and flips 0012 \
                             to Accepted.",
                            p.display(),
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn adr_0012_no_speculative_feature_gate_in_cargo() {
    let root = repo_root();
    let core = read(&root.join("crates").join("plugin3-core").join("Cargo.toml"));
    let cli = read(&root.join("crates").join("plugin3-cli").join("Cargo.toml"));
    let hosts = read(&root.join("crates").join("plugin3-hosts").join("Cargo.toml"));
    let workspace = read(&root.join("Cargo.toml"));
    for (label, body) in [
        ("workspace", &workspace),
        ("plugin3-core", &core),
        ("plugin3-cli", &cli),
        ("plugin3-hosts", &hosts),
    ] {
        // Negative-pin: no `speculative` feature gate anywhere.
        // ADR-0012 § Implementation notes says "Add a `speculative`
        // feature gate" — that addition IS the promotion signal.
        // A contributor who adds `[features] speculative = []`
        // without flipping the ADR to Accepted documents a
        // gate the impl does not use.
        for phantom in ["[features]", "speculative"] {
            assert!(
                !body.contains(phantom),
                "[{label}] Cargo.toml references phantom `{phantom}` — \
                 ADR-0017 § Feature gates says \"The MVP has no \
                 [features] section in any Cargo.toml\" and ADR-0012 \
                 is Deferred. Adding the gate is the promotion signal; \
                 flip ADR-0012 to Accepted first.",
            );
        }
    }
}

// ponytail: pin the workspace `directories` dep. ADR-0017
// § Workspace Cargo.toml lists `directories = "5"` as a load-bearing
// workspace dep — it powers the XDG path fallback inside
// `Paths::resolve()` (ADR-0014 § Path resolver). A contributor who
// drops the dep (e.g. to chase a 0.1 MB release binary shrink)
// silently breaks XDG path resolution on Linux/macOS: every
// `Paths::resolve()` call falls back to the `PLUGIN3_DATA_DIR`
// branch only, and a contributor who never sets the env var (the
// default case) gets a panic instead of a path. This is also the
// drift guard for ADR-0010 § File location, which used to claim
// "the workspace does not depend on the `directories` crate" —
// that prose was wrong (false negative on ADR-0017 + paths.rs +
// Cargo.toml), and this test pins the *positive* contract so the
// false claim cannot reappear in the ADR without breaking CI.
#[test]
fn workspace_dependencies_wire_directories_crate() {
    let body = read(&repo_root().join("Cargo.toml"));
    assert!(
        body.contains("directories = \"5\""),
        "workspace Cargo.toml must declare `directories = \"5\"` per \
         ADR-0017 § Workspace Cargo.toml — the crate powers the XDG \
         path fallback inside `Paths::resolve()` (ADR-0014). Dropping \
         it silently breaks every default-config Linux/macOS user \
         whose `PLUGIN3_DATA_DIR` is unset. Got:\n{body}",
    );
}
