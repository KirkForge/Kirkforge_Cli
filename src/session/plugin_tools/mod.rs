//! Tool wrappers for KirkForge plugins.
//!
//! Plugin tools are loaded from `~/.local/share/kirkforge/plugins` via the
//! `PluginRegistry`. Each plugin tool is wrapped to implement the executor's
//! `Tool` trait. Plugin tool scripts are invoked asynchronously with a
//! sandboxed working directory, curated environment, timeout, and process-group
//! cleanup.
//!
//! ## Two-path dispatch (ADR-050)
//!
//! Folded plugins (Stratum, Plugin3, Draw, Video) have two possible dispatch
//! paths:
//! - **Compiled-in** (feature on): tools register as direct Rust calls in
//!   `main/mod.rs`; the shell plugin dir is skipped by the loader.
//! - **External** (feature off): the shell plugin dir loads via the normal
//!   `PluginToolWrapper` shell-out path (graceful degradation).
//!
//! The `enabled_plugins` config is the single toggle for both paths.
//!
//! This module is split into two submodules:
//! - [`wrapper`] defines [`PluginToolWrapper`], the `Tool` impl that wraps a
//!   plugin's external command.
//! - [`loader`] contains the plugin loader hub: `plugins_dir`,
//!   `trust_policy_from_config`, `load_workspace_plugins`,
//!   `load_plugin_registry`, and `all_plugin_tools`.

pub mod loader;
pub mod wrapper;

pub use loader::{
    all_plugin_tools, folded_feature, folded_feature_enabled, is_folded, load_plugin_registry,
    load_workspace_plugins, plugins_dir, trust_policy_from_config,
};
pub use wrapper::PluginToolWrapper;

// Test-only re-export so the in-tree test module can call `npm_bin_dirs()`
// via `use super::*;` without widening the crate's public API.
#[cfg(test)]
pub(crate) use wrapper::npm_bin_dirs;

#[cfg(test)]
#[allow(unused_imports)]
mod tests;
