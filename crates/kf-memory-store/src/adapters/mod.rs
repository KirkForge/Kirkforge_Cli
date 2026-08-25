//! `MemoryAdapter` trait — the storage backend contract. Port of
//! `memory-palace/src/types.ts::MemoryAdapter`.
//!
//! TS models run/emission storage as optional duck-typed methods
//! (`adapter.writeRun?()`). The Rust port uses default trait impls:
//! write-side defaults are no-ops (`Ok(())`), read/transactional defaults
//! return `Ok(None)` / `Ok(false)` so the store can detect "no specialized
//! path" and fall back to generic `MemoryObject` storage. `SqliteAdapter`
//! overrides the specialized methods; `FileAdapter` / `InMemoryAdapter`
//! accept the defaults.

use anyhow::Result;

use crate::types::{EmissionRow, MemoryObject, MemoryQuery, MemoryStats, RunRow};

pub mod file;
pub mod in_memory;
pub mod sqlite;

pub trait MemoryAdapter: Send + Sync {
    fn write(&self, obj: &MemoryObject) -> Result<()>;
    fn read(&self, id: &str) -> Result<Option<MemoryObject>>;
    fn delete(&self, id: &str) -> Result<()>;
    fn query(&self, q: &MemoryQuery) -> Result<Vec<MemoryObject>>;
    fn stats(&self) -> Result<MemoryStats>;
    fn persist(&self) -> Result<()> {
        Ok(())
    }

    // ── Specialized run/emission storage ──────────────────────────────────
    // Default = no-op (adapter only supports generic MemoryObject storage).
    // SqliteAdapter overrides with INSERT INTO runs/emissions. The store
    // ALWAYS follows up with a generic MemoryObject write for back-compat,
    // matching TS `store.writeRunRecord`.
    fn write_run_row(&self, _run: &RunRow) -> Result<()> {
        Ok(())
    }
    fn write_emission_row(&self, _emission: &EmissionRow) -> Result<()> {
        Ok(())
    }
    /// Atomic run + emissions write. Returns `Ok(true)` when handled
    /// transactionally (store returns early); `Ok(false)` when the adapter
    /// needs the sequential fallback.
    fn write_run_and_emissions_tx(
        &self,
        _run: &RunRow,
        _emissions: &[EmissionRow],
    ) -> Result<bool> {
        Ok(false)
    }
    fn query_run_rows(&self, _limit: usize) -> Result<Option<Vec<RunRow>>> {
        Ok(None)
    }
    fn query_emission_rows_for_run(&self, _run_id: &str) -> Result<Option<Vec<EmissionRow>>> {
        Ok(None)
    }
    fn schema_version(&self) -> Option<i64> {
        None
    }
}
