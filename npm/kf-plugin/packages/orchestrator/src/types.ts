import type { DelegationMode, DelegationDecision } from "@kirkforge/core-types";
import type { Actor } from "@kirkforge/core-rbac";
import type {
  ArtifactBlockedEvent,
  ArtifactUnterminatedEvent,
  ArtifactTruncatedEvent,
  ArtifactEmittedEvent,
} from "@kirkforge/core-types";

export interface TaskInput {
  taskId?: string;
  description: string;
  context?: string;
  files?: string[];
  modeOverride?: DelegationMode;
  taskPass?: boolean | null;
  suppressMemory?: boolean;
  /**
   * Optional language override. When set, the orchestrator uses
   * `profileForLanguage` instead of `detectTaskProfile` (which parses
   * the description with regexes that can be tripped by shell/CLI
   * keywords in validator feedback or test commands). See
   * bench/kirkforge-mini/RESULTS.md for the bug this prevents.
   */
  language?: string;
  /**
   * Optional verifier-policy override. When set, replaces the
   * profile's default verifierPolicy entirely. Useful for the bench
   * (which only cares about the task validator, not the lint/types/
   * security verifiers that need a fully-bootstrapped project to
   * even run) and for tools that want to skip the verifiers.
   */
  verifierPolicy?: { required: VerifierSlot[]; advisory: VerifierSlot[] };
  /** Authenticated actor context. Used for audit logging and tenant-scoped policy enforcement. */
  actor?: Actor;
}

export interface DelegationResult {
  decision: DelegationDecision;
  emission: {
    agentId: string;
    content: string;
    promptTokens: number;
    completionTokens: number;
    totalTokens: number;
    reasoningTokens?: number;
    model: string;
    format: "hard-prompt" | "schema-contract" | "artifact" | "task-decompose";
    schemaContract?: Record<string, unknown>;
    finishReason?: string;
    retried?: boolean;
  };
  signals: Array<{
    id: string;
    taskId: string;
    domain: string;
    kind: string;
    source: string;
    ts: string;
    value: unknown;
    confidence?: number;
  }>;
  packet?: ReducedStatePacket;
  providerResolved?: string;
  skillsLoaded?: string[];
}

export function extractWrittenFiles(result: DelegationResult): string[] {
  for (const sig of result.signals) {
    if (sig.kind === "files.written" || sig.kind === "artifact.emitted") {
      const v = sig.value as { files?: Array<string | { path: string }>; filesWritten?: number };
      if (Array.isArray(v.files)) {
        return v.files.map((f) => (typeof f === "string" ? f : f.path)).filter(Boolean);
      }
    }
  }
  return [];
}

export interface EmittedFileInfo {
  path: string;
  sha256: string;
  bytes: number;
  beforeHash: string | null;
  existed: boolean;
}

export function extractEmissionFiles(result: DelegationResult): EmittedFileInfo[] {
  for (const sig of result.signals) {
    if (sig.kind === "artifact.emitted" || sig.kind === "files.written") {
      const v = sig.value as { files?: EmittedFileInfo[]; filesWritten?: number };
      if (Array.isArray(v.files) && v.files.length > 0) {
        return v.files;
      }
    }
  }
  return [];
}

export type OrchestratorResult = import("@kirkforge/core-types").Result<DelegationResult, Error>;

import type { ReducedStatePacket } from "./reducer.js";

export interface DecompositionResult {
  rootTask: string;
  tasks: import("@kirkforge/core-types").TaskNode[];
  totalEstimatedTokens: number;
  rationale: string;
}

export interface SubtaskExecutionResult {
  nodeId: string;
  ok: boolean;
  description: string;
  language: string;
  durationMs: number;
  tokensUsed: number;
  verdict?: string;
  error?: string;
  files?: string[];
}

export interface DecompositionExecutionResult {
  rootTask: string;
  results: SubtaskExecutionResult[];
  totalSubtasks: number;
  succeededCount: number;
  failedCount: number;
  totalTokens: number;
  totalDurationMs: number;
}

// ── Typed stats and health-check result interfaces ────────────────────────

/** Stats returned by `Orchestrator.getStats()`. */
export interface OrchestratorStats {
  totalDelegations: number;
  totalTokens: number;
  totalErrors?: number;
  activeTasks?: number;
  memoryEntries?: number;
  memorySizeBytes?: number;
}

/** Health-check result returned by `Orchestrator.healthCheck()`. */
export interface HealthCheckResult {
  status: "healthy" | "shutting_down";
  stats: OrchestratorStats;
  eventBus: {
    running: boolean;
    inflight: number;
    bufferSize: number;
  };
  memory: string;
  providers: number;
}

// ── Signal value type helpers ─────────────────────────────────────────────

/** Type-safe extraction of signal values from DelegationResult signals. */
export type SignalValueOf<T> = T extends { value: infer V } ? V : never;

export type ArtifactBlockedSignalValue = ArtifactBlockedEvent["value"];
export type ArtifactUnterminatedSignalValue = ArtifactUnterminatedEvent["value"];
export type ArtifactTruncatedSignalValue = ArtifactTruncatedEvent["value"];
export type ArtifactEmittedSignalValue = ArtifactEmittedEvent["value"];

// ── Validator + correction-loop + Orchestrator config types ───────────────

import type { ModelConfig } from "@kirkforge/model-config";
import type { EventBus } from "@kirkforge/core-events";
import type { Logger } from "@kirkforge/core-logging";
import type { MemoryStore } from "@kirkforge/memory-palace";
import type { QuotaManager } from "@kirkforge/core-enterprise";
import type {
  TaskValidationResult,
  TaskOutcome,
  CorrectionDecision,
  VerifierSlot,
} from "@kirkforge/correction-core";
import type { FinalVerdict, SourceOfTruth } from "./truth-model.js";

export interface ValidatorRunConfig {
  shellCommand?: string;
  timeoutMs?: number;
}

export interface LegacyValidatorRunConfig {
  command?: string;
  timeoutMs?: number;
}

export interface StructuredValidatorConfig {
  command: string;
  args: string[];
  cwd?: string;
  timeoutMs?: number;
}

export interface CorrectionLoopConfig {
  maxCorrections: number;
  maxCost?: number;
  maxValidatorMs?: number;
  validator?: ValidatorRunConfig | LegacyValidatorRunConfig | StructuredValidatorConfig;
}

export interface CorrectionLoopOutcome {
  finalAction: "accept" | "escalate";
  finalVerdict: FinalVerdict;
  sourceOfTruth: SourceOfTruth;
  taskValidation: TaskValidationResult;
  taskOutcome: TaskOutcome;
  turns: CorrectionDecision[];
  allPackets: ReducedStatePacket[];
  sessionTokens: number;
  sessionCost: number;
  validatorDurationMs: number;
}

export interface OrchestratorConfig {
  modelConfig: ModelConfig;
  providerKey?: string;
  logger?: Logger;
  eventBus?: EventBus;
  memoryStore?: MemoryStore;
  cwd?: string;
  decomposeProvider?: string;
  policyEngine?: import("@kirkforge/core-policy").PolicyEngine;
  auditLogger?: import("@kirkforge/core-events").AuditLogger;
  quotaManager?: QuotaManager;
}
