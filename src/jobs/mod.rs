//! Scheduled jobs — cron/one-shot task runner and TUI integration.
//!
//! Session 3 lands persistent scheduled jobs for bash and workflow commands.
//! The daemon and its socket client are Unix-only. The data model (schedule,
//! store, runner) is cross-platform and is used by the TUI slash commands.

// daemon-gated (WO 47.12): the jobd server + its socket client depend on
// crate::daemon, which is compiled in only with --features daemon. The
// cross-platform data model (runner/schedule/store) stays unconditional.
#[cfg(all(unix, feature = "daemon"))]
pub mod client;
#[cfg(all(unix, feature = "daemon"))]
pub mod daemon;
pub mod runner;
pub mod schedule;
pub mod store;

#[cfg(all(unix, feature = "daemon"))]
pub use daemon::run_job_daemon;
pub use schedule::*;
pub use store::{JobStore, RunPaths};
