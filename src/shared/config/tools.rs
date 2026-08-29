use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::shared::{LspServerEntry, McpServerConfig};

fn default_max_tool_calls_per_turn() -> usize {
    200
}

fn default_max_persona_turns() -> usize {
    10
}

fn default_max_continuation_rounds() -> usize {
    20
}

fn default_tool_timeout_secs() -> Option<u64> {
    Some(120)
}

fn default_max_tool_result_chars() -> usize {
    4000
}

fn default_minify_write_side() -> bool {
    // Defaults to true: when read_file auto-minifies large files, the
    // minified content is wrapped in a <minified lang='...'> envelope.
    // edit_file detects the envelope and expands it back to raw before
    // matching. This round-trip preserves the token savings of minify
    // while ensuring edit_file string-match always works.
    true
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

/// WO 48.34: ceiling for the `task` tool's model-supplied `max_turns`.
/// Generous default — a legit subagent rarely needs more; a runaway
/// model value (u64::MAX) must not reach the executor loop. The tool
/// layer clamps against this.
pub const DEFAULT_MAX_SUBAGENT_TURNS: usize = 32;

fn default_max_subagent_turns() -> usize {
    DEFAULT_MAX_SUBAGENT_TURNS
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

fn default_memory_auto_populate() -> bool {
    true
}

fn default_allow_sampling_unattended() -> bool {
    false
}

fn default_doom_loop_max_hits() -> usize {
    1
}

fn default_doom_loop_action() -> String {
    "auto_plan".to_string()
}

// WO 39.2: load `.mcp.json` from the project root by default. The file
// is attacker-controllable (a cloned repo can ship one), so a per-project
// approval prompt gates the first load — see `mcp_project::load_project_mcp`.
fn default_load_project_mcp_json() -> bool {
    true
}

fn default_plugin_sources() -> HashMap<String, PathBuf> {
    // No default filesystem sources: `kf-plugin` is compiled-in behind the
    // `kf-plugin-tools` feature (WO 29.1), and stratum/kf-budget are folded
    // behind their own features. The shell-plugin tree that used to live at
    // `plugins/kf-plugin/` was deleted in WO 29.9. Users can still add custom
    // v1 (`PluginToolWrapper`) plugin sources via `[plugin_sources]` in config.
    HashMap::new()
}

fn default_enabled_plugins() -> Vec<String> {
    let mut names: Vec<String> = default_plugin_sources().keys().cloned().collect();
    #[cfg(feature = "kf-plugin-tools")]
    names.push("kf-plugin".into());
    #[cfg(feature = "stratum")]
    names.push("stratum".into());
    #[cfg(feature = "budget")]
    names.push("kf-budget".into());
    names.sort();
    names
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolConfig {
    #[serde(default = "default_max_tool_result_chars")]
    pub max_tool_result_chars: usize,
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
    #[serde(default = "default_max_subagent_turns")]
    pub max_subagent_turns: usize,
    #[serde(default = "default_max_plugin_trust")]
    pub max_plugin_trust: kf_plugin_host::TrustTier,
    #[serde(default = "default_reject_on_excess_plugin_trust")]
    pub reject_on_excess_plugin_trust: bool,
    #[serde(default = "default_plugin_signature_validation")]
    pub plugin_signature_validation: bool,
    #[serde(default = "default_plugin_trust_workspace")]
    pub plugin_trust_workspace: bool,
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
    #[serde(default = "default_memory_auto_populate")]
    pub memory_auto_populate: bool,
    #[serde(default = "default_allow_sampling_unattended")]
    pub allow_sampling_unattended: bool,
    #[serde(default = "default_doom_loop_max_hits")]
    pub doom_loop_max_hits: usize,
    #[serde(default = "default_doom_loop_action")]
    pub doom_loop_action: String,
    // WO 39.2: when true, a `.mcp.json` in the project root is parsed and
    // its servers merged into the MCP config (gated by first-load approval).
    #[serde(default = "default_load_project_mcp_json")]
    pub load_project_mcp_json: bool,
    // WO 43.17 / WO 45.61 / WO 46.13: when true, plugins loaded from the
    // data dir or workspace sources must be in the `approved_plugins.json`
    // ledger with a matching content hash. The ledger is layered on top of
    // signature verification — a signed plugin must ALSO be ledger-approved,
    // because the manifest-only signature does not cover the command scripts
    // the manifest points to. Defaults on (matching
    // plugin_signature_validation's default) so the content-hash gate runs
    // in the out-of-the-box config; set to false to opt out.
    #[serde(default = "default_plugin_consent_ledger")]
    pub plugin_consent_ledger: bool,
}

fn default_plugin_signature_validation() -> bool {
    true
}

// WO 46.13: defaults on to match plugin_signature_validation. The WO 45.61
// fix (ledger layers on top of signatures) is inert when the ledger is off;
// defaulting on closes the default-config hole where a signed manifest +
// swapped command script passes both gates.
fn default_plugin_consent_ledger() -> bool {
    true
}

fn default_plugin_trust_workspace() -> bool {
    false
}

fn default_max_plugin_trust() -> kf_plugin_host::TrustTier {
    kf_plugin_host::TrustTier::Shell
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            max_tool_result_chars: default_max_tool_result_chars(),
            mcp_servers: vec![],
            lsp_servers: vec![],
            tool_timeout_secs: default_tool_timeout_secs(),
            max_tool_calls_per_turn: default_max_tool_calls_per_turn(),
            max_persona_turns: default_max_persona_turns(),
            max_continuation_rounds: default_max_continuation_rounds(),
            dry_run: false,
            hooks_dir: None,
            minify_write_side: default_minify_write_side(),
            minify_above_bytes: default_minify_above_bytes(),
            follow_symlinks: false,
            block_binary_reads: false,
            scheduled_bash_auto_approve: false,
            max_concurrent_scheduled_jobs: default_max_concurrent_scheduled_jobs(),
            max_background_tasks: default_max_background_tasks(),
            task_concurrency_mode: default_task_concurrency_mode(),
            max_subagent_turns: default_max_subagent_turns(),
            max_plugin_trust: default_max_plugin_trust(),
            reject_on_excess_plugin_trust: default_reject_on_excess_plugin_trust(),
            plugin_signature_validation: true,
            plugin_trust_workspace: false,
            plugin_public_key_path: None,
            plugin_allowed_env_vars: vec![],
            plugin_sources: default_plugin_sources(),
            enabled_plugins: default_enabled_plugins(),
            disabled_plugins: HashSet::new(),
            stratum_mode: None,
            budget_ceiling: default_budget_ceiling(),
            budget_approaching_ratio: default_budget_approaching_ratio(),
            memory_auto_populate: true,
            allow_sampling_unattended: false,
            doom_loop_max_hits: default_doom_loop_max_hits(),
            doom_loop_action: default_doom_loop_action(),
            load_project_mcp_json: default_load_project_mcp_json(),
            plugin_consent_ledger: true,
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
        assert!(
            cfg.plugin_signature_validation,
            "R7: signature validation default-on"
        );
        assert_eq!(cfg.max_continuation_rounds, 20);
        assert_eq!(cfg.max_tool_calls_per_turn, 200);
        assert_eq!(cfg.tool_timeout_secs, Some(120));
        assert_eq!(cfg.doom_loop_max_hits, 1);
        assert_eq!(cfg.doom_loop_action, "auto_plan");
        assert_eq!(
            cfg.max_subagent_turns, 32,
            "WO 48.34: model-supplied max_turns ceiling defaults to 32"
        );
        assert!(
            !cfg.allow_sampling_unattended,
            "sampling must default to approval-gated (deny in headless)"
        );
        assert!(
            cfg.load_project_mcp_json,
            "WO 39.2: project .mcp.json discovery defaults on"
        );
        assert!(
            cfg.plugin_consent_ledger,
            "WO 46.13: plugin consent ledger defaults on (matching plugin_signature_validation)"
        );
    }

    #[test]
    fn tool_config_toml_overrides_defaults() {
        let toml = r#"
stratum_mode = "lite"
budget_ceiling = 50000
budget_approaching_ratio = 0.9
minify_above_bytes = 1024
doom_loop_action = "halt"
"#;
        let cfg: ToolConfig = toml::from_str(toml).expect("parse");
        assert_eq!(cfg.stratum_mode.as_deref(), Some("lite"));
        assert_eq!(cfg.budget_ceiling, 50_000);
        assert!((cfg.budget_approaching_ratio - 0.9).abs() < f64::EPSILON);
        assert_eq!(cfg.minify_above_bytes, 1024);
        assert_eq!(cfg.doom_loop_action, "halt");
    }

    #[test]
    fn tool_config_toml_omitted_uses_defaults() {
        let toml = "";
        let cfg: ToolConfig = toml::from_str(toml).expect("parse");
        assert_eq!(cfg.budget_ceiling, 200_000);
        assert!((cfg.budget_approaching_ratio - 0.8).abs() < f64::EPSILON);
        assert!(cfg.stratum_mode.is_none());
        assert!(cfg.plugin_signature_validation, "R7: serde default-on");
        assert!(
            cfg.plugin_consent_ledger,
            "WO 46.13: serde default-on (matches plugin_signature_validation)"
        );
    }
}
