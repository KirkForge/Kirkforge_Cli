mod display;
mod model;
mod security;
mod session;
mod tools;

pub use display::DisplayConfig;
pub use model::ModelConfig;
pub use security::SecurityConfig;
pub use session::SessionConfig;
pub use tools::ToolConfig;

use serde::{Deserialize, Serialize};

/// Total number of pub fields across all Config sub-structs.
///
/// When you add a field to ModelConfig, SecurityConfig, ToolConfig,
/// SessionConfig, or DisplayConfig, **increment this constant** and add
/// handling in both `merge_toml_into_config` and `apply_env_overrides`.
/// If either site is missing the new field, the drift-guard test will fail.
///
/// Breakdown:
///   ModelConfig    32  (31 direct + subagent_provider sub-struct handle)
///   SecurityConfig 22  (19 direct + 3 sub-struct handles)
///   ToolConfig     33
///   SessionConfig   8
///   DisplayConfig   7
///   Note: 1 field (seed) has #[serde(skip_serializing)], so serde
///   produces 97 keys; ToolConfig.memory_auto_populate and
///   DisplayConfig.memory_auto_populate flatten to the same JSON key,
///   dropping 1 more. The drift-guard test accounts for both.
//
// WO 27.2-R2: corrected from 103 → 98. The const had drifted +5 over
// the actual struct count (doc claimed 33/20/33/9/8; reality is
// 31/19/33/8/7). The "known-broken" drift-guard test hid the drift
// for several WO cycles; un-ignoring it forced the correction.
// WO 27.1: bumped 98 → 99 (added SecurityConfig.landlock_extra_paths).
// WO 30.0.6: bumped 99 → 100 (added ModelConfig.subagent_provider).
// WO 32.18: bumped 100 → 102 (added SecurityConfig.bash_require_allowlist,
//           SecurityConfig.bash_allowlist).
// WO 32.13: bumped 102 → 103 (added ModelConfig.streaming_timeout_secs).
// WO 39.2: bumped 103 → 104 → 105 (added ToolConfig.load_project_mcp_json).
// WO 41.1: bumped 105 → 106 (added SessionConfig.auto_apply_patch).
// WO 43.17: bumped 106 → 107 (added ToolConfig.plugin_consent_ledger).
// WO 44.22: bumped 107 → 108 (added ModelConfig.anthropic_api_base).
pub const CONFIG_FIELD_COUNT: usize = 108;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(flatten)]
    pub model: ModelConfig,
    #[serde(flatten)]
    pub security: SecurityConfig,
    #[serde(flatten)]
    pub tools: ToolConfig,
    #[serde(flatten)]
    pub session: SessionConfig,
    #[serde(flatten)]
    pub display: DisplayConfig,
}
