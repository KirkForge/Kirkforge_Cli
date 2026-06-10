//! Stub module — deepseek's in-flight work referenced `ScheduleConfig` and
//! `Scheduler` from `main.rs` but the full implementations weren't included
//! in the saved work. Added at push time with placeholder structs that
//! match the field shape main.rs expects, so `cargo check` passes.
//!
//! **Review.md gap #1 — the real cron-driven scheduler is a follow-up.**
//! Until then, `Scheduler::run` returns an explicit `Err` so the CLI can't
//! silently exit 0 after doing nothing. The `kirkforge schedule
//! --print-config` path still works because it short-circuits in
//! `main.rs` before `Scheduler::new`/`run` is called. See the message on
//! the error for the tracking reference.

#![allow(dead_code, unused_imports)]
use anyhow::{anyhow, Result};
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
        // Review.md gap #1: this used to return Ok(()) and silently
        // exit 0 after doing nothing. That was worse than not having
        // a scheduler at all — a user who ran `kirkforge schedule
        // --config my-schedule.toml` expecting an autonomous loop
        // would see a clean exit and assume it was working.
        //
        // Until the real cron-driven scheduler ships, surface the
        // gap explicitly. The CLI side (main.rs) still does
        // `sched.run(shutdown).await?`, so this error propagates to
        // the user with a clear message and a non-zero exit code.
        Err(anyhow!(
            "scheduler is not implemented yet (review.md gap #1). \
             Use `kirkforge schedule --print-config` to inspect your \
             config, or run `kirkforge run` for the interactive TUI."
        ))
    }
}
