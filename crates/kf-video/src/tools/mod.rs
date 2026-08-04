//! Tool trait + registry.
//!
//! Mirrors OpenMontage's `tools/base_tool.py` — every capability is a `Tool`
//! with a stable name, version, tier, input schema, and `invoke` method.
//!
//! ponytail: this exists — keep the trait surface small; one impl per tool.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter};

use crate::error::{KfError, Result};

pub mod analysis;
pub mod audio;
pub mod doctor;
pub mod enhancement;
pub mod providers;
pub mod transcoder;
pub mod video;

use providers::{Elevenlabs, OpenaiImage, Runway, Suno, Veo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumIter, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolTier {
    Core,
    Provider,
    Experimental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumIter, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStability {
    Stable,
    Beta,
    Experimental,
}

/// Result envelope returned by every `Tool::invoke`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Stable artifact name (e.g. `"video.mp4"`, `"manifest.json"`).
    pub artifact: String,
    /// Absolute or project-relative path to the produced file.
    pub path: std::path::PathBuf,
    /// Free-form metadata (durations, provider used, cost, etc.).
    pub meta: serde_json::Value,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn tier(&self) -> ToolTier;
    fn stability(&self) -> ToolStability;
    /// Capabilities this tool exposes (e.g. `"stitch"`, `"crossfade"`).
    fn capabilities(&self) -> &'static [&'static str];
    async fn invoke(
        &self,
        project: &Path,
        op: &str,
        params: serde_json::Value,
    ) -> Result<ToolOutput>;
}

/// In-process registry of all available tools, keyed by `Tool::name()`.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, t: Arc<dyn Tool>) {
        self.tools.insert(t.name(), t);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn require(&self, name: &str) -> Result<Arc<dyn Tool>> {
        self.get(name)
            .ok_or_else(|| KfError::ToolMissing(name.into()))
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.tools.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// Build the registry with all built-in tools registered.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(video::VideoStitch::new()));
        r.register(Arc::new(audio::AudioMixer::new()));
        r.register(Arc::new(analysis::Analyzer::new()));
        r.register(Arc::new(transcoder::Transcoder::new()));
        r.register(Arc::new(enhancement::Enhancer::new()));
        r.register(Arc::new(Veo::new()));
        r.register(Arc::new(Runway::new()));
        r.register(Arc::new(Elevenlabs::new()));
        r.register(Arc::new(Suno::new()));
        r.register(Arc::new(OpenaiImage::new()));
        r
    }
}
