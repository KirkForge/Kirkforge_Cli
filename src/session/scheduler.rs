//! Stub module — deepseek's in-flight work referenced `ScheduleConfig` and
//! `Scheduler` from `main.rs` but the full implementations weren't included
//! in the saved work. Added at push time with placeholder structs that
//! match the field shape main.rs expects, so `cargo check` passes. The
//! real cron-driven scheduler is a follow-up.

#![allow(dead_code, unused_imports)]
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScheduleBlock {
    pub cron: String,
    pub work_budget_secs: u64,
    pub idle_timeout_secs: u64,
    pub token_cap: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionBlock {
    pub model: String,
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
}

/// The shape `main.rs` calls `ScheduleConfig::load` and then accesses
/// `cfg.schedule.cron` / `cfg.session.model` / `resolved_*_dir()` on.
/// Stub: returns a default-initialized config. Real implementation
/// would parse a TOML file at `path`.
pub struct ScheduleConfig {
    pub schedule: ScheduleBlock,
    pub session: SessionBlock,
    pub project_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl ScheduleConfig {
    pub fn load(_path: &Path) -> Result<Self> {
        Ok(Self {
            schedule: ScheduleBlock::default(),
            session: SessionBlock::default(),
            project_dir: PathBuf::new(),
            state_dir: PathBuf::new(),
        })
    }

    pub fn resolved_project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn resolved_state_dir(&self) -> &Path {
        &self.state_dir
    }
}

/// Stub: the real implementation would spawn a tokio task that
/// triggers the configured cron job. Returns `Ok(())` immediately.
pub struct Scheduler {
    pub config: ScheduleConfig,
}

impl Scheduler {
    pub fn new(config: ScheduleConfig) -> Result<Self> {
        Ok(Self { config })
    }

    pub async fn run(self, _shutdown: Arc<AtomicBool>) -> Result<()> {
        Ok(())
    }
}
