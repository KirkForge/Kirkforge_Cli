// Memory-palace shared type surface. The package barrel re-exports these
// from here so consumers see a single, stable public surface.

export type { BackupMetadata } from "./sqlite-adapter.js";

export interface MemoryObject {
  id: string;
  kind: string;
  taskId: string;
  runId?: string;
  timestamp: string;
  description: string;
  properties: Record<string, unknown>;
  tags: string[];
}

export interface MemoryQuery {
  kind?: string;
  tags?: string[];
  limit?: number;
  since?: string;
}

export interface MemoryStats {
  totalObjects: number;
  lastWrite: string;
}

export interface Recommendation {
  mode: string;
  model: string;
  confidence: number;
  evidence: number;
  expectedTokens: number;
  score: number;
  routingBias?: RoutingBias;
}

export interface RoutingCase {
  taskFamily: string;
  language: string;
  mode: string;
  model: string;
  outcome: "pass" | "fail" | "error";
  outcomeClass?: "pass" | "task_fail" | "validator_error" | "tool_error" | "escalated" | "unknown";
  sourceOfTruth?: "task-validator" | "verifier";
  reason: string;
  tokens: number;
  durationMs: number;
  similarity: number;
  truthWeight: number;
}

export interface RoutingBias {
  prefer: string[];
  avoid: string[];
  confidence: number;
  influence: number;
  evidence: number;
  similarCases: RoutingCase[];
}

export interface EmittedFileRecord {
  id?: string;
  path: string;
  sha256: string;
  bytes: number;
  beforeHash: string | null;
  existed: boolean;
  timestamp?: string;
}

export interface RunRecord {
  runId: string;
  taskId: string;
  description: string;
  language: string;
  taskFamily?: string;
  mode: string;
  model: string;
  providerKey: string;
  providerType: string;
  baseUrl?: string;
  outcome: "pass" | "fail" | "error";
  outcomeClass: "pass" | "task_fail" | "validator_error" | "tool_error" | "escalated" | "unknown";
  routingLesson: "reward" | "punish" | "neutral";
  finalVerdict: "pass" | "fail" | "error" | "unknown";
  sourceOfTruth: "task-validator" | "verifier";
  finalAction: "accept" | "escalate";
  tokens: number;
  durationMs: number;
  turns: number;
  validatorDurationMs: number;
  verifierOverall?: string;
  filesEmitted: number;
  totalBytesEmitted: number;
  emissions: EmittedFileRecord[];
  emissionIds: string[];
  timestamp: string;
}

export interface TaskObservationInput {
  taskId: string;
  description: string;
  taskFamily?: string;
  language: string;
  runtime?: string;
  mode: string;
  model: string;
  providerKey?: string;
  providerType?: string;
  baseUrl?: string;
  promptShape?: string;
  verifierOverall?: string;
  finalAction?: "accept" | "escalate";
  taskPass?: boolean | null;
  outcome?: "pass" | "fail" | "error";
  outcomeClass?: "pass" | "task_fail" | "validator_error" | "tool_error" | "escalated" | "unknown";
  routingLesson?: "reward" | "punish" | "neutral";
  reason?: string;
  tokens: number;
  durationMs: number;
  turns?: number;
  finalVerdict?: "pass" | "fail" | "error" | "unknown";
  sourceOfTruth?: "task-validator" | "verifier";
  taskValidation?: {
    status: string;
    validator: string;
    reason?: string;
    durationMs?: number;
    details?: unknown;
  };
  emissions?: EmittedFileRecord[];
  emissionIds?: string[];
  validatorDurationMs?: number;
}

export interface MemoryAdapter {
  write(obj: MemoryObject): Promise<Result<void, Error>>;
  read(id: string): Promise<Result<MemoryObject | null, Error>>;
  query(q: MemoryQuery): Promise<Result<MemoryObject[], Error>>;
  stats(): Promise<Result<MemoryStats, Error>>;
  /** Specialized run/emission methods for SQLite-backed adapters. */
  writeRun?(run: RunRow): void;
  writeEmission?(emission: EmissionRow): void;
  queryRuns?(limit?: number): Array<Record<string, unknown>>;
  queryEmissionsForRun?(runId: string): Array<Record<string, unknown>>;
  writeRunAndEmissions?(run: RunRow, emissions: EmissionRow[]): void;
  /** Schema version for migration tracking. */
  schemaVersion?(): number | null;
  /** Persist in-memory state to durable storage. No-op for in-memory adapters. */
  persist(): Promise<void>;
}

import type { Result } from "@kirkforge/core-types";

/** Normalized run row shape for SQLite specialized adapters. */
export interface RunRow {
  runId: string;
  taskId: string;
  description: string;
  language: string;
  taskFamily?: string;
  mode: string;
  model: string;
  providerKey: string;
  providerType: string;
  baseUrl?: string;
  outcome: string;
  outcomeClass: string;
  routingLesson: string;
  finalVerdict: string;
  sourceOfTruth: string;
  finalAction: string;
  tokens: number;
  durationMs: number;
  turns: number;
  validatorDurationMs: number;
  verifierOverall?: string;
  filesEmitted: number;
  totalBytesEmitted: number;
  emissionIds: string[];
  timestamp: string;
}

/** Normalized emission row shape for SQLite specialized adapters. */
export interface EmissionRow {
  id: string;
  runId: string;
  taskId: string;
  turn: number;
  path: string;
  sha256: string;
  bytes: number;
  beforeHash: string | null;
  existed: boolean;
  timestamp: string;
}
