//! Scheduled jobs — cron/one-shot task runner and TUI integration.
//!
//! Session 3 lands persistent scheduled jobs for bash commands. Skill jobs are
//! accepted by the data model and scheduler but are intentionally not
//! executable yet; they record a clear "not implemented" failure.

pub mod client;
pub mod daemon;
pub mod runner;
pub mod schedule;
pub mod store;

pub use daemon::run_job_daemon;
pub use schedule::*;
pub use store::{JobListEntry, JobStore, RunPaths};
