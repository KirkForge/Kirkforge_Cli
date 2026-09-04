//! Library crate for the `kf-code` CLI.
//!
//! The binary in `src/main/mod.rs` is a thin wrapper that consumes this library.
//! Exposing the internal modules as a library lets `benches/` and `tests/`
//! targets exercise real parser/executor code without duplication.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod adapters;
pub mod cli;
// daemon-gated (WO 47.12): src/daemon/** + kf-rbac are excised from
// default builds; compiled in only with --features daemon.
#[cfg(feature = "daemon")]
pub mod daemon;
pub mod jobs;
pub mod line_mode;
pub mod session;
pub mod shared;
pub mod tools;
pub mod tui;
