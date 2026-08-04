import type { DelegationMode } from "@kirkforge/core-types";
import {
  taskOutcomeFromValidation,
  makeSkippedValidation,
  type CorrectionDecision,
  type ReducedStatePacket,
  type TaskValidationResult,
  type TaskOutcome,
} from "@kirkforge/correction-core";
import { decideCorrection } from "./correction-loop.js";
import { detectTaskProfile } from "./task-profile.js";
import { resolveCostProviderKey, estimateSimpleCost } from "./cost.js";
import {
  resolveValidatorShellCommand,
  resolveStructuredValidatorConfig,
  runStructuredTaskValidator,
  runTaskValidator,
} from "./orchestrator-validators.js";
import { classifyTask } from "./classifier.js";
import { writeCorrectionMemoryObservation, flushMemory } from "./orchestrator-memory.js";
import {
  runIsolatedTurn,
  ensureBaselineSnapshot,
  cleanupTurnWorkspace,
  cleanupIsolatedWorkspace,
} from "./orchestrator-workspace.js";
import type {
  TaskInput,
  CorrectionLoopConfig,
  CorrectionLoopOutcome,
} from "./types.js";
import type { OrchestratorInternals } from "./orchestrator-shared.js";

/**
 * Run a task through the correction loop. Each iteration delegates,
 * captures the packet + emission, optionally runs a validator, then
 * feeds the resulting state into `decideCorrection`. Stops on accept,
 * escalate, or max-corrections.
 */
 
export async function runCorrectionLoop(
  s: OrchestratorInternals & {
    reducer: any;
    policyEngine?: any;
    logger?: any;
    _auditLogger?: any;
    _authPolicySlo?: any;
    _classifierMemory?: any;
    sharedEventBus: any;
    activeTurnWorkspace: string | null;
    delegate: (t: TaskInput) => Promise<import("./types.js").OrchestratorResult>;
  },
  task: TaskInput,
  config: CorrectionLoopConfig,
): Promise<CorrectionLoopOutcome> {
  if (s.busy)
    throw new Error("Orchestrator busy — only one correction loop may run concurrently");
  s.busy = true;
  const baseId = task.taskId ?? `task-${Date.now()}`;
  let taskId = baseId;
  const originalDescription = task.description;
  const originalProfile = detectTaskProfile(originalDescription);
  const profile = originalProfile;
  const turns: CorrectionDecision[] = [];
  const allPackets: ReducedStatePacket[] = [];
  let sessionTokens = 0;
  let sessionCost = 0;
  let done = false;
  let taskValidation: TaskValidationResult = makeSkippedValidation(
    "none",
    "no task validator configured",
  );
  const loopStartedAt = Date.now();
  try {
    if (s.classifierMemory && !s.classifierLoaded) {
      await s.classifierMemory.loadFromStore();
      s.classifierLoaded = true;
    }
    let actualMode: string = classifyTask(task, s.classifierMemory).mode;
    let actualModel: string = "unknown";
    const validatorShellCommand = resolveValidatorShellCommand(config.validator);
    const structuredValidator = resolveStructuredValidatorConfig(config.validator);

    const baselineCwd = ensureBaselineSnapshot(s);
    for (let turn = 0; turn <= config.maxCorrections && !done; turn++) {
      const result = await runIsolatedTurn(
        s,
        s.delegate.bind(s),
        task,
        taskId,
        baselineCwd,
      );
      if (!result.ok) {
        turns.push({
          action: "escalate",
          rationale: `delegation failed: ${result.error.message}`,
          packet: s.reducer.reduce(
            taskId,
            turn,
            profile.verifierPolicy,
            s.policyEngine?.getHash(),
          ),
          correctionCount: turn,
          workerTokens: 0,
          sessionTokens,
        });
        allPackets.push(
          s.reducer.reduce(
            taskId,
            turn,
            profile.verifierPolicy,
            s.policyEngine?.getHash(),
          ),
        );
        cleanupTurnWorkspace(s);
        break;
      }

      const delegationResult = result.value;
      actualMode = delegationResult.emission.format;
      actualModel = delegationResult.emission.model;

      const emission = delegationResult.emission;
      const workerTokens = emission.totalTokens;
      sessionTokens += workerTokens;
      const costKey = resolveCostProviderKey(delegationResult.providerResolved ?? "local-ollama");
      sessionCost += estimateSimpleCost(
        costKey,
        emission.promptTokens,
        emission.completionTokens,
      );

      const packet =
        delegationResult.packet ??
        s.reducer.reduce(taskId, turn, profile.verifierPolicy, s.policyEngine?.getHash());
      const emittedFiles =
        packet.emissions?.files?.map((f: { path: string }) => ({ path: f.path })) ?? [];
      if (structuredValidator) {
        taskValidation = await runStructuredTaskValidator(
          s,
          structuredValidator,
          emittedFiles,
          s.activeTurnWorkspace ?? undefined,
        );
        task = {
          ...task,
          taskPass:
            taskValidation.status === "pass"
              ? true
              : taskValidation.status === "fail"
                ? false
                : null,
        };
      } else if (validatorShellCommand) {
        taskValidation = await runTaskValidator(
          s,
          validatorShellCommand,
          config.validator?.timeoutMs ?? 120000,
          emittedFiles,
          s.activeTurnWorkspace ?? undefined,
        );
        task = {
          ...task,
          taskPass:
            taskValidation.status === "pass"
              ? true
              : taskValidation.status === "fail"
                ? false
                : null,
        };
      }
      cleanupTurnWorkspace(s);

      if (taskValidation.status === "error") {
        const escalateDecision: CorrectionDecision = {
          action: "escalate",
          rationale: `validator infrastructure error: ${taskValidation.reason ?? "unknown"}`,
          packet,
          correctionCount: turn,
          workerTokens,
          sessionTokens,
        };
        turns.push(escalateDecision);
        allPackets.push(packet);
        done = true;
        cleanupTurnWorkspace(s);
        continue;
      }

      const decision = decideCorrection(
        packet,
        turn,
        config.maxCorrections,
        workerTokens,
        sessionTokens,
        sessionCost,
        config.maxCost,
        profile.language,
        task.taskPass,
      );

      turns.push(decision);
      allPackets.push(packet);

      if (decision.action === "correct") {
        const nextTaskId = `${baseId}-c${turn + 1}`;
        const validatorFeedback =
          taskValidation.status === "fail"
            ? `\n\nExternal task validator (${taskValidation.validator}) ${taskValidation.status}: ${taskValidation.reason ?? "no reason provided"}`
            : "";
        task = {
          ...task,
          description:
            task.description + "\n\n" + (decision.correctionPrompt ?? "") + validatorFeedback,
          taskId: nextTaskId,
        };
        taskId = nextTaskId;
      } else {
        done = true;
      }
    }
    cleanupTurnWorkspace(s);

    let finalAction: "accept" | "escalate" =
      turns[turns.length - 1]!.action === "accept" ? "accept" : "escalate";
    if ((validatorShellCommand || structuredValidator) && taskValidation.status !== "pass") {
      finalAction = "escalate";
    }
    const loopDurationMs = Date.now() - loopStartedAt;
    const taskOutcome = taskOutcomeFromValidation(taskValidation);
    const lastPacket = allPackets[allPackets.length - 1];
    const protocolBroken =
      lastPacket?.artifactEnforcement?.status === "fail" &&
      (lastPacket.artifactEnforcement.unterminated || lastPacket.artifactEnforcement.truncated);
    const { computeFinalVerdict } = await import("./truth-model.js");
    const truth = computeFinalVerdict({
      taskValidation,
      hasValidator: !!(validatorShellCommand || structuredValidator),
      finalAction,
      packet: lastPacket,
      profile: { language: profile.language, validatorRequired: profile.validatorRequired },
      actualMode,
      protocolBroken,
    });
    const sourceOfTruth = truth.sourceOfTruth;
    const finalVerdict = truth.finalVerdict;
    await writeCorrectionMemoryObservation(
      s,
      originalDescription,
      originalProfile.language,
      task,
      taskId,
      finalAction,
      turns,
      allPackets,
      sessionTokens,
      taskValidation,
      finalVerdict,
      sourceOfTruth,
      actualModel,
      actualMode,
      loopDurationMs,
    );

    if (s.classifierMemory) {
      const outcomeClass =
        taskOutcome === "pass"
          ? "pass"
          : taskValidation.status === "error"
            ? "validator_error"
            : "task_fail";
      s.classifierMemory.learn(
        originalDescription,
        actualMode as DelegationMode,
        outcomeClass,
      );
    }
    await flushMemory(s);
    return {
      finalAction,
      finalVerdict,
      sourceOfTruth,
      taskValidation,
      taskOutcome,
      turns,
      allPackets,
      sessionTokens,
      sessionCost,
      validatorDurationMs: taskValidation.durationMs ?? 0,
    };
  } catch (error) {
    s.busy = false;
    const errMsg = error instanceof Error ? error.message : String(error);
    s.logger?.error(`[orchestrator] runCorrectionLoop crashed: ${errMsg}`);
    const escalateDecision: CorrectionDecision = {
      action: "escalate",
      rationale: `internal orchestrator error: ${errMsg}`,
      packet: s.reducer.reduce(
        taskId,
        0,
        profile.verifierPolicy,
        s.policyEngine?.getHash(),
      ),
      correctionCount: turns.length,
      workerTokens: 0,
      sessionTokens,
    };
    turns.push(escalateDecision);
    allPackets.push(
      s.reducer.reduce(taskId, 0, profile.verifierPolicy, s.policyEngine?.getHash()),
    );
    const fallbackValidation: TaskValidationResult = {
      status: "error",
      validator: "orchestrator",
      reason: errMsg,
    };
    try {
      if (s.classifierMemory) {
        s.classifierMemory.learn(
          originalDescription,
          "hard-prompt" as DelegationMode,
          "validator_error",
        );
      }
    } catch {
      /* best effort */
    }
    try {
      await flushMemory(s);
    } catch {
      /* best effort */
    }
    return {
      finalAction: "escalate",
      finalVerdict: "unknown",
      sourceOfTruth: "verifier",
      taskValidation: fallbackValidation,
      taskOutcome: "error" as TaskOutcome,
      turns,
      allPackets,
      sessionTokens,
      sessionCost: sessionCost ?? 0,
      validatorDurationMs: 0,
    };
  } finally {
    s.busy = false;
    cleanupIsolatedWorkspace(s);
    cleanupTurnWorkspace(s);
  }
}
