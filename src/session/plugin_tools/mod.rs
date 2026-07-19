//! Tool wrappers for KirkForge plugins.
//!
//! Plugin tools are loaded from `~/.local/share/kirkforge/plugins` via the
//! `PluginRegistry`. Each plugin tool is wrapped to implement the executor's
//! `Tool` trait. Plugin tool scripts are invoked asynchronously with a
//! sandboxed working directory, curated environment, timeout, and process-group
//! cleanup.
//!
//! This module is split into two submodules:
//! - [`wrapper`] defines [`PluginToolWrapper`], the `Tool` impl that wraps a
//!   plugin's external command.
//! - [`loader`] contains the plugin loader hub: `plugins_dir`,
//!   `trust_policy_from_config`, `load_workspace_plugins`,
//!   `load_plugin_registry`, and `all_plugin_tools`.
//!
//! Everything that was public in the original single-file module is re-exported
//! here so existing call sites (`crate::session::plugin_tools::*`) keep
//! resolving unchanged.

pub mod loader;
pub mod wrapper;

pub use loader::{
    all_plugin_tools, load_plugin_registry, load_workspace_plugins, plugins_dir,
    trust_policy_from_config,
};
pub use wrapper::PluginToolWrapper;

// Test-only re-export so the in-tree test module can call `npm_bin_dirs()`
// via `use super::*;` without widening the crate's public API.
#[cfg(test)]
pub(crate) use wrapper::npm_bin_dirs;

#[cfg(test)]
#[allow(unused_imports)]
mod tests;
