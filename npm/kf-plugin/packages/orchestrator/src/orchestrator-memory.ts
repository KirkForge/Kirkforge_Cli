import { scrubSecrets } from "@kirkforge/core-logging";
import { createHash, randomBytes } from "node:crypto";
import type { TaskInput, DelegationResult } from "./types.js";
import type {
  ReducedStatePacket,
  TaskValidationResult,
  CorrectionDecision,
} from "@kirkforge/correction-core";
import type { FinalVerdict, SourceOfTruth } from "./truth-model.js";
import type { OrchestratorInternals } from "./orchestrator-shared.js";

/**
 * Persist a single delegation outcome to memory. Derives outcome
 * (pass/fail/error) and reason from `task.taskPass`. Errors during
 * persistence are logged but non-fatal.
 */
export async function writeMemoryObservation(
  s: OrchestratorInternals,
  task: TaskInput,
  taskId: string,
  mode: string,
  result: DelegationResult,
  language: string,
  durationMs: number,
  emissions?: Array<{
    path: string;
    sha256: string;
    bytes: number;
    beforeHash: string | null;
    existed: boolean;
  }>,
): Promise<void> {
  if (!s.memoryStore) return;
  const packet = result.packet;
  let outcome: "pass" | "fail" | "error";
  let reason: string;
  if (task.taskPass === true) {
    outcome = "pass";
    reason = "task passed";
  } else if (task.taskPass === false) {
    outcome = "fail";
    reason = "task tests failed";
  } else {
    outcome = "error";
    reason = "task outcome unknown";
  }
  try {
    await s.memoryStore.writeTaskObservation({
      taskId,
      description: task.description,
      language,
      mode,
      model: result.emission.model,
      promptShape: result.emission.format,
      verifierOverall: packet?.verification.overall,
      finalAction:
        task.taskPass === true
          ? "accept"
          : packet?.verification.overall === "pass"
            ? "accept"
            : "escalate",
      taskPass: task.taskPass,
      outcome,
      reason,
      tokens: result.emission.totalTokens,
      durationMs,
      turns: 1,
      emissions: emissions ?? [],
    });
  } catch (e) {
    s.logger?.warn(
      `[orchestrator] Memory write failed: ${e instanceof Error ? e.message : String(e)}`,
    );
  }
}

/**
 * Persist the full correction-loop outcome: a task-observation record
 * (with scrubbed validator stdout/stderr) plus a run record linking
 * to its emission IDs. Used to teach memory-routing in future calls.
 */
export async function writeCorrectionMemoryObservation(
  s: OrchestratorInternals,
  originalDescription: string,
  originalLanguage: string,
  task: TaskInput,
  taskId: string,
  finalAction: "accept" | "escalate",
  turns: CorrectionDecision[],
  packets: ReducedStatePacket[],
  sessionTokens: number,
  taskValidation: TaskValidationResult,
  finalVerdict: FinalVerdict,
  sourceOfTruth: SourceOfTruth,
  actualModel: string,
  actualMode: string,
  durationMs: number,
): Promise<void> {
  if (!s.memoryStore || packets.length === 0) return;

  // Scrub secrets from validator stdout/stderr before persisting
  const scrubbedValidation = structuredClone(taskValidation);
  if (scrubbedValidation.details && typeof scrubbedValidation.details === "object") {
    const d = scrubbedValidation.details as Record<string, unknown>;
    if (typeof d.stdout === "string") d.stdout = scrubSecrets(d.stdout);
    if (typeof d.stderr === "string") d.stderr = scrubSecrets(d.stderr);
  }

  const lastPacket = packets[packets.length - 1]!;

  let outcome: "pass" | "fail" | "error";
  if (task.taskPass === true) {
    outcome = "pass";
  } else if (task.taskPass === false) {
    outcome = "fail";
  } else if (taskValidation.status === "pass") {
    outcome = "pass";
  } else if (taskValidation.status === "fail") {
    outcome = "fail";
  } else {
    outcome = "error";
  }

  let outcomeClass:
    | "pass"
    | "task_fail"
    | "validator_error"
    | "tool_error"
    | "escalated"
    | "unknown";
  if (outcome === "pass") {
    outcomeClass = "pass";
  } else if (task.taskPass === false) {
    outcomeClass = "task_fail";
  } else if (taskValidation.status === "error") {
    outcomeClass = "validator_error";
  } else if (finalAction === "escalate") {
    outcomeClass = "escalated";
  } else if (finalVerdict === "unknown") {
    outcomeClass = "unknown";
  } else {
    outcomeClass = "tool_error";
  }

  let routingLesson: "reward" | "punish" | "neutral";
  if (outcomeClass === "pass") {
    routingLesson = "reward";
  } else if (outcomeClass === "task_fail") {
    routingLesson = "punish";
  } else {
    routingLesson = "neutral";
  }

  const reason =
    task.taskPass === false
      ? taskValidation.status === "fail" || taskValidation.status === "error"
        ? `task validator ${taskValidation.status}: ${taskValidation.reason ?? "validator failed"}`
        : "task validator failed"
      : task.taskPass === true
        ? "task passed"
        : finalAction === "accept"
          ? "verification passed"
          : finalAction === "escalate"
            ? "correction loop escalated"
            : "task outcome unknown";

  const providerKey = s.providerKey;
  const providerConfig = s.modelConfig.providers[providerKey];
  const providerType = providerConfig?.provider ?? "unknown";
  const baseUrl = providerConfig?.baseUrl;

  const emissions = lastPacket.emissions?.files ?? [];
  const filesEmitted = emissions.length;
  const totalBytesEmitted = emissions.reduce((sum, f) => sum + f.bytes, 0);

  try {
    await s.memoryStore.writeTaskObservation({
      taskId,
      description: originalDescription,
      language: originalLanguage,
      mode: actualMode,
      model: actualModel,
      providerKey,
      providerType,
      baseUrl,
      promptShape: "correction-loop",
      verifierOverall: lastPacket.verification.overall,
      finalAction,
      taskPass: task.taskPass,
      outcome,
      outcomeClass,
      routingLesson,
      reason,
      finalVerdict,
      sourceOfTruth,
      taskValidation: scrubbedValidation,
      tokens: sessionTokens,
      durationMs,
      turns: turns.length,
      validatorDurationMs: taskValidation.durationMs ?? 0,
      emissions: emissions.map((f) => ({
        path: f.path,
        sha256: f.sha256,
        bytes: f.bytes,
        beforeHash: f.beforeHash ?? null,
        existed: f.existed ?? false,
      })),
    });

    const runId = `run-${taskId}-${Date.now()}-${randomBytes(4).toString("hex")}`;
    const runTurn = turns.length;

    const emissionRecords = emissions.map((f) => ({
      path: f.path,
      sha256: f.sha256,
      bytes: f.bytes,
      beforeHash: f.beforeHash ?? null,
      existed: f.existed ?? false,
    }));
    const preEmissionIds = emissionRecords.map((e, i) => {
      const pathHash = createHash("sha256").update(e.path).digest("hex").slice(0, 8);
      const sha256Prefix = e.sha256.slice(0, 8);
      return `emission-${runId}-t${runTurn}-${i}-${pathHash}-${sha256Prefix}`;
    });

    const runRecord = {
      runId,
      taskId,
      description: originalDescription,
      language: originalLanguage,
      mode: actualMode,
      model: actualModel,
      providerKey,
      providerType,
      baseUrl,
      outcome,
      outcomeClass,
      routingLesson,
      finalVerdict,
      sourceOfTruth,
      finalAction,
      tokens: sessionTokens,
      durationMs,
      turns: runTurn,
      validatorDurationMs: taskValidation.durationMs ?? 0,
      verifierOverall: lastPacket.verification.overall,
      filesEmitted,
      totalBytesEmitted,
      emissions,
      emissionIds: preEmissionIds,
      timestamp: new Date().toISOString(),
    };
    await s.memoryStore.writeRunAndEmissions(runRecord, emissionRecords, runTurn);
  } catch (e) {
    s.logger?.warn(
      `[orchestrator] Memory write (correction) failed: ${e instanceof Error ? e.message : String(e)}`,
    );
  }
}

/** Force the underlying adapter to flush to durable storage. */
export async function flushMemory(s: OrchestratorInternals): Promise<void> {
  if (!s.memoryStore) return;
  await s.memoryStore.adapter.persist();
}
