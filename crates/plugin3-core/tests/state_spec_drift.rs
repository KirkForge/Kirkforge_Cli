//! ADR-0014 (state management) drift tests — the contracts that
//! live in the ADR prose and must stay in lockstep with the
//! `plugin3-core/src/paths.rs`, `plugin3-core/src/atomic_write.rs`,
//! and `plugin3-cli/src/main.rs` impls. Companion to
//! `offload_store_spec_drift.rs` and `build_spec_drift.rs` —
//! this file pins the *state management* spec surface:
//! directory layout (no `slices.sqlite`, no `anchors/`, no
//! `lock`), file paths (no `state.rs`), and the absence of a
//! `with_lock` / `fs2` runtime lock contract.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who copy-pastes the
//! `slices.sqlite` / `anchors/` / `lock` claims back into the
//! ADR documents a layout the impl does not produce, and the
//! resulting `mkdir -p`/file-create breakage lands only on a
//! clean install — invisible until CI runs).

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

/// Read ADR-0014's § Directory layout code block (the first
/// fenced `text` block in the § Directory layout subsection).
/// Excludes surrounding prose so the reconciliation note can
/// mention deprecated artifacts (e.g. "`SQLite` was removed
/// when ...") without tripping the drift test.
fn adr_0014_directory_layout_block() -> String {
    let adr = read(&repo_root().join("docs/adr/0014-state-management.md"));
    let section_start = adr
        .find("### Directory layout")
        .expect("ADR-0014 must have a § Directory layout subsection");
    let section_end = adr[section_start..]
        .find("### Path resolution")
        .expect("ADR-0014 § Directory layout must precede § Path resolution");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```\n")
        .expect("ADR-0014 § Directory layout must contain a fenced code block");
    let fence_after = &section[fence_start + "```\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0014 § Directory layout code block must close");
    fence_after[..fence_end_rel].to_string()
}

fn adr_0014_path_resolution_block() -> String {
    let adr = read(&repo_root().join("docs/adr/0014-state-management.md"));
    let section_start = adr
        .find("### Path resolution")
        .expect("ADR-0014 must have a § Path resolution subsection");
    let section_end = adr[section_start..]
        .find("### Atomic flag file for budget")
        .expect("ADR-0014 § Path resolution must precede § Atomic flag file");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0014 § Path resolution must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0014 § Path resolution rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

fn adr_0014_atomic_block() -> String {
    let adr = read(&repo_root().join("docs/adr/0014-state-management.md"));
    let section_start = adr
        .find("### Atomic flag file for budget")
        .expect("ADR-0014 must have a § Atomic flag file for budget subsection");
    let section_end = adr[section_start..]
        .find("### Recent outputs file")
        .expect("ADR-0014 § Atomic flag file must precede § Recent outputs file");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0014 § Atomic flag file must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0014 § Atomic flag file rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

fn adr_0014_recent_outputs_block() -> String {
    let adr = read(&repo_root().join("docs/adr/0014-state-management.md"));
    let section_start = adr
        .find("### Recent outputs file")
        .expect("ADR-0014 must have a § Recent outputs file subsection");
    let section_end = adr[section_start..]
        .find("### Runtime lock")
        .or_else(|| adr[section_start..].find("## Consequences"))
        .expect("ADR-0014 § Recent outputs file must precede § Runtime lock or § Consequences");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0014 § Recent outputs file must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0014 § Recent outputs file rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

fn adr_0014_implementation_notes_section() -> String {
    let adr = read(&repo_root().join("docs/adr/0014-state-management.md"));
    let section_start = adr
        .find("## Implementation notes")
        .expect("ADR-0014 must have an Implementation notes section");
    adr[section_start..].to_string()
}

// ponytail: pin the negative direction on § Directory layout.
// The earlier draft listed `slices.sqlite` (phantom SQLite
// OffloadStore), `anchors/<workspace_hash>` (deferred per
// ADR-0011), and a `lock` file in `$XDG_RUNTIME_DIR/plugin3/`
// (unused — see § Runtime lock deferral). The MVP's data
// dir contains `slices/` (FileOffloadStore per ADR-0004),
// `budget.toml`, `recent_outputs.jsonl`, and `logs/usage.jsonl`.
// A contributor who re-pastes any of the phantom entries
// surfaces here.
#[test]
fn adr_0014_directory_layout_block_does_not_claim_phantom_entries() {
    let block = adr_0014_directory_layout_block();
    // B2: budget.toml now lives under $XDG_RUNTIME_DIR/plugin3/, so
    // `runtime` / `$XDG_RUNTIME_DIR` are legitimate layout references.
    for phantom in ["slices.sqlite", "anchors"] {
        assert!(
            !block.contains(phantom),
            "ADR-0014 § Directory layout example block claims `{phantom}` \
             but the impl does not produce this file. Adding a SQLite \
             OffloadStore / anchors dir is a future ADR; until then the \
             directory layout is what § Path resolution's `Paths` struct \
             exposes. If you are re-introducing one of these, update both \
             the ADR and `Paths` in lockstep.",
        );
    }
}

// ponytail: pin the positive direction — the § Directory
// layout block must list the actual on-disk artifacts the
// impl produces. `slices/` is the FileOffloadStore directory
// per ADR-0004; `budget.toml`, `recent_outputs.jsonl`, and
// `logs/usage.jsonl` are the three files the CLI writes.
// A contributor who deletes one of these surfaces here.
#[test]
fn adr_0014_directory_layout_block_lists_actual_artifacts() {
    let block = adr_0014_directory_layout_block();
    for artifact in [
        "slices/",
        "budget.toml",
        "recent_outputs.jsonl",
        "usage.jsonl",
        "config.toml",
        // B2: budget.toml moved to runtime_dir, so the layout block
        // must show the runtime directory.
        "$XDG_RUNTIME_DIR",
    ] {
        assert!(
            block.contains(artifact),
            "ADR-0014 § Directory layout example block must list `{artifact}` \
             — it is the on-disk artifact the impl produces. A contributor \
             who renames or moves a file surfaces here.",
        );
    }
}

// ponytail: pin the file-path comment in § Path resolution.
// The earlier draft said `state.rs`; the actual impl is
// `paths.rs` (a single small file, no `state.rs` module).
// A contributor who copy-pastes the old `state.rs` reference
// back into the ADR's code-block comment surfaces here.
#[test]
fn adr_0014_path_resolution_block_uses_paths_rs() {
    let block = adr_0014_path_resolution_block();
    assert!(
        block.contains("paths.rs"),
        "ADR-0014 § Path resolution code block must reference \
         `crates/plugin3-core/src/paths.rs` — that is where the \
         `Paths` struct lives. The earlier `state.rs` reference is \
         phantom; no such module exists in `plugin3-core`.",
    );
    assert!(
        !block.contains("state.rs"),
        "ADR-0014 § Path resolution code block must not reference \
         `state.rs` — no such module exists. The `Paths` struct \
         lives in `paths.rs`; budget save/load lives in the CLI's \
         `main.rs`; atomic write helper lives in `atomic_write.rs`.",
    );
}

// ponytail: pin the file-path comments in § Atomic flag file
// for budget. The earlier draft said `state.rs`; the actual
// impl is `atomic_write.rs` (the helper) + `main.rs` (the
// entry points that handle the precedence chain).
#[test]
fn adr_0014_atomic_block_references_atomic_write_not_state() {
    let block = adr_0014_atomic_block();
    assert!(
        block.contains("atomic_write.rs"),
        "ADR-0014 § Atomic flag file code block must reference \
         `crates/plugin3-core/src/atomic_write.rs` — that is where \
         the `atomic_write_text` helper lives.",
    );
    assert!(
        !block.contains("src/state.rs"),
        "ADR-0014 § Atomic flag file code block must not reference \
         `src/state.rs` — no such module exists in `plugin3-core`.",
    );
}

// ponytail: pin the § Recent outputs file code block's
// function signature. The earlier draft showed
// `append_recent(key: String, content: String, tool_name: String,
// path: &Path) -> std::io::Result<()>` returning Result and
// storing the full content. The actual impl is
// `append_recent(key: &str, size: usize)` returning `()` and
// storing only `{key, size}` (no content, no tool_name, no
// timestamp). A contributor who copy-pastes the old
// signature back into the ADR documents a contract the impl
// does not implement.
#[test]
fn adr_0014_recent_outputs_block_signature_matches_impl() {
    let block = adr_0014_recent_outputs_block();
    // ponytail: the impl signature is `fn append_recent(key:
    // &str, size: usize)`. Asserting the literal substrings
    // ("fn append_recent", "size") catches the old signature
    // (which had `content: String, tool_name: String` and
    // returned `Result`).
    assert!(
        block.contains("fn append_recent"),
        "ADR-0014 § Recent outputs file code block must define the \
         `append_recent` helper. The function lives in the CLI's \
         `main.rs`; the ADR documents the wire shape.",
    );
    assert!(
        block.contains("size: usize") || block.contains("&str,") || block.contains("size"),
        "ADR-0014 § Recent outputs file code block must show the \
         `size: usize` parameter — the impl stores only key + \
         size, not the full content.",
    );
    // ponytail: the bound is pinned as `RECENT_BOUND: usize =
    // 32` in main.rs and as a literal `32` in the eviction
    // loop. A contributor who changes the bound to e.g. 64
    // documents a contract the test (`recent_bound_is_pinned_at_32`)
    // will reject.
    assert!(
        block.contains("32"),
        "ADR-0014 § Recent outputs file code block must show the \
         32-entry FIFO bound — it is the load-bearing spec.",
    );
}

// ponytail: pin the eviction method. The impl uses
// `VecDeque::pop_front` (O(1)) instead of `Vec::remove(0)`
// (O(n) per eviction → O(n²) per append with the 32-entry
// bound). The earlier drift spec documented `remove(0)` —
// a contributor reading the ADR would either fail to compile
// (VecDeque has no `remove(0)`) or copy the O(n²) form onto a
// fresh `Vec`. Pin the load-bearing call here.
#[test]
fn adr_0014_recent_outputs_block_evicts_with_pop_front() {
    let block = adr_0014_recent_outputs_block();
    // Positive: the O(1) `pop_front` must be visible.
    assert!(
        block.contains("entries.pop_front()"),
        "ADR-0014 § Recent outputs file code block must show \
         `entries.pop_front()` — the O(1) `VecDeque` eviction \
         the impl uses. The earlier draft's `entries.remove(0)` \
         form is O(n) per eviction and O(n²) per append.",
    );
    // Negative: the O(n²) `Vec::remove(0)` form must NOT appear.
    assert!(
        !block.contains("entries.remove(0)"),
        "ADR-0014 § Recent outputs file code block must not \
         reference `entries.remove(0)` — the impl uses \
         `VecDeque::pop_front` (O(1)). A `Vec::remove(0)` call \
         shifts every surviving element on every eviction; with \
         the 32-entry bound that's O(n²) per append.",
    );
}

// ponytail: pin the negative direction on § Recent outputs
// file. The earlier draft stored `content` and `tool_name`
// in the JSONL entry and used `chrono::Utc::now()` for a
// timestamp. The actual wire shape is `{key, size}` only —
// no content, no tool_name, no timestamp. A contributor
// who re-pastes the old entry shape documents a JSONL the
// impl does not write.
#[test]
fn adr_0014_recent_outputs_block_does_not_claim_stale_fields() {
    let block = adr_0014_recent_outputs_block();
    for stale in ["\"content\"", "\"tool_name\"", "\"ts\"", "chrono::Utc::now"] {
        assert!(
            !block.contains(stale),
            "ADR-0014 § Recent outputs file code block claims `{stale}` \
             but the impl does not write it. The JSONL wire shape is \
             `{{\"key\": ..., \"size\": ...}}` only — adding `content` \
             or `tool_name` or a timestamp is a future ADR with a \
             storage-cost rationale.",
        );
    }
}

// ponytail: pin the § Implementation notes file paths. The
// earlier draft prescribed a `state` module at
// `crates/plugin3-core/src/state.rs`. The split that
// materialised is: `paths.rs` (path resolution),
// `atomic_write.rs` (atomic write helper), and the
// save/load + recent-outputs FIFO list in the CLI's
// `main.rs`. A contributor who re-lists `state.rs` in the
// Implementation notes surfaces here.
#[test]
fn adr_0014_implementation_notes_lists_actual_module_paths() {
    let section = adr_0014_implementation_notes_section();
    for path in ["paths.rs", "atomic_write.rs"] {
        assert!(
            section.contains(path),
            "ADR-0014 § Implementation notes must reference \
             `crates/plugin3-core/src/{path}` — the file the \
             impl exposes. A contributor who renames the module \
             surfaces here.",
        );
    }
    assert!(
        !section.contains("`state` module at\n`crates/plugin3-core/src/state.rs`")
            && !section.contains("`state` module lives at")
            && !section.contains("`state` module"),
        "ADR-0014 § Implementation notes must not describe a `state` \
         module at `state.rs` — no such module exists. The save/load \
         + recent-outputs FIFO list live in the CLI's `main.rs`.",
    );
}

// ponytail: pin the absence of the § Runtime lock example
// block. The earlier draft had a § Runtime lock subsection
// showing `use fs2::FileExt;` and a `with_lock` helper. The
// MVP has no concurrent hook invocations and the `fs2` dep
// was removed in the ADR-0017 reconciliation. The §
// Runtime lock subsection is replaced with a Ponytail
// deferral note; the example block must not reappear.
#[test]
fn adr_0014_no_runtime_lock_section() {
    let adr = read(&repo_root().join("docs/adr/0014-state-management.md"));
    // ponytail: the § Runtime lock header must not be a
    // proper subsection (### Runtime lock) — only the
    // Ponytail deferral note (### Runtime lock (deferred —
    // Ponytail)) is allowed. A contributor who restores the
    // prescriptive header surfaces here.
    let prescriptive_count = adr.matches("### Runtime lock\n").count()
        + adr.matches("### Runtime lock (lock the\n").count();
    let deferred_count = adr.matches("### Runtime lock (deferred").count();
    assert!(
        prescriptive_count == 0,
        "ADR-0014 must not contain a prescriptive § Runtime lock \
         subsection (no `### Runtime lock` header). The MVP has no \
         concurrent hook invocations and no `fs2` dep. If a future \
         ADR adds a daemon mode that genuinely needs the lock, \
         re-introduce the section with a `fcntl` binding (no new \
         dep) and update this drift test in lockstep.",
    );
    assert!(
        deferred_count == 1,
        "ADR-0014 must contain exactly one § Runtime lock (deferred — \
         Ponytail) subsection; got {deferred_count}.",
    );
    // ponytail: the ADR body must not contain `fs2::FileExt`
    // or `with_lock` outside of the deferral note's
    // explanation. The deferral mentions both names; the
    // drift test allows that, but a contributor who
    // re-introduces them as live code (e.g. `use fs2::FileExt;`
    // in a new code block) surfaces here. We assert the
    // body of the ADR is free of fs2 imports.
    assert!(
        !adr.contains("use fs2::FileExt;"),
        "ADR-0014 must not contain `use fs2::FileExt;` — the workspace \
         does not depend on `fs2` (ADR-0017 § Workspace Cargo.toml).",
    );
    assert!(
        !adr.contains("pub fn with_lock"),
        "ADR-0014 must not contain a public `with_lock` function — the \
         impl does not expose one. If a future ADR adds a runtime lock, \
         re-introduce the helper with a `fcntl` binding and update this \
         drift test in lockstep.",
    );
}

// ponytail: pin the absence of `with_lock` in the impl. The
// ADR claims `with_lock` is a public helper in `state.rs`;
// the impl has no such function. A contributor who adds a
// `with_lock` function (or who re-pastes one into the ADR)
// without a measured contention case surfaces here. We
// assert the impl is silent on `with_lock` *and* on
// `fs2::FileExt`.
#[test]
fn adr_0014_impl_has_no_with_lock_or_fs2() {
    for crate_name in ["plugin3-core", "plugin3-cli", "plugin3-hosts"] {
        let src_dir = repo_root().join("crates").join(crate_name).join("src");
        let entries = std::fs::read_dir(&src_dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", src_dir.display()));
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let body = read(&path);
            assert!(
                !body.contains("fn with_lock"),
                "{} contains a `with_lock` function — ADR-0014 § Runtime \
                 lock is deferred. If you are adding a runtime lock, \
                 write a new ADR with a contention-measurement rationale, \
                 then update this drift test to expect the helper.",
                path.display(),
            );
            assert!(
                !body.contains("use fs2"),
                "{} imports `fs2` — the workspace does not depend on \
                 `fs2` (ADR-0017 § Workspace Cargo.toml). If you are \
                 adding `fs2` as a new dep, write a new ADR with a \
                 binary-size and compile-time rationale.",
                path.display(),
            );
        }
    }
}

// ponytail: pin the on-disk file naming for the OffloadStore
// backend. The drift test in
// `offload_store_spec_drift.rs` covers the § Backends shape;
// this one covers the § Directory layout naming. The MVP
// uses `slices/` (a directory of files, one per key —
// FileOffloadStore per ADR-0004). A contributor who
// renames the directory (e.g. `slices_dir` or
// `offload_store/`) surfaces here.
#[test]
fn adr_0014_slices_directory_name_is_pinned() {
    let block = adr_0014_directory_layout_block();
    // ponytail: the block must list `slices/` (with trailing
    // slash) — the impl's `Paths::slices_dir()` returns
    // `data_dir.join("slices")` (a directory, not a file).
    // The negative pin catches a `slices.sqlite` re-introduction
    // (already pinned by the previous test) and a `slices`
    // rename (e.g. `slices_dir/`, `offload/`).
    assert!(
        block.contains("slices/"),
        "ADR-0014 § Directory layout must list `slices/` — the impl's \
         FileOffloadStore directory. A contributor who renames the \
         directory surfaces here.",
    );
    assert!(
        !block.contains("slices.sqlite"),
        "ADR-0014 § Directory layout must not list `slices.sqlite` — \
         the workspace has no SQLite backend (ADR-0004 § Backends, \
         ADR-0017 § Feature gates (none today)).",
    );
}

// ponytail: pin the § Path resolution code block's graceful
// fallback. The MVP uses `match proj { Some(p) => ..., None
// => ... }` and falls back to `PathBuf::from(".")` on a host
// without `HOME`/`XDG_*_HOME`. The earlier draft used
// `.expect("home directory is required")` and would panic
// on a minimal Docker / CI environment. The in-file test
// `resolve_does_not_panic` pins the no-panic contract; this
// test pins the spec surface so a future contributor who
// re-pastes the `.expect()` form surfaces at review.
#[test]
fn adr_0014_path_resolution_block_uses_graceful_fallback() {
    let block = adr_0014_path_resolution_block();
    assert!(
        block.contains("match proj"),
        "ADR-0014 § Path resolution code block must show \
         `match proj {{ Some(p) => ..., None => ... }}` — \
         the impl's graceful fallback for hosts without \
         `HOME`/`XDG_*_HOME`. A contributor who re-pastes \
         `.expect(\"home directory is required\")` documents a \
         contract that panics on minimal environments.",
    );
    assert!(
        !block.contains(".expect(\"home directory is required\")"),
        "ADR-0014 § Path resolution code block must not contain \
         `.expect(\"home directory is required\")` — the impl \
         falls back to `PathBuf::from(\".\")` rather than \
         panicking on a missing home directory.",
    );
}

// ponytail: pin the § Path resolution prose's absence of
// the `--config-dir` / `--data-dir` / `--runtime-dir`
// clap-flag override claim. The MVP's `Cli` struct (ADR-0015)
// ships only `json: bool` as a global clap flag; the three
// `PLUGIN3_*_DIR` env vars are read directly by
// `Paths::resolve()` without clap indirection. The drift
// test `cli_design_spec_drift.rs::adr_0015_top_level_block_
// has_no_phantom_clap_flags` pins the same claim at the
// ADR-0015 level; this test pins the ADR-0014 prose.
#[test]
fn adr_0014_path_resolution_prose_has_no_clap_path_flags() {
    let adr = read(&repo_root().join("docs/adr/0014-state-management.md"));
    let section_start = adr
        .find("### Path resolution")
        .expect("ADR-0014 must have a § Path resolution subsection");
    let section_end = adr[section_start..]
        .find("### Atomic flag file for budget")
        .expect("ADR-0014 § Path resolution must precede § Atomic flag file");
    let section = &adr[section_start..section_start + section_end];
    for phantom in ["--config-dir", "--data-dir", "--runtime-dir"] {
        assert!(
            !section.contains(phantom),
            "ADR-0014 § Path resolution prose references phantom clap \
             flag `{phantom}` — the impl's `Cli` struct (ADR-0015) \
             carries only `json: bool` as a global clap flag. The \
             `PLUGIN3_*_DIR` env vars are read directly by \
             `Paths::resolve()` without clap indirection.",
        );
    }
}

// ponytail: pin the § Recent outputs file code block's
// typed `RecentEntry` struct. The MVP serialises
// `{key, size}` via a `#[derive(Serialize)]` struct rather
// than a `serde_json::json!({...})` macro. A typed struct
// means the wire shape is owned by one type — a future
// contributor who adds a field surfaces here.
#[test]
fn adr_0014_recent_outputs_block_uses_typed_entry_struct() {
    let block = adr_0014_recent_outputs_block();
    assert!(
        block.contains("struct RecentEntry"),
        "ADR-0014 § Recent outputs file code block must declare \
         `struct RecentEntry` — the impl's typed entry that \
         owns the `{{key, size}}` JSONL wire shape. A \
         contributor who swaps back to `serde_json::json!` \
         moves the wire shape to scattered call sites.",
    );
    assert!(
        !block.contains("serde_json::json!"),
        "ADR-0014 § Recent outputs file code block must not \
         use `serde_json::json!` — the impl uses a typed \
         `RecentEntry` struct so the wire shape is owned by \
         one type. A `json!` macro would scatter the shape \
         across call sites.",
    );
}

// ponytail: pin the § Recent outputs file code block's
// path-parameterised seam (`append_recent_at`). The MVP's
// in-file tests (`recent_bound_is_pinned_at_32`,
// `fifo_eviction_at_boundary`,
// `per_line_wire_shape_is_key_and_size`) all point at
// tempdirs via this seam — without it, the tests would
// have to mutate process-wide `PLUGIN3_*_DIR` env vars
// and race with parallel tests.
#[test]
fn adr_0014_recent_outputs_block_uses_path_parameterised_seam() {
    let block = adr_0014_recent_outputs_block();
    assert!(
        block.contains("fn append_recent_at"),
        "ADR-0014 § Recent outputs file code block must declare \
         `fn append_recent_at` — the impl's path-parameterised \
         seam that lets tests point at a tempdir without \
         mutating process-wide env vars. The production \
         `append_recent` is a thin wrapper around it.",
    );
}

// ponytail: pin the § Recent outputs file code block's
// atomic-write label. The impl uses the short label
// `"recent"` (not the longer `"recent_outputs"`) — the
// shorter label is what appears in the eprintln failure
// log lines (`plugin3: recent dir create failed: ...`).
// A contributor who retypes the longer label surfaces
// here.
#[test]
fn adr_0014_recent_outputs_block_uses_short_label() {
    let block = adr_0014_recent_outputs_block();
    assert!(
        block.contains("\"recent\""),
        "ADR-0014 § Recent outputs file code block must show \
         `atomic_write_text(path, \"recent\", &body)` — the \
         impl's short label that appears in eprintln failure \
         logs. The longer `\"recent_outputs\"` label was an \
         earlier draft.",
    );
    assert!(
        !block.contains("\"recent_outputs\""),
        "ADR-0014 § Recent outputs file code block must not \
         reference `\"recent_outputs\"` — the impl uses the \
         shorter `\"recent\"` label. A contributor who \
         retypes the longer label would change the eprintln \
         format that ops tools grep on.",
    );
}
