//! Pipeline trait + built-in pipelines.
//!
//! ponytail: pipelines live as Rust code, not YAML, so they're type-checked.
//! When you need 10+ pipelines, swap to serde_yaml of the YAML form.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::orchestrator::Stage;
use crate::tools::ToolRegistry;

pub mod animated_explainer;
pub mod brief;
pub mod cinematic;
pub mod screen_demo;

pub use animated_explainer::AnimatedExplainer;
pub use cinematic::Cinematic;
pub use screen_demo::ScreenDemo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    AnimatedExplainer,
    Cinematic,
    ScreenDemo,
}

impl Kind {
    pub fn all() -> &'static [Kind] {
        &[Kind::AnimatedExplainer, Kind::Cinematic, Kind::ScreenDemo]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AnimatedExplainer => "animated_explainer",
            Self::Cinematic => "cinematic",
            Self::ScreenDemo => "screen_demo",
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "animated_explainer" => Some(Self::AnimatedExplainer),
            "cinematic" => Some(Self::Cinematic),
            "screen_demo" => Some(Self::ScreenDemo),
            _ => None,
        }
    }
}

#[async_trait]
pub trait Pipeline: Send + Sync {
    fn name(&self) -> &'static str;
    /// One-line description, used by `kf pipelines list`.
    fn description(&self) -> &'static str;
    fn stages(&self) -> &'static [Stage];
    async fn run_stage(
        &self,
        stage: Stage,
        project_dir: &Path,
        registry: &ToolRegistry,
    ) -> Result<String>;
}

pub fn get(kind: Kind) -> Box<dyn Pipeline> {
    match kind {
        Kind::AnimatedExplainer => Box::new(AnimatedExplainer),
        Kind::Cinematic => Box::new(Cinematic),
        Kind::ScreenDemo => Box::new(ScreenDemo),
    }
}

/// ponytail: every built-in pipeline reported here. Used by
/// `kf pipelines list` to render a discoverable catalog without a
/// YAML manifest round-trip.
pub fn all_pipelines() -> Vec<&'static dyn Pipeline> {
    vec![
        &AnimatedExplainer as &'static dyn Pipeline,
        &Cinematic as &'static dyn Pipeline,
        &ScreenDemo as &'static dyn Pipeline,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_pipelines_returns_each_kind_once() {
        let pipes = all_pipelines();
        let mut labels: Vec<&str> = pipes.iter().map(|p| p.name()).collect();
        labels.sort();
        assert_eq!(
            labels,
            vec!["animated_explainer", "cinematic", "screen_demo"]
        );
    }

    #[test]
    fn each_pipeline_has_a_description() {
        for p in all_pipelines() {
            assert!(
                !p.description().is_empty(),
                "{} missing description",
                p.name()
            );
        }
    }

    #[test]
    fn each_pipeline_has_at_least_three_stages() {
        for p in all_pipelines() {
            assert!(p.stages().len() >= 3, "{} has too few stages", p.name());
        }
    }
}
