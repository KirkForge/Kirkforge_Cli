use serde::{Deserialize, Serialize};

fn default_carryover_enabled() -> bool {
    true
}

fn default_preserve_recent_messages() -> usize {
    2
}

fn default_checkpoint_interval_messages() -> usize {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_carryover_enabled")]
    pub carryover_enabled: bool,
    #[serde(default = "default_preserve_recent_messages")]
    pub preserve_recent_messages: usize,
    #[serde(default = "default_checkpoint_interval_messages")]
    pub checkpoint_interval_messages: usize,
    #[serde(default)]
    pub worktree_enabled: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            carryover_enabled: default_carryover_enabled(),
            preserve_recent_messages: default_preserve_recent_messages(),
            checkpoint_interval_messages: default_checkpoint_interval_messages(),
            worktree_enabled: false,
        }
    }
}
