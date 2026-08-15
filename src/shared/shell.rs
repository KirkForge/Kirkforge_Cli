//! Shell I/O port — the seam `tools/` uses to run foreground shell commands
//! and reach the global background-job registry without importing
//! `session/`.
//!
//! WO 32.8 (ADR-073 residual): `session::bash_runner` and
//! `session::bash_jobs` own the *implementations* (they need
//! `process_group`, landlock, seccomp, `model_command_path`, `access`).
//! This module re-exports the types and the single free-function entry
//! points so `tools/` depends on `shared`, not `session`. Same shim
//! pattern WO 28.1 used for `access` / `bash_safety` / `undo`.
//!
//! DEFERRED (ADR-073 §"Why relocation"): a `ShellRunner` port trait.
//! Single impl today (`session::bash_runner::run_shell_with_token`),
//! no second consumer — a trait would be a dynamic-dispatch seam with
//! no benefit. Introduce when a second impl (e.g. a test fake) appears.

pub use crate::session::bash_jobs::{global_registry, BashJob, BashJobRegistry, JobStatus};
pub use crate::session::bash_runner::{
    is_timeout_marker, run_shell, run_shell_with_token, ShellError, ShellOutput,
};

// PTY support is feature-gated and lives under session::bash_runner::pty
// (it depends on session::executor::TurnEvent). Re-export so tools/bash.rs
// can reach it via shared::shell without a crate::session:: import.
#[cfg(feature = "pty")]
pub use crate::session::bash_runner::pty::{run_with_pty, PtyResult};
