use serde::{Deserialize, Serialize};

fn default_memory_enabled() -> bool {
    true
}

fn default_memory_max_tokens() -> usize {
    500
}

fn default_memory_top_n() -> usize {
    10
}

fn default_memory_auto_populate() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayConfig {
    #[serde(default = "default_memory_enabled")]
    pub memory_enabled: bool,
    #[serde(default = "default_memory_max_tokens")]
    pub memory_max_tokens: usize,
    #[serde(default = "default_memory_top_n")]
    pub memory_top_n: usize,
    #[serde(default = "default_memory_auto_populate")]
    pub memory_auto_populate: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            memory_enabled: default_memory_enabled(),
            memory_max_tokens: default_memory_max_tokens(),
            memory_top_n: default_memory_top_n(),
            memory_auto_populate: default_memory_auto_populate(),
        }
    }
}
