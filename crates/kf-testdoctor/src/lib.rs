//! kf-testdoctor — test-performance doctor for Rust workspaces.
//!
//! Library API: profile, classify, partition, suggest, gaps, diagnose.
//! See `docs/ideas/test-doctor.md` for the design.

pub mod apply;
pub mod classify;
pub mod diagnose;
pub mod flaky;
pub mod gaps;
pub mod partition;
pub mod profile;
pub mod suggest;
