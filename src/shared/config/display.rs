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

fn default_memory_show_in_status() -> bool {
    true
}

fn default_theme() -> String {
    "default".to_string()
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
    #[serde(default = "default_memory_show_in_status")]
    pub memory_show_in_status: bool,
    /// TUI color theme name. Built-ins: `"default"`, `"dark"`,
    /// `"light"`, `"monokai"`. Unknown values fall back to `"default"`.
    /// Live-switchable via the `/theme` slash command (WO 27.6).
    #[serde(default = "default_theme")]
    pub theme: String,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            memory_enabled: default_memory_enabled(),
            memory_max_tokens: default_memory_max_tokens(),
            memory_top_n: default_memory_top_n(),
            memory_auto_populate: default_memory_auto_populate(),
            memory_show_in_status: default_memory_show_in_status(),
            theme: default_theme(),
        }
    }
}
