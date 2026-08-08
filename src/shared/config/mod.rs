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
///   ModelConfig    33
///   SecurityConfig 20  (17 direct + 3 sub-struct handles)
///   ToolConfig     32
///   SessionConfig   9
///   DisplayConfig   5
///   Note: 1 field (seed) has #[serde(skip_serializing)], so serde
///   produces 98 keys. The drift-guard test accounts for this.
pub const CONFIG_FIELD_COUNT: usize = 99;

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
