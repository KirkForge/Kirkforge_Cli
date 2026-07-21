//! ADR-0017 § Size budget — pins the 8 MB release-binary cap.
//!
//! ponytail: one assertion. The release profile (lto=thin,
//! codegen-units=1, strip=symbols, panic=abort) is what keeps
//! the binary small. A contributor who adds a heavy dep
//! (tokio, heavy crypto, image-processing) silently blows the
//! budget; this test makes the regression loud. The test
//! skips cleanly when the release binary hasn't been built
//! yet (a fresh checkout running only `cargo test`) so it
//! never blocks a `cargo test --workspace` run.

use std::path::Path;

/// ADR-0017 § Size budget — 8 MB hard cap on the release binary.
const SIZE_BUDGET_BYTES: u64 = 8 * 1024 * 1024;

fn release_binary_path() -> std::path::PathBuf {
    // ponytail: resolve relative to CARGO_MANIFEST_DIR so the test
    // finds the binary regardless of where cargo runs from. Layout
    // is `crates/plugin3-core/tests/...` → workspace root is three
    // levels up; from there `target/release/plugin3` is the standard
    // cargo location.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2) // crates/plugin3-core -> crates -> workspace root
        .expect("workspace root")
        .join("target")
        .join("release")
        .join("plugin3")
}

/// Returns the byte size of the release binary, or None if absent.
fn release_binary_size() -> Option<u64> {
    let p = release_binary_path();
    if !p.is_file() {
        return None;
    }
    Some(std::fs::metadata(&p).ok()?.len())
}

#[test]
fn release_binary_under_size_budget() {
    let Some(size) = release_binary_size() else {
        eprintln!(
            "skipping: release binary not built at {}; \
             run `cargo build --release --bin plugin3` to enforce the budget.",
            release_binary_path().display(),
        );
        return;
    };
    assert!(
        size <= SIZE_BUDGET_BYTES,
        "release binary is {size} bytes ({} MB), exceeds ADR-0017 budget of {} MB ({} bytes).\n  path: {}",
        size / (1024 * 1024),
        SIZE_BUDGET_BYTES / (1024 * 1024),
        SIZE_BUDGET_BYTES,
        release_binary_path().display(),
    );
}

#[test]
fn size_budget_constant_is_pinned() {
    // ponytail: the budget constant is the load-bearing contract.
    // A contributor who tunes it (e.g. 8 MB → 16 MB) without
    // updating the README's "release binary <8 MB" claim surfaces
    // the change here.
    assert_eq!(SIZE_BUDGET_BYTES, 8 * 1024 * 1024);
}

#[test]
fn release_profile_uses_size_optimisations() {
    // ponytail: ADR-0017 § Workspace Cargo.toml specifies the
    // release profile (`lto = "thin"`, `codegen-units = 1`,
    // `strip = "symbols"`, `panic = "abort"`). Read the workspace
    // Cargo.toml and assert each key so a contributor who softens
    // any of these (e.g. `lto = false`) gets a review prompt
    // here AND a silent binary-size regression caught above.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_toml = manifest
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("Cargo.toml");
    let body = std::fs::read_to_string(&workspace_toml)
        .unwrap_or_else(|e| panic!("read {}: {e}", workspace_toml.display()));
    for needle in [
        "lto = \"thin\"",
        "codegen-units = 1",
        "strip = \"symbols\"",
        "panic = \"abort\"",
    ] {
        assert!(
            body.contains(needle),
            "workspace Cargo.toml missing ADR-0017 release setting `{needle}` (path: {})",
            workspace_toml.display(),
        );
    }
    let _ = Path::new("."); // keep std::path::Path in scope for clippy
}
