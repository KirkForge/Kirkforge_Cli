import { ok, err } from "@kirkforge/core-types";
import { EventBus } from "@kirkforge/core-events";
import type { Logger } from "@kirkforge/core-logging";
import type { ModelConfig } from "@kirkforge/model-config";
import { Agent } from "@kirkforge/agent-core";
import { BUILTIN_TEMPLATES, getContractTemplate } from "@kirkforge/prompt-core";
import { classifyTask } from "./classifier.js";
import { StateReducer } from "./reducer.js";
import { executeHardPrompt, executeSchemaContract } from "./modes.js";
import { executeArtifact } from "./artifact-mode.js";
import { KirkForgeError } from "@kirkforge/core-errors";

export { parseJsonlArtifacts } from "./artifact-mode.js";
export type { JsonlArtifact, ParsedArtifact, ParseResult } from "./artifact-mode.js";
import { detectTaskProfile, profileForLanguage } from "./task-profile.js";
import type {
  TaskInput,
  DelegationResult,
  OrchestratorResult,
  OrchestratorStats,
  HealthCheckResult,
  CorrectionLoopConfig,
  CorrectionLoopOutcome,
  OrchestratorConfig,
} from "./types.js";
import { SloMonitor, AuthPolicySloMonitor, type SloReport } from "./slo-monitor.js";
import { ClassifierMemory } from "./classifier-persistence.js";
import type { MemoryStore } from "@kirkforge/memory-palace";
import { QuotaManager } from "@kirkforge/core-enterprise";
import type { ReducedStatePacket } from "@kirkforge/correction-core";

export { StateReducer } from "./reducer.js";
export type { ReducedStatePacket } from "./reducer.js";
export { createVerificationEmitters } from "./emitter-factory.js";
export { decideCorrection } from "./correction-loop.js";
export { buildCorrectionPrompt, toolNames } from "@kirkforge/correction-core";
export type { CorrectionConfig, CorrectionDecision } from "@kirkforge/correction-core";
export type { TaskInput, DelegationResult, OrchestratorResult } from "./types.js";
export type { OrchestratorStats, HealthCheckResult } from "./types.js";
export type {
  DecompositionResult,
  SubtaskExecutionResult,
  DecompositionExecutionResult,
} from "./types.js";
export type { FinalVerdict, SourceOfTruth } from "./truth-model.js";
export { extractWrittenFiles } from "./types.js";
export type { TaskLanguage, TaskProfile, EmissionSchema } from "./task-profile.js";
export { detectTaskProfile, extensionForLanguage, profileForLanguage } from "./task-profile.js";
export {
  resolveValidatorShellCommand,
  resolveStructuredValidatorConfig,
  runStructuredTaskValidator,
  runTaskValidator,
} from "./orchestrator-validators.js";

import { resolveProvider, recallMemory } from "./orchestrator-provider.js";
import { runVerifiers, makeBrief } from "./orchestrator-verifiers.js";
import {
  runIsolatedTurn,
  cleanupTurnWorkspace,
  cleanupIsolatedWorkspace,
  ensureBaselineSnapshot,
} from "./orchestrator-workspace.js";
import { decomposeTask, executeDecomposition } from "./orchestrator-decompose.js";
import { runCorrectionLoop } from "./orchestrator-correction.js";
import {
  auditPolicyDeny,
  computeSlo,
  recordAuthEvent,
  recordPolicyAllow,
  healthCheckResult,
  authPolicySloReport,
} from "./orchestrator-telemetry.js";
import { finalizeDelegation } from "./orchestrator-finalize.js";
import type { OrchestratorInternals } from "./orchestrator-shared.js";

/**
 * Core delegation loop: classify, recall memory, dispatch to the
 * appropriate mode (hard-prompt / schema-contract / artifact /
 * task-decompose), then finalize (verifiers, artifact re-emit,
 * memory write).
 */
export class Orchestrator {
  modelConfig: ModelConfig;
  providerKey: string;
  logger?: Logger;
  reducer: StateReducer;
  sharedEventBus: EventBus;
  memoryStore?: MemoryStore;
  cwd: string;
  decomposeProvider: string;
  stats: OrchestratorStats = { totalDelegations: 0, totalTokens: 0 };
  shuttingDown = false;
  busy = false;
  classifierLoaded = false;
  sloMonitor: SloMonitor | null = null;
  authPolicySlo: AuthPolicySloMonitor;
  classifierMemory: ClassifierMemory | null = null;
  baselineSnapshotDir: string | null = null;
  isolatedBaselineDirs: string[] = [];
  policyEngine?: import("@kirkforge/core-policy").PolicyEngine;
  auditLogger?: import("@kirkforge/core-events").AuditLogger;
  quotaManager?: QuotaManager;

  // Workspace state (per-loop)
  isolatedWorkspaceDirs: string[] = [];
  activeTurnWorkspace: string | null = null;

  constructor(config: OrchestratorConfig) {
    this.modelConfig = config.modelConfig;
    this.providerKey = config.providerKey ?? config.modelConfig.defaultProvider;
    this.cwd = config.cwd ?? process.cwd();
    this.decomposeProvider = config.decomposeProvider ?? config.modelConfig.defaultProvider;
    this.logger = config.logger;
    this.memoryStore = config.memoryStore;
    const eb = config.eventBus ?? new EventBus();
    this.sharedEventBus = eb;
    this.reducer = new StateReducer(eb);
    if (this.memoryStore) {
      this.sloMonitor = new SloMonitor(this.memoryStore);
    }
    this.classifierMemory = new ClassifierMemory(this.memoryStore);
    this.authPolicySlo = new AuthPolicySloMonitor();
    this.policyEngine = config.policyEngine;
    this.auditLogger = config.auditLogger;
    this.quotaManager = config.quotaManager;
  }

  async delegate(task: TaskInput): Promise<OrchestratorResult> {
    if (this.shuttingDown) return err(new Error("Orchestrator is shutting down"));
    const taskId = task.taskId ?? `task-${Date.now()}`;

    if (this.policyEngine) {
      const providerConfig = resolveProvider(this as unknown as OrchestratorInternals, null);
      const modelDecision = this.policyEngine.checkModel(providerConfig.defaultModel);
      if (!modelDecision.allowed) {
        auditPolicyDeny(
          this as unknown as OrchestratorInternals,
          "model.deny",
          modelDecision.reason,
          modelDecision.policyHash,
          task.actor,
        );
        return err(
          new KirkForgeError("POLICY_DENIED", modelDecision.reason, {
            rule: modelDecision.rule,
            policyHash: modelDecision.policyHash,
          }),
        );
      }
      const profile = detectTaskProfile(task.description);
      const toolDecision = this.policyEngine.checkTool(profile.language ?? "unknown");
      if (!toolDecision.allowed) {
        auditPolicyDeny(
          this as unknown as OrchestratorInternals,
          "tool.deny",
          toolDecision.reason,
          toolDecision.policyHash,
          task.actor,
        );
        return err(
          new KirkForgeError("POLICY_DENIED", toolDecision.reason, {
            rule: toolDecision.rule,
            policyHash: toolDecision.policyHash,
          }),
        );
      }
    }

    let decision = classifyTask(task, this.classifierMemory);
    // Allow the caller to pin the language profile (e.g. the bench's
    // task.json declares a language; passing it through here keeps
    // `detectTaskProfile` from picking up "bash" or "shell" keywords
    // that appear in validator feedback or test scripts once the
    // correction loop appends them to the description. See
    // bench/kirkforge-mini/RESULTS.md for the symptom this fixes.
    const detected = (task as { language?: string }).language
      ? profileForLanguage((task as { language?: string }).language as never)
      : detectTaskProfile(task.description);
    // Allow the caller to override the verifier policy too. The bench
    // uses this to skip the lint/types/security verifiers (which need
    // a fully-bootstrapped project) and only run the task validator.
    const taskVerifierPolicy = (task as { verifierPolicy?: { required: import("@kirkforge/correction-core").VerifierSlot[]; advisory: import("@kirkforge/correction-core").VerifierSlot[] } }).verifierPolicy;
    const profile = taskVerifierPolicy
      ? { ...detected, verifierPolicy: taskVerifierPolicy }
      : detected;
    const memoryRecommendation = await recallMemory(
      this as unknown as OrchestratorInternals,
      task,
    );
    if (
      !task.modeOverride &&
      memoryRecommendation?.routingBias &&
      memoryRecommendation.confidence >= 0.75 &&
      memoryRecommendation.evidence >= 3
    ) {
      decision = {
        ...decision,
        mode: memoryRecommendation.mode as typeof decision.mode,
        reason: `${decision.reason}; memory bias ${memoryRecommendation.mode} (${memoryRecommendation.evidence} similar)`,
      };
    }
    this.logger?.info(
      `[orchestrator] Routing "${task.description.slice(0, 80)}" → ${decision.mode} (${decision.reason})`,
    );

    const providerConfig = resolveProvider(
      this as unknown as OrchestratorInternals,
      memoryRecommendation,
    );
    const delegationStartedAt = Date.now();

    switch (decision.mode) {
      case "hard-prompt": {
        const agent = new Agent(
          `agent-${taskId}`,
          providerConfig,
          BUILTIN_TEMPLATES["hard-prompt"],
        );
        const brief = makeBrief(this as unknown as OrchestratorInternals, task);
        return finalizeDelegation(
          this as unknown as OrchestratorInternals & { stats: OrchestratorStats; providerKey: string },
          await executeHardPrompt(agent, brief, taskId, this.cwd, profile, task.files?.[0]),
          taskId,
          task,
          decision.mode,
          profile,
          providerConfig,
          delegationStartedAt,
        );
      }
      case "schema-contract": {
        const contractTemplate = getContractTemplate(profile.language, profile.promptHint);
        const agent = new Agent(`agent-${taskId}`, providerConfig, contractTemplate);
        const brief = makeBrief(this as unknown as OrchestratorInternals, task);
        return finalizeDelegation(
          this as unknown as OrchestratorInternals & { stats: OrchestratorStats; providerKey: string },
          await executeSchemaContract(agent, brief, taskId),
          taskId,
          task,
          decision.mode,
          profile,
          providerConfig,
          delegationStartedAt,
        );
      }
      case "task-decompose": {
        const decomp = await decomposeTask(this as unknown as OrchestratorInternals, task);
        if (!decomp.ok) return err(decomp.error);
        const dr: DelegationResult = {
          decision,
          emission: {
            agentId: "decomposer",
            content: JSON.stringify(decomp.value.tasks),
            promptTokens: decomp.value.totalEstimatedTokens,
            completionTokens: 0,
            totalTokens: decomp.value.totalEstimatedTokens,
            model: "decompose",
            format: "task-decompose",
            schemaContract: {
              taskCount: decomp.value.tasks.length,
              rationale: decomp.value.rationale,
              tasks: decomp.value.tasks,
            },
          },
          signals: [
            {
              id: `sig-${taskId}`,
              taskId,
              domain: "task",
              kind: "decomposed",
              source: "decomposer",
              ts: new Date().toISOString(),
              value: {
                taskCount: decomp.value.tasks.length,
                tasks: decomp.value.tasks.map((t) => t.id),
              },
            },
          ],
        };
        return finalizeDelegation(
          this as unknown as OrchestratorInternals & { stats: OrchestratorStats; providerKey: string },
          ok(dr),
          taskId,
          task,
          decision.mode,
          profile,
          providerConfig,
          delegationStartedAt,
        );
      }
      case "artifact": {
        const agent = new Agent(`agent-${taskId}`, providerConfig, BUILTIN_TEMPLATES["artifact"]);
        const brief = makeBrief(this as unknown as OrchestratorInternals, task);
        return finalizeDelegation(
          this as unknown as OrchestratorInternals & { stats: OrchestratorStats; providerKey: string },
          await executeArtifact(agent, brief, taskId, this.cwd, profile),
          taskId,
          task,
          decision.mode,
          profile,
          providerConfig,
          delegationStartedAt,
        );
      }
    }
  }

  async runCorrectionLoop(task: TaskInput, config: CorrectionLoopConfig): Promise<CorrectionLoopOutcome> {
    return runCorrectionLoop(
      this as unknown as Parameters<typeof runCorrectionLoop>[0],
      task,
      config,
    );
  }

  reduce(taskId: string, turn?: number): ReducedStatePacket {
    return this.reducer.reduce(taskId, turn ?? 0);
  }

  async verify(
    task: { taskId?: string; description?: string; files?: string[] } = {},
  ): Promise<ReducedStatePacket> {
    const taskId = task.taskId ?? `verify-${Date.now()}`;
    // Default to a language-neutral profile so `verify` on a non-TypeScript
    // workspace does not fail-closed because there is no tsconfig.json to check.
    // The user can override by passing --task with a language-specific description.
    const profile = detectTaskProfile(task.description ?? "verify current workspace");
    await runVerifiers(
      this as unknown as OrchestratorInternals,
      taskId,
      task.files,
      profile.language,
    );
    const packet = this.reducer.reduce(
      taskId,
      0,
      profile.verifierPolicy,
      this.policyEngine?.getHash(),
    );
    this.reducer.resetTask(taskId);
    return packet;
  }

  getStats(): OrchestratorStats {
    return { ...this.stats };
  }

  getReducer(): StateReducer {
    return this.reducer;
  }
  getEventBus(): EventBus {
    return this.sharedEventBus;
  }

  async gracefulShutdown(): Promise<void> {
    this.shuttingDown = true;
    cleanupBaselineDirs(this as unknown as OrchestratorInternals);
    cleanupIsolatedWorkspace(this as unknown as OrchestratorInternals);
    cleanupTurnWorkspace(this as unknown as OrchestratorInternals);
    await this.sharedEventBus.gracefulShutdown();
    if (this.memoryStore) {
      await this.memoryStore.adapter.persist();
    }
  }

  async slo(): Promise<SloReport | null> {
    return computeSlo(this as unknown as OrchestratorInternals);
  }

  authPolicySloReport(): SloReport {
    return authPolicySloReport(this as unknown as OrchestratorInternals);
  }

  recordAuthEvent(
    type: "auth.success" | "auth.failure",
    actorId?: string,
    tenantId?: string,
  ): void {
    recordAuthEvent(this as unknown as OrchestratorInternals, type, actorId, tenantId);
  }

  recordPolicyAllow(actorId?: string, tenantId?: string): void {
    recordPolicyAllow(this as unknown as OrchestratorInternals, actorId, tenantId);
  }

  healthCheck(): HealthCheckResult {
    return healthCheckResult(this as unknown as OrchestratorInternals, this.stats);
  }

  async decomposeTask(
    task: TaskInput,
  ): Promise<
    import("@kirkforge/core-types").Result<import("./types.js").DecompositionResult, Error>
  > {
    return decomposeTask(this as unknown as OrchestratorInternals, task);
  }

  async executeDecomposition(
    taskId: string,
    actor?: import("@kirkforge/core-rbac").Actor,
  ): Promise<
    import("@kirkforge/core-types").Result<import("./types.js").DecompositionExecutionResult, Error>
  > {
    return executeDecomposition(
      this as unknown as Parameters<typeof executeDecomposition>[0],
      taskId,
      actor,
    );
  }

  /**
   * Create an isolated validator workspace. Exposed for the runTaskValidator
   * and runStructuredTaskValidator helpers in orchestrator-validators.ts
   * to call back into the class. Public so sub-module functions can
   * invoke it as `s.orchestrator._createIsolatedWorkspace(...)`.
   */
   
  async _createIsolatedWorkspace(emittedFiles?: any, baselineDir?: string): Promise<string> {
    const { createIsolatedWorkspace } = await import("./orchestrator-workspace.js");
    return createIsolatedWorkspace(this as unknown as OrchestratorInternals, emittedFiles, baselineDir);
  }
}

function cleanupBaselineDirs(s: OrchestratorInternals): void {
  for (const dir of s.isolatedBaselineDirs) {
    try {
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      require("node:fs").rmSync(dir, { recursive: true, force: true });
    } catch {
      /* best effort */
    }
  }
  s.isolatedBaselineDirs = [];
}

// re-export the run correction loop machinery helpers (not part of public API
// but accessible to test files via the package)
export { resolveProvider, recallMemory };
export { runVerifiers, makeBrief };
export { decomposeTask, executeDecomposition };
export { finalizeDelegation };
export { auditPolicyDeny, computeSlo, authPolicySloReport, recordAuthEvent, recordPolicyAllow, healthCheckResult };
export { runIsolatedTurn, cleanupTurnWorkspace, cleanupIsolatedWorkspace, ensureBaselineSnapshot };
export { runCorrectionLoop } from "./orchestrator-correction.js";
