//! E2E test harness — integration test entry point.
//!
//! Run with: `cargo test --test e2e -- --test-threads=1`
//!
//! The harness spawns the real `kf-code` binary against an in-test mock
//! provider.  Isolated envs ensure tests don't collide with each other or
//! with the user's real config.

#![allow(clippy::len_zero)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::useless_conversion)]
#![allow(clippy::io_other_error)]
#![allow(dead_code)]

mod fixtures;
mod harness;

mod scenarios;

// The `harness` module re-exports all submodules.  The `scenarios`
// module contains individual regression tests, each in its own file.
// Cargo discovers `tests/e2e.rs` as an integration test crate and
// runs all `#[test]` / `#[tokio::test]` functions found in it and
// its descendants.
