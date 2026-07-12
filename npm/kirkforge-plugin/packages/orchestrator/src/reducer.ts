import type {
  VerifyLintEvent,
  VerifyTypesEvent,
  VerifySecurityEvent,
  VerifyImportsEvent,
  StateChangesEvent,
  StateGraphEvent,
  ArtifactBlockedEvent,
  ArtifactUnterminatedEvent,
  ArtifactTruncatedEvent,
  ArtifactEmittedEvent,
} from "@kirkforge/core-types";
import { ok } from "@kirkforge/core-types";
import type { EventBus } from "@kirkforge/core-events";
import type {
  ReducedStatePacket,
  ArtifactEnforcement,
  VerifierPolicy,
  VerifierPolicyResult,
  VerifierSlot,
} from "@kirkforge/correction-core";

export type { ReducedStatePacket } from "@kirkforge/correction-core";

type ReducerSignal =
  | VerifyLintEvent
  | VerifyTypesEvent
  | VerifySecurityEvent
  | VerifyImportsEvent
  | StateChangesEvent
  | StateGraphEvent
  | ArtifactBlockedEvent
  | ArtifactUnterminatedEvent
  | ArtifactTruncatedEvent
  | ArtifactEmittedEvent;

const SLOT_TO_SIGNAL: Record<VerifierSlot, string> = {
  lint: "verify.lint",
  types: "verify.types",
  security: "verify.security",
  graph: "state.graph",
  imports: "verify.imports",
};

export class StateReducer {
  private perTask = new Map<string, Map<string, ReducerSignal[]>>();

  constructor(private eventBus: EventBus) {
    eventBus.on<VerifyLintEvent>("verify.lint", (e) => {
      this._store(e.taskId, "verify.lint", e);
      return Promise.resolve(ok(undefined));
    });
    eventBus.on<VerifyTypesEvent>("verify.types", (e) => {
      this._store(e.taskId, "verify.types", e);
      return Promise.resolve(ok(undefined));
    });
    eventBus.on<VerifySecurityEvent>("verify.security", (e) => {
      this._store(e.taskId, "verify.security", e);
      return Promise.resolve(ok(undefined));
    });
    eventBus.on<VerifyImportsEvent>("verify.imports", (e) => {
      this._store(e.taskId, "verify.imports", e);
      return Promise.resolve(ok(undefined));
    });
    eventBus.on<StateChangesEvent>("state.changes", (e) => {
      this._store(e.taskId, "state.changes", e);
      return Promise.resolve(ok(undefined));
    });
    eventBus.on<StateGraphEvent>("state.graph", (e) => {
      this._store(e.taskId, "state.graph", e);
      return Promise.resolve(ok(undefined));
    });
    eventBus.on<ArtifactBlockedEvent>("artifact.blocked", (e) => {
      this._store(e.taskId, "artifact.blocked", e);
      return Promise.resolve(ok(undefined));
    });
    eventBus.on<ArtifactUnterminatedEvent>("artifact.unterminated", (e) => {
      this._store(e.taskId, "artifact.unterminated", e);
      return Promise.resolve(ok(undefined));
    });
    eventBus.on<ArtifactTruncatedEvent>("artifact.truncated", (e) => {
      this._store(e.taskId, "artifact.truncated", e);
      return Promise.resolve(ok(undefined));
    });
    eventBus.on<ArtifactEmittedEvent>("artifact.emitted", (e) => {
      this._store(e.taskId, "artifact.emitted", e);
      return Promise.resolve(ok(undefined));
    });
  }

  private _store(taskId: string, kind: string, signal: ReducerSignal): void {
    const map = this.perTask.get(taskId) ?? new Map();
    const arr = map.get(kind) ?? [];
    arr.push(signal);
    map.set(kind, arr);
    this.perTask.set(taskId, map);
  }

  reduce(
    taskId: string,
    turn: number,
    policy?: VerifierPolicy,
    policyHash?: string,
  ): ReducedStatePacket {
    const map = this.perTask.get(taskId) ?? new Map();
    const get = <T extends ReducerSignal>(kind: string): T | undefined => {
      const arr = map.get(kind);
      return arr && arr.length > 0 ? (arr[arr.length - 1] as T) : undefined;
    };

    const lintS = get<VerifyLintEvent>("verify.lint");
    const typesS = get<VerifyTypesEvent>("verify.types");
    const secS = get<VerifySecurityEvent>("verify.security");
    const importsS = get<VerifyImportsEvent>("verify.imports");
    const changesS = get<StateChangesEvent>("state.changes");
    const graphS = get<StateGraphEvent>("state.graph");
    const blockedS = get<ArtifactBlockedEvent>("artifact.blocked");
    const unterminatedS = get<ArtifactUnterminatedEvent>("artifact.unterminated");
    const truncatedS = get<ArtifactTruncatedEvent>("artifact.truncated");
    const _emittedS = get<ArtifactEmittedEvent>("artifact.emitted");

    const notApplicable = new Set<VerifierSlot>();
    if (policy) {
      const policySlots = new Set([...policy.required, ...policy.advisory]);
      for (const slot of ["lint", "types", "security", "graph", "imports"] as VerifierSlot[]) {
        const signalKind = SLOT_TO_SIGNAL[slot];
        if (!policySlots.has(slot) && !map.has(signalKind)) {
          notApplicable.add(slot);
        }
      }
      for (const slot of policy.advisory) {
        const signalKind = SLOT_TO_SIGNAL[slot];
        if (!map.has(signalKind)) {
          notApplicable.add(slot);
        }
      }
    }

    const lintNAC = notApplicable.has("lint");
    const typesNAC = notApplicable.has("types");
    const secNAC = notApplicable.has("security");
    const graphNAC = notApplicable.has("graph");
    const importsNAC = notApplicable.has("imports");

    const lintStatus = lintS?.value?.status ?? (lintNAC ? "skipped" : "error");
    const typesStatus = typesS?.value?.status ?? (typesNAC ? "skipped" : "error");
    const secStatus = secS?.value?.status ?? (secNAC ? "skipped" : "error");
    const graphStatus = graphS?.value?.status ?? (graphNAC ? "skipped" : "error");
    const lint = {
      errors: lintS?.value?.errors ?? (lintNAC ? 0 : 1),
      warnings: lintS?.value?.warnings ?? 0,
      suppressed: lintS?.value?.suppressed ?? 0,
      status: lintStatus,
      error: lintS?.value?.error,
    };
    const types = {
      errors: typesS?.value?.errors ?? (typesNAC ? 0 : 1),
      status: typesStatus,
      error: typesS?.value?.error,
    };
    const sec = {
      findings: secS?.value?.findings ?? (secNAC ? 0 : 1),
      critical: secS?.value?.critical ?? 0,
      high: secS?.value?.high ?? (secNAC ? 0 : 1),
      status: secStatus,
      error: secS?.value?.error,
    };
    const imports = {
      findings: importsS?.value?.findings ?? 0,
      warnings: importsS?.value?.warnings ?? 0,
      info: importsS?.value?.info ?? 0,
      status: importsS?.value?.status ?? (importsNAC ? "skipped" : "error"),
      error: importsS?.value?.error,
    };
    const changes = {
      filesChanged: changesS?.value?.filesChanged ?? 0,
      paths: changesS?.value?.paths ?? [],
      insertions: changesS?.value?.insertions ?? 0,
      deletions: changesS?.value?.deletions ?? 0,
    };
    const graph = {
      edgeCount: graphS?.value?.edgeCount ?? 0,
      newEdges: graphS?.value?.newEdges ?? 0,
      brokenEdges:
        graphS?.value?.status === "skipped" || graphNAC ? 0 : (graphS?.value?.brokenEdges ?? 1),
      cycles: graphS?.value?.cycles ?? 0,
      status: graphStatus,
      error: graphS?.value?.error,
    };

    const blockedPaths = blockedS?.value?.blockedPaths ?? [];
    const parseWarnings = blockedS?.value?.parseWarnings ?? [];
    const hasUnterminated = !!unterminatedS;
    const hasTruncated = !!truncatedS;

    const artifactEnforcement: ArtifactEnforcement | undefined =
      blockedPaths.length > 0 || hasUnterminated || hasTruncated
        ? {
            blocked: blockedPaths.length,
            blockedPaths,
            ...(parseWarnings.length > 0 ? { parseWarnings } : {}),
            status: "fail",
            ...(hasUnterminated
              ? { unterminated: true, unterminatedWarnings: unterminatedS!.value.warnings }
              : {}),
            ...(hasTruncated
              ? {
                  truncated: true,
                  truncatedFinishReason: truncatedS!.value.finishReason,
                  truncatedWarnings: truncatedS!.value.warnings,
                }
              : {}),
          }
        : undefined;

    // Aggregate all artifact.emitted signals for this task/turn
    const allEmitted = map.get("artifact.emitted") as ArtifactEmittedEvent[] | undefined;
    let emissions:
      | {
          filesWritten: number;
          totalBytes: number;
          files: Array<{
            path: string;
            sha256: string;
            bytes: number;
            beforeHash: string | null;
            existed: boolean;
          }>;
        }
      | undefined = undefined;
    if (allEmitted && allEmitted.length > 0) {
      const allFiles: Array<{
        path: string;
        sha256: string;
        bytes: number;
        beforeHash: string | null;
        existed: boolean;
      }> = [];
      let totalBytes = 0;
      for (const s of allEmitted) {
        for (const f of s.value.files) {
          allFiles.push(f);
          totalBytes += f.bytes;
        }
      }
      emissions = { filesWritten: allFiles.length, totalBytes, files: allFiles };
    }

    let overall: ReducedStatePacket["verification"]["overall"] = "pass";
    const verifierError = [lintStatus, typesStatus, secStatus, graphStatus].includes("error");
    const graphPolicy = !policy
      ? "required"
      : policy.required.includes("graph")
        ? "required"
        : policy.advisory.includes("graph")
          ? "advisory"
          : "absent";
    const graphBrokenEdges = graph.brokenEdges;
    const graphHardFail = graphPolicy === "required" && graphBrokenEdges > 0;
    const graphAdvisory = graphPolicy === "advisory" && graphBrokenEdges > 0;
    if (
      verifierError ||
      types.errors > 0 ||
      lint.errors > 0 ||
      sec.critical > 0 ||
      graphHardFail ||
      artifactEnforcement
    )
      overall = "fail";
    // Warnings alone are not blocking: they are surfaced in the per-slot counts
    // but should not trigger a correction loop. Only high-severity security
    // findings (when security is advisory) and advisory graph broken edges
    // elevate the aggregate verdict to warn.
    else if (sec.high > 0 || graphAdvisory) overall = "warn";

    const contributingSignals: ReducedStatePacket["contributingSignals"] = [];
    for (const [kind, signals] of map) {
      for (const s of signals)
        contributingSignals.push({ kind, ts: s.timestamp, source: s.source });
    }

    let verifierPolicyResult: VerifierPolicyResult | undefined;
    if (policy) {
      const missingRequired: VerifierSlot[] = [];
      const skippedRequired: VerifierSlot[] = [];
      for (const slot of policy.required) {
        const signalKind = SLOT_TO_SIGNAL[slot];
        const signal = get(signalKind!);
        if (!signal) {
          missingRequired.push(slot);
        } else {
          const status = (signal.value as { status?: string })?.status;
          if (status === "skipped") {
            skippedRequired.push(slot);
          }
        }
      }
      if (missingRequired.length > 0 || skippedRequired.length > 0) {
        overall = "fail";
      }
      verifierPolicyResult = {
        required: policy.required,
        advisory: policy.advisory,
        missingRequired,
        skippedRequired,
      };
    }

    return {
      taskId,
      turn,
      ts: new Date().toISOString(),
      changes,
      graph,
      verification: { lint, types, security: sec, imports, overall },
      artifactEnforcement,
      emissions,
      verifierPolicy: verifierPolicyResult,
      contributingSignals,
      policyHash,
    };
  }

  resetTask(taskId: string): void {
    this.perTask.delete(taskId);
  }
}
