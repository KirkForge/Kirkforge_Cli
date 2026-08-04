import type {
  ReducedStatePacket,
  CorrectionDecision,
  VerifierSlot,
} from "@kirkforge/correction-core";
import { buildCorrectionPrompt } from "@kirkforge/correction-core";
import type { TaskLanguage } from "@kirkforge/correction-core";

export type { CorrectionConfig, CorrectionDecision } from "@kirkforge/correction-core";
export { buildCorrectionPrompt, toolNames } from "@kirkforge/correction-core";

export function decideCorrection(
  packet: ReducedStatePacket,
  correctionCount: number,
  maxCorrections: number,
  workerTokens: number,
  sessionTokens: number,
  sessionCost: number,
  maxCost?: number,
  language?: TaskLanguage,
  taskPass?: boolean | null,
): CorrectionDecision {
  if (taskPass === true) {
    return {
      action: "accept",
      rationale: "taskPass: true (external validator passed)",
      packet,
      correctionCount,
      workerTokens,
      sessionTokens,
    };
  }
  if (taskPass === false) {
    if (correctionCount >= maxCorrections) {
      return {
        action: "escalate",
        rationale: "taskPass: false (external validator failed); exceeded corrections",
        packet,
        correctionCount,
        workerTokens,
        sessionTokens,
      };
    }
    if (maxCost && sessionCost >= maxCost) {
      return {
        action: "escalate",
        rationale: `taskPass: false (external validator failed); session cost $${sessionCost.toFixed(4)} exceeds budget $${maxCost.toFixed(4)}`,
        packet,
        correctionCount,
        workerTokens,
        sessionTokens,
      };
    }
    return {
      action: "correct",
      rationale: "taskPass: false (external validator failed); targeted correction",
      correctionPrompt: buildCorrectionPrompt(packet, language),
      packet,
      correctionCount,
      workerTokens,
      sessionTokens,
    };
  }
  const securityPolicy: "required" | "advisory" | "absent" = !packet.verifierPolicy
    ? "required"
    : packet.verifierPolicy.required.includes("security" as VerifierSlot)
      ? "required"
      : packet.verifierPolicy.advisory.includes("security" as VerifierSlot)
        ? "advisory"
        : "absent";
  if (packet.verification.security.critical > 0 && securityPolicy === "required") {
    return {
      action: "escalate",
      rationale: "critical security finding",
      packet,
      correctionCount,
      workerTokens,
      sessionTokens,
    };
  }
  const graphPolicy: "required" | "advisory" | "absent" = !packet.verifierPolicy
    ? "required"
    : packet.verifierPolicy.required.includes("graph" as VerifierSlot)
      ? "required"
      : packet.verifierPolicy.advisory.includes("graph" as VerifierSlot)
        ? "advisory"
        : "absent";
  if (packet.graph.brokenEdges > 0 && graphPolicy === "required") {
    return {
      action: "escalate",
      rationale: `${packet.graph.brokenEdges} broken import edges`,
      packet,
      correctionCount,
      workerTokens,
      sessionTokens,
    };
  }
  if (correctionCount >= maxCorrections) {
    return {
      action: "escalate",
      rationale: `exceeded ${maxCorrections} corrections`,
      packet,
      correctionCount,
      workerTokens,
      sessionTokens,
    };
  }
  if (maxCost && sessionCost >= maxCost) {
    return {
      action: "escalate",
      rationale: `session cost $${sessionCost.toFixed(4)} exceeds budget $${maxCost.toFixed(4)}`,
      packet,
      correctionCount,
      workerTokens,
      sessionTokens,
    };
  }
  if (packet.verification.overall === "pass") {
    return {
      action: "accept",
      rationale: "verification passed",
      packet,
      correctionCount,
      workerTokens,
      sessionTokens,
    };
  }
  return {
    action: "correct",
    rationale: `verification ${packet.verification.overall}; targeted correction`,
    correctionPrompt: buildCorrectionPrompt(packet, language),
    packet,
    correctionCount,
    workerTokens,
    sessionTokens,
  };
}
