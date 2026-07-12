import type { SchemaVersion } from "./types.js";

export type VerifierStatus = "pass" | "fail" | "error" | "skipped";

export interface VerifyLintEvent {
  kind: "verify.lint";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    status?: VerifierStatus;
    error?: string;
    errors: number;
    warnings: number;
    suppressed?: number;
    filesScanned: number;
    durationMs: number;
    details: Array<{
      file: string;
      line: number;
      rule: string;
      severity?: string;
      category?: string;
      message: string;
    }>;
  };
  source?: string;
  timestamp: string;
}

export interface VerifyTypesEvent {
  kind: "verify.types";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    status?: VerifierStatus;
    error?: string;
    errors: number;
    durationMs: number;
    details: Array<{ file: string; line: number; code: string; message: string }>;
  };
  source?: string;
  timestamp: string;
}

export interface VerifySecurityEvent {
  kind: "verify.security";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    status?: VerifierStatus;
    error?: string;
    findings: number;
    critical: number;
    high: number;
    filesScanned: number;
    durationMs: number;
    details: Array<{ file: string; line: number; rule: string; severity: string; message: string }>;
  };
  source?: string;
  timestamp: string;
}

export interface VerifyImportsEvent {
  kind: "verify.imports";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    status?: VerifierStatus;
    error?: string;
    findings: number;
    warnings: number;
    info: number;
    filesScanned: number;
    durationMs: number;
    details: Array<{
      file: string;
      line: number;
      rule: string;
      oldName: string;
      newName: string;
      message: string;
    }>;
  };
  source?: string;
  timestamp: string;
}

export interface StateChangesEvent {
  kind: "state.changes";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    filesChanged: number;
    paths: string[];
    insertions: number;
    deletions: number;
    durationMs: number;
    warning?: string;
  };
  source?: string;
  timestamp: string;
}

export interface StateGraphEvent {
  kind: "state.graph";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    status?: VerifierStatus;
    error?: string;
    edgeCount: number;
    newEdges: number;
    brokenEdges: number;
    cycles: number;
    durationMs: number;
  };
  source?: string;
  timestamp: string;
}

export interface EventBusOverflowEvent {
  kind: "event.bus.overflowed";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  bufferSize: number;
  bufferCapacity: number;
  originalEventKind?: string;
  originalStreamId?: string;
  source?: string;
  timestamp: string;
}

export interface ArtifactBlockedEvent {
  kind: "artifact.blocked";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    blockedPaths: Array<{ path: string; reason: string }>;
    parseWarnings?: Array<{ line: number; warning: string }>;
  };
  source?: string;
  timestamp: string;
}

export interface ArtifactUnterminatedEvent {
  kind: "artifact.unterminated";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    warnings: string[];
  };
  source?: string;
  timestamp: string;
}

export interface ArtifactTruncatedEvent {
  kind: "artifact.truncated";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    finishReason: string;
    warnings: string[];
  };
  source?: string;
  timestamp: string;
}

export interface ArtifactEmittedEvent {
  kind: "artifact.emitted";
  schemaVersion: SchemaVersion;
  sequence: number;
  streamId: string;
  taskId: string;
  value: {
    filesWritten: number;
    totalBytes: number;
    files: Array<{
      path: string;
      sha256: string;
      bytes: number;
      beforeHash: string | null;
      existed: boolean;
    }>;
    language: string;
  };
  confidence?: number;
  source?: string;
  timestamp: string;
}

export type KirkForgeEvent =
  | VerifyLintEvent
  | VerifyTypesEvent
  | VerifySecurityEvent
  | VerifyImportsEvent
  | StateChangesEvent
  | StateGraphEvent
  | EventBusOverflowEvent
  | ArtifactBlockedEvent
  | ArtifactUnterminatedEvent
  | ArtifactTruncatedEvent
  | ArtifactEmittedEvent;

export type KirkForgeEventKind = KirkForgeEvent["kind"];
