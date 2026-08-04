import type { ModelConfig } from "@kirkforge/model-config";
import type { EventBus } from "@kirkforge/core-events";
import type { Logger } from "@kirkforge/core-logging";
import type { MemoryStore } from "@kirkforge/memory-palace";
import type { StateReducer } from "./reducer.js";
import type { OrchestratorStats } from "./types.js";
import type { SloMonitor, AuthPolicySloMonitor } from "./slo-monitor.js";
import type { ClassifierMemory } from "./classifier-persistence.js";
import type { PolicyEngine } from "@kirkforge/core-policy";
import type { AuditLogger } from "@kirkforge/core-events";
import type { QuotaManager } from "@kirkforge/core-enterprise";

/**
 * State object that the helper modules under `orchestrator-*.ts` need to
 * read or mutate. The Orchestrator class in index.ts satisfies this
 * shape; helper functions take an `OrchestratorInternals` parameter so
 * they don't need to know the full class.
 */
export interface OrchestratorInternals {
  // Config
  modelConfig: ModelConfig;
  providerKey: string;
  logger?: Logger;
  cwd: string;
  decomposeProvider: string;
  policyEngine?: PolicyEngine;
  auditLogger?: AuditLogger;
  quotaManager?: QuotaManager;
  // State
  reducer: StateReducer;
  sharedEventBus: EventBus;
  memoryStore?: MemoryStore;
  stats: OrchestratorStats;
  shuttingDown: boolean;
  busy: boolean;
  classifierLoaded: boolean;
  // Sub-monitors
  sloMonitor: SloMonitor | null;
  authPolicySlo: AuthPolicySloMonitor;
  classifierMemory: ClassifierMemory | null;
  // Workspace
  baselineSnapshotDir: string | null;
  isolatedBaselineDirs: string[];
  isolatedWorkspaceDirs: string[];
  activeTurnWorkspace: string | null;
}

/** Helper shape for any code that needs the full Orchestrator + a public class. */
 
export interface OrchestratorWithOrch extends OrchestratorInternals {
  orchestrator: any;
}

/** Compute-orchestrator base shape: config + shared deps + sub-monitors. */
 
export interface OrchestratorBase extends OrchestratorInternals {
  orchestrator: any;
}
