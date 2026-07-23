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
