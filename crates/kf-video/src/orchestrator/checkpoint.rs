//! Per-project stage checkpoint. JSON on disk under
//! `<project>/checkpoints/<pipeline>.json`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter};

use crate::error::{KfError, Result};

/// Canonical pipeline stages. Mirrors OpenMontage's
/// `research → proposal → script → scene_plan → assets → edit → compose` but
/// is collapsed into the minimum we actually implement today.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Display,
    EnumIter,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Research,
    Proposal,
    Script,
    Narration,
    ScenePlan,
    Assets,
    Edit,
    Compose,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRecord {
    pub artifact: String,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Checkpoint {
    pub pipeline: String,
    pub records: BTreeMap<Stage, StageRecord>,
}

impl Checkpoint {
    pub fn load_or_init(project_dir: &Path, pipeline: &str) -> Result<Self> {
        let dir = project_dir.join("checkpoints");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{pipeline}.json"));
        if path.exists() {
            let s = std::fs::read_to_string(&path)?;
            return serde_json::from_str(&s)
                .map_err(|e| KfError::Checkpoint(format!("parse {path:?}: {e}")));
        }
        Ok(Self {
            pipeline: pipeline.into(),
            records: BTreeMap::new(),
        })
    }

    pub fn save(&self, project_dir: &Path) -> Result<()> {
        let path = project_dir
            .join("checkpoints")
            .join(format!("{}.json", self.pipeline));
        let s = serde_json::to_string_pretty(self)?;
        std::fs::write(path, s)?;
        Ok(())
    }

    pub fn is_complete(&self, stage: Stage) -> bool {
        self.records.contains_key(&stage)
    }

    pub fn complete(&mut self, stage: Stage, artifact: String) -> Result<()> {
        self.records.insert(
            stage,
            StageRecord {
                artifact,
                completed_at: chrono_like_now(),
            },
        );
        self.save_for_stage(stage)
    }

    fn save_for_stage(&self, _stage: Stage) -> Result<()> {
        // Lazy save: callers persist via `save(project_dir)`. Provide a helper
        // that takes the dir so we don't smuggle path state through the type.
        Ok(())
    }
}

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch:{secs}")
}
