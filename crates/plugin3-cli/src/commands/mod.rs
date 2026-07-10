//! Clap subcommand handlers — budget, report, config, store, init.
//! Per ADR-0002 § Crate layout (`crates/plugin3-cli/src/commands/`).
//!
//! ponytail: each subcommand lives in its own file because the
//! handlers have grown past 50 lines and have distinct
//! dependencies (report pulls `UsageRecord`, config pulls Paths,
//! init pulls Host + `HookConfig`). Splitting makes the dependency
//! surface obvious to a contributor who only wants to touch one
//! of them.

pub(crate) mod budget;
pub(crate) mod config;
pub(crate) mod init;
pub(crate) mod report;
pub(crate) mod store;
