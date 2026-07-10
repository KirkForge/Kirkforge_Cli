//! Build script for `kirkstratum-hosts`.

fn main() {
    // Re-run the build when the canonical ruleset changes so host adapters
    // receive the updated rules without requiring a clean rebuild.
    println!("cargo:rerun-if-changed=docs/rules/CANONICAL.md");
}
