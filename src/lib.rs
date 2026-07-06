//! Library crate for the `kirkforge` CLI.
//!
//! The binary in `src/main.rs` is a thin wrapper that consumes this library.
//! Exposing the internal modules as a library lets `benches/` and `tests/`
//! targets exercise real parser/executor code without duplication.

pub mod adapters;
pub mod daemon;
pub mod line_mode;
pub mod session;
pub mod shared;
pub mod tools;
pub mod tui;
