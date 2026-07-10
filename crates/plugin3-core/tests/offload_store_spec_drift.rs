//! ADR-0004 (`OffloadStore`) drift tests — the contracts that
//! live in the ADR prose and must stay in lockstep with the
//! `plugin3-core/src/store.rs` impl. Companion to
//! `store_drift.rs` (which pins the key-format wire contract);
//! this file pins the *spec surface* — backends count, file
//! path, mutex choice, and the absence of phantom `SQLite` /
//! `parking_lot` claims.
//!
//! ponytail: literal-substring scan per contract, no markdown
//! parser. The ADR owns the exact strings; `contains` catches
//! the silent regressions (a contributor who copy-pastes the
//! `SQLite` backend example back into the ADR documents a
//! dependency the impl does not wire, and the resulting
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

/// Read ADR-0004's § Backends code block (the only fenced
/// `rust` block in § Backends, scoped to the in-memory +
/// file backends). Excludes the explanatory paragraph above
/// the block so the prose can mention deprecated backends
/// (e.g. "`SQLite` was removed when ...") without tripping the
/// drift test.
fn adr_0004_backends_block() -> String {
    let adr = read(&repo_root().join("docs/adr/0004-offload-store.md"));
    let section_start = adr
        .find("### Backends")
        .expect("ADR-0004 must have a § Backends subsection");
    let section_end = adr[section_start..]
        .find("### Marker format")
        .expect("ADR-0004 § Backends must precede § Marker format");
    let section = &adr[section_start..section_start + section_end];

    // Find the first ```rust fence in this section — the
    // backends code block.
    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0004 § Backends must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0004 § Backends rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

fn adr_0004_loud_failure_block() -> String {
    let adr = read(&repo_root().join("docs/adr/0004-offload-store.md"));
    let section_start = adr
        .find("### Loud failure on init")
        .expect("ADR-0004 must have a § Loud failure on init subsection");
    let section_end = adr[section_start..]
        .find("## Consequences")
        .expect("ADR-0004 § Loud failure on init must precede § Consequences");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0004 § Loud failure on init must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0004 § Loud failure on init rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

/// Read ADR-0004's full § Loud failure on init — both prose
/// and the fenced code block. The fallback-carve-out
/// justification is prose (the ADR-0009 cross-reference and
/// the data-loss-on-restart acknowledgement), not code, so
/// the code-block-only helper above can't pin it.
fn adr_0004_loud_failure_section() -> String {
    let adr = read(&repo_root().join("docs/adr/0004-offload-store.md"));
    let section_start = adr
        .find("### Loud failure on init")
        .expect("ADR-0004 must have a § Loud failure on init subsection");
    let section_end = adr[section_start..]
        .find("## Consequences")
        .expect("ADR-0004 § Loud failure on init must precede § Consequences");
    adr[section_start..section_start + section_end].to_string()
}

fn adr_0004_marker_block() -> String {
    let adr = read(&repo_root().join("docs/adr/0004-offload-store.md"));
    let section_start = adr
        .find("### Marker format")
        .expect("ADR-0004 must have a § Marker format subsection");
    let section_end = adr[section_start..]
        .find("### Loud failure on init")
        .expect("ADR-0004 § Marker format must precede § Loud failure on init");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0004 § Marker format must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0004 § Marker format rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

fn adr_0004_key_format_block() -> String {
    let adr = read(&repo_root().join("docs/adr/0004-offload-store.md"));
    let section_start = adr
        .find("### Key format")
        .expect("ADR-0004 must have a § Key format subsection");
    let section_end = adr[section_start..]
        .find("### Backends")
        .expect("ADR-0004 § Key format must precede § Backends");
    let section = &adr[section_start..section_start + section_end];

    let fence_start = section
        .find("```rust\n")
        .expect("ADR-0004 § Key format must contain a rust code block");
    let fence_after = &section[fence_start + "```rust\n".len()..];
    let fence_end_rel = fence_after
        .find("```")
        .expect("ADR-0004 § Key format rust code block must close");
    fence_after[..fence_end_rel].to_string()
}

// ponytail: pin the negative direction on § Backends. The
// earlier draft listed a third `SqliteOffloadStore` backend
// gated on a `sqlite` feature. The MVP ships without
// `rusqlite`; the § Backends example block must not claim a
// SQLite backend, a `rusqlite` dep, or a `parking_lot` mutex
// (the SQLite example used `parking_lot::Mutex`).
#[test]
fn adr_0004_backends_block_does_not_claim_sqlite_or_parking_lot() {
    let block = adr_0004_backends_block();
    for phantom in [
        "SqliteOffloadStore",
        "rusqlite",
        "parking_lot",
        "parking_lot::Mutex",
    ] {
        assert!(
            !block.contains(phantom),
            "ADR-0004 § Backends example block claims `{phantom}` but the \
             impl does not wire it. The MVP ships only InMemoryOffloadStore \
             and FileOffloadStore — adding SQLite is a future ADR with a \
             binary-size budget to negotiate. If you re-introduce the \
             SQLite backend, update both the ADR and the impl, then update \
             this drift test to expect the new name.",
        );
    }
}

// ponytail: pin the positive direction — the § Backends
// example block must list the two implemented backends by
// their concrete type names. A contributor who renames
// `InMemoryOffloadStore` or `FileOffloadStore` surfaces here
// (the names are part of the public API contract that
// `store.rs` exports and that callers in the test corpus
// depend on).
#[test]
fn adr_0004_backends_block_names_both_impl_backends() {
    let block = adr_0004_backends_block();
    for backend in ["InMemoryOffloadStore", "FileOffloadStore"] {
        assert!(
            block.contains(backend),
            "ADR-0004 § Backends example block must name `{backend}` — \
             it is the only persisted-backend type the impl exports today.",
        );
    }
}

// ponytail: pin the negative direction on § Loud failure on
// init. The earlier draft's example used `parking_lot::Mutex`
// and `rusqlite::Connection::open`. Neither is in the
// workspace today; the example must demonstrate the
// `FileOffloadStore::open` shape with `std::fs::create_dir_all`.
#[test]
fn adr_0004_loud_failure_block_uses_std_mutex_not_parking_lot() {
    let block = adr_0004_loud_failure_block();
    assert!(
        !block.contains("parking_lot::Mutex"),
        "ADR-0004 § Loud failure on init example must not use \
         `parking_lot::Mutex` — the workspace does not depend on \
         `parking_lot` (ADR-0017 § Workspace Cargo.toml).",
    );
    assert!(
        !block.contains("rusqlite"),
        "ADR-0004 § Loud failure on init example must not reference \
         `rusqlite` — no SQLite backend is wired in the MVP.",
    );
    // ponytail: positive pin — the example must show the
    // `FileOffloadStore::open` shape that the impl actually
    // exposes. A contributor who replaces the example with
    // the in-memory store (which is what the unit tests use)
    // would drift the spec away from the runtime default.
    assert!(
        block.contains("FileOffloadStore"),
        "ADR-0004 § Loud failure on init example must demonstrate the \
         `FileOffloadStore::open` shape — it is the persistent backend \
         whose init failure modes the spec is documenting.",
    );
}

// ponytail: pin the negative direction on a SQL schema. The
// earlier ADR draft had a `CREATE TABLE slices ...` block
// describing the SQLite schema. The MVP has no SQLite
// backend; no SQL DDL must appear in the ADR.
#[test]
fn adr_0004_contains_no_sql_ddl() {
    let adr = read(&repo_root().join("docs/adr/0004-offload-store.md"));
    for sql_phrase in [
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE UNIQUE INDEX",
        "INTEGER NOT NULL", // generic SQL column constraint
        "BLOB NOT NULL",    // generic SQL column constraint
    ] {
        assert!(
            !adr.contains(sql_phrase),
            "ADR-0004 must not contain SQL DDL (`{sql_phrase}`) — the \
             MVP has no SQLite backend. If a future ADR adds SQLite, \
             re-introduce the schema in that ADR with the rusqlite dep, \
             then update this drift test to expect the new schema block.",
        );
    }
}

// ponytail: pin the § Marker format block against the impl
// constants. The marker is the cross-plugin wire format — a
// Plugin3 slice marker must round-trip in `stratum cat
// <marker>`, and `grep -F '<<plugin3:slice:'` is the
// user-facing tool contract. Both prefix and suffix literals
// must match `store.rs` byte-for-byte.
#[test]
fn adr_0004_marker_block_pins_literal_prefix_and_suffix() {
    let block = adr_0004_marker_block();
    assert!(
        block.contains("\"<<plugin3:slice:\""),
        "ADR-0004 § Marker format block must pin SLICE_MARKER_PREFIX as \
         `\"<<plugin3:slice:\"` to match `store.rs`. Got:\n{block}",
    );
    assert!(
        block.contains("\"\\\"<<plugin3:slice:\"") || block.contains("\"<<plugin3:slice:\""),
        "ADR-0004 § Marker format must name the prefix literal",
    );
    assert!(
        block.contains("\"\\\">>\\\"\"") || block.contains("\">>\""),
        "ADR-0004 § Marker format must name the suffix literal `\"\\\">>\\\"\"`",
    );
    // ponytail: explicit raw-string-or-escaped check. The
    // marker block must contain the literal `<<plugin3:slice:`
    // substring (not just an escape-encoded version) so a
    // contributor who copy-pastes the prefix into a test or
    // docs page gets the right bytes.
    assert!(
        block.contains("<<plugin3:slice:") && block.contains(">>"),
        "ADR-0004 § Marker format block must contain the literal \
         `<<plugin3:slice:` prefix and `>>` suffix strings — they are \
         the wire-format contract per ADR-0004 § Marker format and the \
         cross-plugin byte-compat guarantee per § Key format.",
    );
}

// ponytail: pin the § Key format block's truncation length.
// The byte-compat contract is 24 hex chars (96 bits). A
// contributor who re-pastes a 32-hex version documents a
// contract the impl does not implement.
#[test]
fn adr_0004_key_format_block_pins_24_hex_truncation() {
    let block = adr_0004_key_format_block();
    assert!(
        block.contains("[..24]"),
        "ADR-0004 § Key format block must show `hex.as_str()[..24]` to \
         match `store.rs::make_key` — the byte-compat contract is 24 hex \
         chars. A bump to 32 hex is a coordinated ADR with Stratum.",
    );
    assert!(
        block.contains("24 hex") || block.contains("24-hex"),
        "ADR-0004 § Key format block must name the 24-hex contract — the \
         prose around the code block is the source of truth for the \
         byte-compat length, and a contributor who reads only the code \
         and not the prose gets a half-spec.",
    );
}

// ponytail: pin the § Implementation notes file path. The
// earlier draft said `store/mod.rs`; the consolidated
// single-file layout lives at `store.rs`. A contributor who
// re-splits the file into a `store/` module tree must update
// the ADR (and the drift test in lockstep).
#[test]
fn adr_0004_implementation_path_is_store_rs() {
    let adr = read(&repo_root().join("docs/adr/0004-offload-store.md"));
    let section_start = adr
        .find("## Implementation notes")
        .expect("ADR-0004 must have an Implementation notes section");
    let section = &adr[section_start..];

    // ponytail: the path is `store.rs` (single file, not
    // `store/mod.rs`). The negative pin catches a contributor
    // who copy-pastes the old `store/mod.rs` reference back
    // into the ADR — the new layout puts both backends in a
    // single file.
    assert!(
        section.contains("store.rs"),
        "ADR-0004 § Implementation notes must reference \
         `crates/plugin3-core/src/store.rs` (single-file layout) — \
         the trait + both backends live in one module today.",
    );
    assert!(
        !section.contains("store/mod.rs"),
        "ADR-0004 § Implementation notes must not reference `store/mod.rs` \
         — the impl is a single file, not a module directory. If a \
         future contributor re-splits the file into `store/` submodules, \
         update this drift test to expect the new layout in lockstep.",
    );
}

// ponytail: pin the § Implementation notes drift-test path.
// ADR-0004 says the drift test lives at
// `crates/plugin3-core/tests/store_drift.rs`. The actual
// test file IS at that path (verified in `store_drift.rs`'s
// own header docstring). A contributor who moves the test
// file under a subdirectory surfaces here.
#[test]
fn adr_0004_implementation_path_drift_test_exists() {
    let path = repo_root().join("crates/plugin3-core/tests/store_drift.rs");
    assert!(
        path.is_file(),
        "ADR-0004 § Implementation notes says the drift test lives at \
         `crates/plugin3-core/tests/store_drift.rs` but the file is \
         missing. The store_drift.rs test pins the BLAKE3 key-format \
         contract; a contributor who removes it without updating the \
         ADR surfaces here.",
    );
}

// ponytail: pin the § Loud failure on init block's
// acknowledgement of the runtime's hook-survival fallback.
// The MVP's `open_store` helper in `crates/plugin3-cli/src/
// main.rs` catches the `FileOffloadStore::open` error,
// eprintlns to stderr, and falls back to
// `InMemoryOffloadStore::new()` — the alternative (a host
// hook crash on a transient permission error) is worse for
// the user than losing slice markers across the session.
// An earlier draft of this section claimed "the plugin never
// silently falls back to in-memory — that would lose data on
// restart" without carving out the hook-survival exception,
// and the impl had already drifted from that claim. The ADR
// now cites ADR-0009 as the justification for the fallback.
// A contributor who reverts to the pre-fix wording (drops
// the ADR-0009 cross-reference and re-pastes "never silently
// falls back") restores a documentation/implementation
// conflict that no longer reflects the runtime — caught
// here, in lockstep with the updated prose.
#[test]
fn adr_0004_loud_failure_block_acknowledges_hook_fallback() {
    let section = adr_0004_loud_failure_section();
    // Positive: the carve-out for ADR-0009 (hook must not
    // crash host) must be visible in the section, since
    // that's the justification for the runtime fallback.
    assert!(
        section.contains("ADR-0009"),
        "ADR-0004 § Loud failure on init must cross-reference \
         ADR-0009 § Error contract — the runtime falls back to \
         in-memory precisely so a host hook does not crash on a \
         transient permission error. A contributor who drops the \
         cross-reference loses the justification for the fallback.",
    );
    // Positive: the data-loss-on-restart acknowledgement
    // must be visible, since that's the cost the runtime
    // accepts when it falls back.
    assert!(
        section.contains("restart")
            || section.contains("evaporates")
            || section.contains("lost on restart")
            || section.contains("lose"),
        "ADR-0004 § Loud failure on init must acknowledge the \
         data-loss-on-restart cost of the in-memory fallback — \
         a contributor who reverts to 'never silently falls \
         back' removes both the carve-out AND the cost note.",
    );
    // Negative: the pre-fix wording that contradicts the
    // runtime must NOT appear. A contributor who reverts the
    // ADR to the pre-fix form reintroduces a documentation
    // claim that the impl violates.
    assert!(
        !section.contains("never silently falls back"),
        "ADR-0004 § Loud failure on init must not claim the \
         pre-fix 'never silently falls back' wording — the \
         runtime helper `open_store` does fall back (with an \
         eprintln to stderr, per the carve-out). That earlier \
         claim contradicted the impl; the section now carves \
         out the hook-survival exception explicitly.",
    );
}

// ponytail: pin the § Implementation notes' statement of the
// drift-test corpus source. ADR-0004 says the corpus is
// "checked against the Stratum `make_offload_key` output".
// The actual drift test pins BLAKE3 spec test vectors
// (which is what Stratum's `make_offload_key` reduces to
// too). The drift test must NOT claim a Stratum source
// checkout or a separate `stratum-fixtures/` directory — the
// fixture is self-contained, no external dep.
#[test]
fn adr_0004_drift_test_has_no_external_stratum_checkout() {
    let adr = read(&repo_root().join("docs/adr/0004-offload-store.md"));
    assert!(
        !adr.contains("stratum-fixtures"),
        "ADR-0004 must not reference a `stratum-fixtures` checkout — \
         the OffloadStore drift test is self-contained, pinning BLAKE3 \
         spec test vectors (not a vendored Stratum source tree).",
    );
    // ponytail: positive direction — the drift test file is
    // named `store_drift.rs` and is referenced explicitly in
    // § Implementation notes. A contributor who renames the
    // file (e.g. to `key_format_drift.rs`) without updating
    // the ADR surfaces here.
    assert!(
        adr.contains("store_drift.rs"),
        "ADR-0004 § Implementation notes must reference the drift-test \
         file by its concrete name `store_drift.rs` — the file is the \
         load-bearing drift contract per ADR-0016 § Drift tests #1.",
    );
}
