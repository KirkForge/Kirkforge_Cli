//! `cinematic` — motion-led pipeline. Mirrors OpenMontage's cinematic.yaml.

use std::path::Path;

use async_trait::async_trait;

use crate::error::Result;
use crate::orchestrator::Stage;
use crate::pipelines::Pipeline;
use crate::tools::ToolRegistry;

pub struct Cinematic;

#[async_trait]
impl Pipeline for Cinematic {
    fn name(&self) -> &'static str {
        "cinematic"
    }
    fn description(&self) -> &'static str {
        "Skip narration, use library-driven visuals. Pulls from curated stock/b-roll assets."
    }
    fn stages(&self) -> &'static [Stage] {
        &[
            Stage::Research,
            Stage::Proposal,
            Stage::Script,
            Stage::ScenePlan,
            Stage::Assets,
            Stage::Edit,
            Stage::Compose,
        ]
    }
    async fn run_stage(&self, stage: Stage, dir: &Path, _reg: &ToolRegistry) -> Result<String> {
        let arts = dir.join("artifacts");
        std::fs::create_dir_all(&arts)?;
        let path = arts.join(format!("{stage:?}.json").to_lowercase());
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "kind": "cinematic_stage", "stage": stage,
            }))?,
        )?;
        Ok(path.to_string_lossy().into_owned())
    }
}
