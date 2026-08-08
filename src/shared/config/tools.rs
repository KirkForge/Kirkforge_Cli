use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::shared::TruncationStrategy;
use crate::shared::{LspServerEntry, McpServerConfig};

fn default_max_tool_calls_per_turn() -> usize {
    50
}

fn default_max_persona_turns() -> usize {
    10
}

fn default_max_continuation_rounds() -> usize {
    5
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

fn default_minify_above_bytes() -> usize {
    4096
}

fn default_scheduled_bash_auto_approve() -> bool {
    false
}

fn default_max_concurrent_scheduled_jobs() -> usize {
    4
}

fn default_max_background_tasks() -> usize {
    4
}

fn default_task_concurrency_mode() -> String {
    "queue".to_string()
}

fn default_reject_on_excess_plugin_trust() -> bool {
    true
}

fn default_budget_ceiling() -> usize {
    200_000
}

fn default_budget_approaching_ratio() -> f64 {
    0.8
}

fn default_plugin_sources() -> HashMap<String, PathBuf> {
    let mut sources = HashMap::new();
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    sources.insert("kf-draw".into(), base.join("plugins/kf-draw"));
    #[cfg(feature = "video")]
    sources.insert("kf-video".into(), base.join("plugins/kf-video"));
    sources.insert("stratum".into(), base.join("plugins/stratum"));
    sources.insert("kf-budget".into(), base.join("plugins/kf-budget"));
    sources.insert("kf-plugin".into(), base.join("plugins/kf-plugin"));
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
    #[serde(default = "default_max_continuation_rounds")]
    pub max_continuation_rounds: usize,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub hooks_dir: Option<PathBuf>,
    #[serde(default = "default_minify_write_side")]
    pub minify_write_side: bool,
    #[serde(default = "default_minify_above_bytes")]
    pub minify_above_bytes: usize,
    #[serde(default)]
    pub follow_symlinks: bool,
    #[serde(default)]
    pub block_binary_reads: bool,
    #[serde(default = "default_scheduled_bash_auto_approve")]
    pub scheduled_bash_auto_approve: bool,
    #[serde(default = "default_max_concurrent_scheduled_jobs")]
    pub max_concurrent_scheduled_jobs: usize,
    #[serde(default = "default_max_background_tasks")]
    pub max_background_tasks: usize,
    #[serde(default = "default_task_concurrency_mode")]
    pub task_concurrency_mode: String,
    #[serde(default = "default_max_plugin_trust")]
    pub max_plugin_trust: kf_plugin_sdk::TrustTier,
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
    #[serde(default)]
    pub disabled_plugins: HashSet<String>,
    #[serde(default)]
    pub stratum_mode: Option<String>,
    #[serde(default = "default_budget_ceiling")]
    pub budget_ceiling: usize,
    #[serde(default = "default_budget_approaching_ratio")]
    pub budget_approaching_ratio: f64,
}

fn default_max_plugin_trust() -> kf_plugin_sdk::TrustTier {
    kf_plugin_sdk::TrustTier::Shell
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
            max_continuation_rounds: default_max_continuation_rounds(),
            dry_run: false,
            hooks_dir: None,
            minify_write_side: false,
            minify_above_bytes: default_minify_above_bytes(),
            follow_symlinks: false,
            block_binary_reads: false,
            scheduled_bash_auto_approve: false,
            max_concurrent_scheduled_jobs: default_max_concurrent_scheduled_jobs(),
            max_background_tasks: default_max_background_tasks(),
            task_concurrency_mode: default_task_concurrency_mode(),
            max_plugin_trust: default_max_plugin_trust(),
            reject_on_excess_plugin_trust: default_reject_on_excess_plugin_trust(),
            plugin_signature_validation: false,
            plugin_public_key_path: None,
            plugin_allowed_env_vars: vec![],
            plugin_sources: default_plugin_sources(),
            enabled_plugins: default_enabled_plugins(),
            disabled_plugins: HashSet::new(),
            stratum_mode: None,
            budget_ceiling: default_budget_ceiling(),
            budget_approaching_ratio: default_budget_approaching_ratio(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_config_defaults_match_spec() {
        let cfg = ToolConfig::default();
        assert_eq!(cfg.budget_ceiling, 200_000);
        assert!((cfg.budget_approaching_ratio - 0.8).abs() < f64::EPSILON);
        assert!(cfg.stratum_mode.is_none());
        assert_eq!(cfg.minify_above_bytes, 4096);
        assert_eq!(cfg.max_continuation_rounds, 5);
    }

    #[test]
    fn tool_config_toml_overrides_defaults() {
        let toml = r#"
stratum_mode = "lite"
budget_ceiling = 50000
budget_approaching_ratio = 0.9
minify_above_bytes = 1024
"#;
        let cfg: ToolConfig = toml::from_str(toml).expect("parse");
        assert_eq!(cfg.stratum_mode.as_deref(), Some("lite"));
        assert_eq!(cfg.budget_ceiling, 50_000);
        assert!((cfg.budget_approaching_ratio - 0.9).abs() < f64::EPSILON);
        assert_eq!(cfg.minify_above_bytes, 1024);
    }

    #[test]
    fn tool_config_toml_omitted_uses_defaults() {
        let toml = "";
        let cfg: ToolConfig = toml::from_str(toml).expect("parse");
        assert_eq!(cfg.budget_ceiling, 200_000);
        assert!((cfg.budget_approaching_ratio - 0.8).abs() < f64::EPSILON);
        assert!(cfg.stratum_mode.is_none());
    }
}
