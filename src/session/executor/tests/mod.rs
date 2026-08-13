//! Executor unit tests.
//!
//! Split into feature-aligned sub-modules (WO 15.5). The shared test
//! helpers (`MockAdapter`, `MockTool`, `make_executor`, etc.) live in
//! [`common`]; each sub-module imports them via `use super::common::*;`.
//! Test bodies are moved verbatim — no logic change.

mod approval;
mod common;
mod coverage_gaps;
mod dispatch;
mod loop_;
mod scout;
mod turn;
mod verifier_cross;
mod wiremock_integration;
