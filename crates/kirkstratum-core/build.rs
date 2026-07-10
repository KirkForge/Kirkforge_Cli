//! Build script for `kirkstratum-core`.

fn main() {
    // Re-run the build when the embedded default config changes so binaries pick
    // up edits to pipeline.toml without requiring a clean rebuild.
    println!("cargo:rerun-if-changed=config/pipeline.toml");
}
