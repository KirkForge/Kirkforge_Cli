//! `memory` — routing-oriented memory system (folded from the former
//! `kf-memory-store` crate, WO 47.4). Port of
//! `@kirkforge/memory-palace` (WO 29.6).
//!
//! Two pieces: a [`MemoryStore`] facade (orchestrator-friendly write/recall
//! surface) and a [`MemoryAdapter`] backend contract with three impls
//! (`InMemoryAdapter`, `FileAdapter`, `SqliteAdapter`).
//!
//! **Not** to be confused with `src/session/memory/` (prompt-injection
//! facts, different purpose). This crate stores routing observations,
//! run records, and emission records for the empirical recommendation
//! engine in the `routing` module.

pub mod adapters;
pub mod store;
pub mod time;
pub mod types;

pub use adapters::{
    file::FileAdapter, in_memory::InMemoryAdapter, sqlite::SqliteAdapter, MemoryAdapter,
};
pub use store::{Decomposition, MemoryStore, MemoryStoreOptions};
pub use types::{
    BackupMetadata, BackupRowCount, EmissionRow, EmittedFileRecord, MemoryObject, MemoryQuery,
    MemoryStats, RunRecord, RunRow, TaskObservationInput,
};
