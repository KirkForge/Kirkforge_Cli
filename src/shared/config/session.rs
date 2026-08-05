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

fn default_compaction_use_llm() -> bool {
    false
}

fn default_compaction_drop_threshold() -> f64 {
    0.5
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
    /// When `true`, microcompaction uses the LLM summarizer when the
    /// heuristic would drop more than `compaction_drop_threshold` of
    /// the content. When `false` (the default), the heuristic summary
    /// is always used.
    #[serde(default = "default_compaction_use_llm")]
    pub compaction_use_llm: bool,
    /// Fraction of content that must be dropped by the heuristic before
    /// the LLM summarizer is tried. Default 0.5 (50%). Only used when
    /// `compaction_use_llm` is `true`.
    #[serde(default = "default_compaction_drop_threshold")]
    pub compaction_drop_threshold: f64,
    /// Maximum number of file stems included in the system prompt context.
    /// Files exceeding `stem_file_cap` bytes are truncated. Defaults to 4096.
    #[serde(default)]
    pub stem_file_cap: Option<usize>,
    /// Seconds to wait for the executor task to shut down gracefully before
    /// aborting it. Defaults to 3 s.
    #[serde(default)]
    pub shutdown_timeout_secs: Option<u64>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            carryover_enabled: default_carryover_enabled(),
            preserve_recent_messages: default_preserve_recent_messages(),
            checkpoint_interval_messages: default_checkpoint_interval_messages(),
            worktree_enabled: false,
            compaction_use_llm: default_compaction_use_llm(),
            compaction_drop_threshold: default_compaction_drop_threshold(),
            stem_file_cap: None,
            shutdown_timeout_secs: None,
        }
    }
}
