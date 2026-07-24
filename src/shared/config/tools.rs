use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::shared::TruncationStrategy;
use crate::shared::{LspServerEntry, McpServerConfig};

fn default_max_tool_calls_per_turn() -> usize {
    50
}

fn default_max_persona_turns() -> usize {
    10
}

fn default_tool_timeout_secs() -> Option<u64> {
    Some(30)
}

fn default_max_tool_result_chars() -> usize {
    4000
}

fn default_minify_write_side() -> bool {
    false
}

fn default_scheduled_bash_auto_approve() -> bool {
    false
}

fn default_max_concurrent_scheduled_jobs() -> usize {
    4
}

fn default_reject_on_excess_plugin_trust() -> bool {
    true
}

fn default_plugin_sources() -> HashMap<String, PathBuf> {
    let mut sources = HashMap::new();
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    sources.insert("kirkforge-draw".into(), base.join("plugins/kirkforge-draw"));
    #[cfg(feature = "video")]
    sources.insert(
        "kirkforge-video".into(),
        base.join("plugins/kirkforge-video"),
    );
    sources.insert("stratum".into(), base.join("plugins/stratum"));
    sources.insert(
        "kirkforge-plugin3".into(),
        base.join("plugins/kirkforge-plugin3"),
    );
    sources.insert(
        "kirkforge-plugin".into(),
        base.join("plugins/kirkforge-plugin"),
    );
    sources
}

fn default_enabled_plugins() -> Vec<String> {
    let mut names: Vec<String> = default_plugin_sources().keys().cloned().collect();
    names.sort();
    names
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfig {
    #[serde(default = "default_max_tool_result_chars")]
    pub max_tool_result_chars: usize,
    #[serde(default)]
    pub truncation_strategy: TruncationStrategy,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub lsp_servers: Vec<LspServerEntry>,
    #[serde(default = "default_tool_timeout_secs")]
    pub tool_timeout_secs: Option<u64>,
    #[serde(default = "default_max_tool_calls_per_turn")]
    pub max_tool_calls_per_turn: usize,
    #[serde(default = "default_max_persona_turns")]
    pub max_persona_turns: usize,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub hooks_dir: Option<PathBuf>,
    #[serde(default = "default_minify_write_side")]
    pub minify_write_side: bool,
    #[serde(default)]
    pub follow_symlinks: bool,
    #[serde(default)]
    pub block_binary_reads: bool,
    #[serde(default = "default_scheduled_bash_auto_approve")]
    pub scheduled_bash_auto_approve: bool,
    #[serde(default = "default_max_concurrent_scheduled_jobs")]
    pub max_concurrent_scheduled_jobs: usize,
    #[serde(default = "default_max_plugin_trust")]
    pub max_plugin_trust: kirkforge_plugin::TrustTier,
    #[serde(default = "default_reject_on_excess_plugin_trust")]
    pub reject_on_excess_plugin_trust: bool,
    #[serde(default)]
    pub plugin_signature_validation: bool,
    #[serde(default)]
    pub plugin_public_key_path: Option<String>,
    #[serde(default)]
    pub plugin_allowed_env_vars: Vec<String>,
    #[serde(default = "default_plugin_sources")]
    pub plugin_sources: HashMap<String, PathBuf>,
    #[serde(default = "default_enabled_plugins")]
    pub enabled_plugins: Vec<String>,
}

fn default_max_plugin_trust() -> kirkforge_plugin::TrustTier {
    kirkforge_plugin::TrustTier::Shell
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            max_tool_result_chars: default_max_tool_result_chars(),
            truncation_strategy: TruncationStrategy::KeepToolOnly,
            mcp_servers: vec![],
            lsp_servers: vec![],
            tool_timeout_secs: default_tool_timeout_secs(),
            max_tool_calls_per_turn: default_max_tool_calls_per_turn(),
            max_persona_turns: default_max_persona_turns(),
            dry_run: false,
            hooks_dir: None,
            minify_write_side: false,
            follow_symlinks: false,
            block_binary_reads: false,
            scheduled_bash_auto_approve: false,
            max_concurrent_scheduled_jobs: default_max_concurrent_scheduled_jobs(),
            max_plugin_trust: default_max_plugin_trust(),
            reject_on_excess_plugin_trust: default_reject_on_excess_plugin_trust(),
            plugin_signature_validation: false,
            plugin_public_key_path: None,
            plugin_allowed_env_vars: vec![],
            plugin_sources: default_plugin_sources(),
            enabled_plugins: default_enabled_plugins(),
        }
    }
}
