import type { OrchestratorResult, TaskInput, OrchestratorStats } from "./types.js";
import type { ModelProviderConfig } from "@kirkforge/model-config";
import type {
  ArtifactBlockedSignalValue,
  ArtifactUnterminatedSignalValue,
  ArtifactTruncatedSignalValue,
  ArtifactEmittedSignalValue,
} from "./types.js";
import { detectTaskProfile } from "./task-profile.js";
import { extractWrittenFiles, extractEmissionFiles } from "./types.js";
import { runVerifiers } from "./orchestrator-verifiers.js";
import { writeMemoryObservation } from "./orchestrator-memory.js";
import type { OrchestratorInternals } from "./orchestrator-shared.js";

/**
 * Re-emit artifact-blocked/unterminated/truncated/emitted signals to
 * the EventBus, run the deterministic verifiers, and (if not suppressed)
 * persist a memory observation. Returns the same result on success.
 */
export async function finalizeDelegation(
  s: OrchestratorInternals & { stats: OrchestratorStats; providerKey: string },
  result: OrchestratorResult,
  taskId: string,
  task: TaskInput,
  mode: string,
  profile: ReturnType<typeof detectTaskProfile>,
  providerConfig: ModelProviderConfig,
  startedAt: number,
): Promise<OrchestratorResult> {
  if (!result.ok) return result;

  result.value.providerResolved = s.providerKey;
  for (const sig of result.value.signals) {
    if (sig.kind === "artifact.blocked") {
      const bv = sig.value as ArtifactBlockedSignalValue;
      await s.sharedEventBus.emit({
        kind: "artifact.blocked",
        schemaVersion: "v3",
        sequence: 0,
        streamId: sig.id,
        taskId: sig.taskId,
        value: {
          blockedPaths: bv.blockedPaths,
          ...(bv.parseWarnings ? { parseWarnings: bv.parseWarnings } : {}),
        },
        timestamp: sig.ts,
      });
    } else if (sig.kind === "artifact.unterminated") {
      const uv = sig.value as ArtifactUnterminatedSignalValue;
      await s.sharedEventBus.emit({
        kind: "artifact.unterminated",
        schemaVersion: "v3",
        sequence: 0,
        streamId: sig.id,
        taskId: sig.taskId,
        value: { warnings: uv.warnings },
        timestamp: sig.ts,
      });
    } else if (sig.kind === "artifact.truncated") {
      const tv = sig.value as ArtifactTruncatedSignalValue;
      await s.sharedEventBus.emit({
        kind: "artifact.truncated",
        schemaVersion: "v3",
        sequence: 0,
        streamId: sig.id,
        taskId: sig.taskId,
        value: {
          finishReason: tv.finishReason,
          warnings: tv.warnings,
        },
        timestamp: sig.ts,
      });
    } else if (sig.kind === "artifact.emitted") {
      const ev = sig.value as ArtifactEmittedSignalValue;
      await s.sharedEventBus.emit({
        kind: "artifact.emitted",
        schemaVersion: "v3",
        sequence: 0,
        streamId: sig.id,
        taskId: sig.taskId,
        value: {
          filesWritten: ev.filesWritten,
          totalBytes: ev.totalBytes,
          files: ev.files,
          language: ev.language,
        },
        timestamp: sig.ts,
      });
    }
  }
  const writtenFiles = extractWrittenFiles(result.value);
  await runVerifiers(s, taskId, writtenFiles, profile.language, writtenFiles);
  const packet = s.reducer.reduce(
    taskId,
    0,
    profile.verifierPolicy,
    s.policyEngine?.getHash(),
  );
  result.value.packet = packet;
  if (mode === "artifact" && packet.changes.filesChanged === 0 && !packet.artifactEnforcement) {
    result.value.packet = {
      ...packet,
      verification: { ...packet.verification, overall: "fail" },
      artifactEnforcement: { blocked: 0, blockedPaths: [], status: "fail" },
    };
  }
  if (mode === "schema-contract" && packet.changes.filesChanged === 0 && !packet.emissions) {
    result.value.packet = {
      ...packet,
      verification: { ...packet.verification, overall: "fail" },
      artifactEnforcement: {
        blocked: 0,
        blockedPaths: [],
        status: "fail",
        reason: "schema-contract produced zero emissions",
      },
    };
  }
  if (!task.suppressMemory) {
    const _emissionFiles = extractEmissionFiles(result.value);
    await writeMemoryObservation(
      s,
      task,
      taskId,
      mode,
      result.value,
      profile.language,
      Date.now() - startedAt,
      _emissionFiles,
    );
  }

  s.stats.totalDelegations++;
  s.stats.totalTokens += result.value.emission.totalTokens;
  return result;
}
