#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

//! Host adapter helpers for the Stratum pipeline.
//!
//! This crate provides the canonical ruleset filter used by host integrations
//! to select content based on the active pipeline mode.

/// Canonical ruleset filter driven by pipeline [`Mode`].
pub mod rules;

use kirkstratum_core::mode::Mode;

const CANONICAL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/docs/rules/CANONICAL.md"
));

/// Return the canonical ruleset filtered for `mode`.
///
/// # Examples
///
/// ```
/// use kirkstratum_core::mode::Mode;
/// use kirkstratum_hosts::build_rules;
///
/// let rules = build_rules(Mode::Off);
/// assert!(rules.contains("Ship the smallest change"));
/// ```
#[must_use]
pub fn build_rules(mode: Mode) -> String {
    rules::filter_by_mode(CANONICAL, mode)
}
