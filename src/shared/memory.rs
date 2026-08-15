//! Memory port — the seam `tools/` uses to store prompt-injection facts
//! without importing `session/`.
//!
//! WO 32.8 (ADR-073 residual): `session::memory` owns the implementation
//! (it depends on `session::data_dir()` and `session::prompt::count_tokens`).
//! This module re-exports the store types and helpers so `tools/` depends
//! on `shared`, not `session`. Same shim pattern WO 28.1 used for
//! `access` / `bash_safety` / `undo`.
//!
//! DEFERRED (ADR-073 §"Why relocation"): a `MemoryStore` port trait.
//! Single impl today (`session::memory::MemoryStore`), no second
//! consumer. Introduce when a second impl appears.
//!
//! Note: this is the prompt-injection fact store, NOT `crates/kf-memory-store`
//! (routing-oriented, WO 29.6). The `remember` tool uses this store.

pub use crate::session::memory::{parse_frontmatter, slugify_description, MemoryFact, MemoryStore};
