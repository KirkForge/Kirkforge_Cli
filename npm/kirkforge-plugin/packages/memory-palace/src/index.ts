// Thin barrel — types and named re-exports.

export type {
  BackupMetadata,
  MemoryObject,
  MemoryQuery,
  MemoryStats,
  MemoryAdapter,
  Recommendation,
  RoutingCase,
  RoutingBias,
  EmittedFileRecord,
  RunRecord,
  TaskObservationInput,
  RunRow,
  EmissionRow,
} from "./types.js";
// Re-export Recommendation as a type-only to avoid accidental runtime import of types module.
export type { MemoryStoreOptions } from "./store.js";

export { InMemoryAdapter } from "./adapters/in-memory.js";
export { FileAdapter } from "./adapters/file.js";
export { MemoryStore } from "./store.js";
