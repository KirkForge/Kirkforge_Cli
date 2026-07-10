//! Pipeline orchestration: stage state machine, checkpoints, decision log.

pub mod checkpoint;
pub mod decision;
pub mod promise;
pub mod slideshow_risk;
pub mod variation_checker;

pub use checkpoint::{Checkpoint, Stage};
pub use decision::{Decision, DecisionLog};
pub use promise::{PromiseRules, PromiseType};
pub use slideshow_risk::{score_slideshow_risk, RiskVerdict};
pub use variation_checker::{check_scene_variation, SceneView, VariationReport, Verdict};

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::compose::Composition;
use crate::error::Result;
use crate::pipelines::Pipeline;
use crate::tools::ToolRegistry;

/// Run a pipeline end-to-end. `project_dir` holds the per-project workspace
/// (artifacts + checkpoints). Each stage writes its own artifact and a
/// checkpoint, then either advances or pauses for human approval.
pub async fn run_pipeline(
    pipeline: &dyn Pipeline,
    project_dir: &Path,
    registry: &ToolRegistry,
) -> Result<()> {
    let mut checkpoint = Checkpoint::load_or_init(project_dir, pipeline.name())?;
    tracing::info!(pipeline = pipeline.name(), "starting pipeline");

    for stage in pipeline.stages() {
        let stage = *stage;
        if checkpoint.is_complete(stage) {
            tracing::info!(?stage, "skipping (already complete)");
            continue;
        }
        tracing::info!(?stage, "running stage");
        let artifact = pipeline.run_stage(stage, project_dir, registry).await?;
        checkpoint.complete(stage, artifact)?;
        checkpoint.save(project_dir)?;
    }

    // Final render if a Composition was produced.
    let comp_path = project_dir.join("artifacts").join("composition.json");
    if comp_path.exists() {
        let comp: Composition = serde_json::from_str(&std::fs::read_to_string(&comp_path)?)?;
        let out = project_dir.join("render").join("final.mp4");
        std::fs::create_dir_all(out.parent().unwrap())?;
        crate::compose::render::render_composition(&comp, &out).await?;
        tracing::info!(path = %out.display(), "rendered final video");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub pipeline: String,
    pub promise: PromiseType,
    pub render_runtime: RenderRuntime,
    pub title: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RenderRuntime {
    #[default]
    Ffmpeg,
    Remotion,    // reserved — not used in this build
    HyperFrames, // reserved — not used in this build
}
