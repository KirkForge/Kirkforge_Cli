#![forbid(unsafe_code)]
// `clippy::literal_string_with_formatting_args` has a known false positive on
// crate-level doc comments that contain the word "Stratum", so silence it
// locally while keeping the rest of the nursery lints enabled.
#![allow(clippy::literal_string_with_formatting_args)]

//! Core primitives for the Stratum compression and rules pipeline.
//!
//! This crate defines content types, pipeline modes, the orchestrator, the
//! embedded config schema, and a pluggable in-memory offload store. It is
//! intentionally dependency-light and usable as a library by host adapters and
//! the CLI alike.

/// Embedded TOML config and the [`Ratio`](config::Ratio) type.
pub mod config;
/// Content type detection and host-aware tag filtering.
pub mod content;
/// Pipeline [`Mode`](mode::Mode) enum and helpers.
pub mod mode;
/// [`CompressionPipeline`](pipeline::CompressionPipeline) and [`CompressionContext`](pipeline::CompressionContext).
pub mod pipeline;
/// Pluggable [`OffloadStore`](store::OffloadStore) backends.
pub mod store;
