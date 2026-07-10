//! Integration test: every bundled plugin under `plugins/` loads cleanly.
//!
//! This catches manifest/schema drift (e.g. a tool references a missing
//! script, a trust tier is misspelled, or a hook event is unknown) before
//! the host tries to use the plugin at runtime.

use std::path::PathBuf;

use kirkforge_plugin::Plugin;
use kirkforge_plugin_host::{PluginRegistry, TrustPolicy};

fn plugins_dir() -> PathBuf {
    // <crate>/crates/kirkforge-plugin-host -> repo root -> plugins
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("plugins")
        .canonicalize()
        .expect("plugins directory should exist")
}

#[test]
fn all_bundled_plugins_load_without_warnings() {
    let mut registry = PluginRegistry::new();
    let warnings = registry
        .load_from_dir(
            &plugins_dir(),
            TrustPolicy::up_to(kirkforge_plugin::TrustTier::Shell),
        )
        .expect("loading bundled plugins should not fail");

    assert!(
        warnings.is_empty(),
        "bundled plugins produced load warnings: {warnings:?}"
    );

    let active = registry.active_plugins();
    let names: Vec<_> = active
        .iter()
        .map(|p| p.plugin.manifest().name.clone())
        .collect();

    for expected in [
        "kirkforge-draw",
        "kirkforge-plugin",
        "kirkforge-plugin3",
        "kirkforge-video",
        "stratum",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "expected bundled plugin {expected:?} to be active; got {names:?}"
        );
    }
}
