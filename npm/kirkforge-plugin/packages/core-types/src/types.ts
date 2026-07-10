export type SchemaVersion = "v3";
export const SCHEMA_VERSION: SchemaVersion = "v3";

export type AgentStatus = "working" | "blocked" | "done" | "failed" | "needs_review";
export type Severity = "none" | "low" | "medium" | "high" | "critical";
export type ToolSeverity = "info" | "low" | "medium" | "high" | "critical";
export type ToolSource = "secdev" | "gitnexus" | "eslint" | "graphify";

export interface ToolFinding {
  source: ToolSource;
  severity: ToolSeverity;
  code: string;
  message: string;
  fileRefs: string[];
  suggestedAction?: string;
}

export type DelegationMode = "hard-prompt" | "schema-contract" | "artifact" | "task-decompose";

export interface DelegationDecision {
  mode: DelegationMode;
  reason: string;
  autoRouted: boolean;
}

export interface TokenBudget {
  maxTokens: number;
  usedTokens: number;
  remainingTokens: number;
  exceeded: boolean;
}

export interface KirkForgeConfig {
  workspace: string;
  orchestrator: {
    maxConcurrentWorkers: number;
    retryAttempts: number;
    retryDelayMs: number;
  };
  tools: {
    eslint: { enabled: boolean; configFile?: string };
    secdev: { enabled: boolean };
    gitnexus: { enabled: boolean };
    graphify: { enabled: boolean; queryBudget?: number };
  };
  logging: {
    level: "trace" | "debug" | "info" | "warn" | "error";
    format: "json" | "human";
    output?: string;
  };
  memory: {
    path: string;
    retentionDays: number;
  };
}

export interface KirkForgeErrorInfo {
  code: string;
  message: string;
  cause?: Error;
  context?: Record<string, unknown>;
}

export interface PipelineResult {
  outcome: "success" | "partial" | "failed";
  taskId: string;
  steps: PipelineStepResult[];
  timestamp: string;
}

export interface PipelineStepResult {
  step: string;
  success: boolean;
  error?: string;
  durationMs: number;
}

export interface SystemMetrics {
  eventsProcessedTotal: number;
  eventsFailedTotal: number;
  eventsOverflowedTotal: number;
  toolRunsTotal: number;
  toolRunsFailed: number;
  idempotencySkippedTotal: number;
  lastMetricsUpdated: string;
}

export type EstimatedComplexity = "trivial" | "simple" | "moderate" | "complex";

export interface TaskNode {
  id: string;
  description: string;
  language: string;
  dependsOn: string[];
  estimatedComplexity: EstimatedComplexity;
  outputFiles: string[];
  verificationHint: string;
}
