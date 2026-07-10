import type { TaskValidationResult } from "@kirkforge/correction-core";
import type { ReducedStatePacket } from "@kirkforge/correction-core";

// ── Truth-model: single source of verdict computation ────────────────────
//
// This module defines the **single precedence table** for computing the
// final verdict of a task run. Every code path that decides what happened
// must go through these functions — no scattered if/else in the orchestrator.
//
// Precedence (highest to lowest):
//   1. Protocol integrity break    → "fail" (unterminated/truncated artifact = no trust)
//   2. Validator pass/fail         → overrides all verifiers
//   3. Validator error/timeout     → "unknown" (infrastructure failure, not task failure)
//   4. Validator recommended but   → "unknown"
//      not configured (weak profile)
//   5. Schema-contract pass for    → "unknown" (doesn't write files)
//      coding task
//   6. Verifier fail (required)    → "fail"
//   7. Escalate with no clear fail → "unknown"
//   8. Verifier pass               → "pass"

// ── Types ──────────────────────────────────────────────────────────────────

export type FinalVerdict = "pass" | "fail" | "error" | "unknown";
export type SourceOfTruth = "task-validator" | "verifier";

export interface TruthInput {
  taskValidation: TaskValidationResult;
  hasValidator: boolean;
  finalAction: "accept" | "escalate";
  packet?: ReducedStatePacket;
  profile: {
    language: string;
    validatorRequired?: boolean;
  };
  actualMode: string;
  protocolBroken?: boolean;
}

export interface TruthOutput {
  finalVerdict: FinalVerdict;
  sourceOfTruth: SourceOfTruth;
  reason: string;
}

// ── Verdict computation ────────────────────────────────────────────────────

/**
 * Compute the final verdict given all available truth inputs.
 * This is the single entry point for verdict computation.
 */
export function computeFinalVerdict(input: TruthInput): TruthOutput {
  const { taskValidation, hasValidator, finalAction, packet, profile, actualMode, protocolBroken } =
    input;

  // Determine the effective source of truth
  const effectiveSourceOfTruth: SourceOfTruth = hasValidator ? "task-validator" : "verifier";

  // ── Precedence 1: Protocol integrity ──────────────────────────────────
  if (protocolBroken) {
    return {
      finalVerdict: "fail",
      sourceOfTruth: effectiveSourceOfTruth,
      reason:
        "protocol integrity broken (unterminated markers or truncated model output) — all artifact writes blocked",
    };
  }

  // ── Precedence 2: Validator result ────────────────────────────────────
  if (hasValidator) {
    return {
      finalVerdict: finalVerdictFromValidation(taskValidation),
      sourceOfTruth: "task-validator",
      reason: `validator result: ${taskValidation.status}`,
    };
  }

  // ── Precedence 3: Validator recommended but missing ───────────────────
  if (profile.validatorRequired) {
    return {
      finalVerdict: "unknown",
      sourceOfTruth: "verifier",
      reason: `validator required for ${profile.language} profile but not configured — verifier pass is advisory only`,
    };
  }

  // ── Precedence 4: Schema-contract mode clarification ──────────────────
  if (actualMode === "schema-contract") {
    if (packet && packet.verification.overall === "pass") {
      return {
        finalVerdict: "unknown",
        sourceOfTruth: "verifier",
        reason:
          "schema-contract validates structured output but does not persist files — pass cannot confirm code emission",
      };
    }
    return {
      finalVerdict: finalVerdictFromVerifier(finalAction, packet),
      sourceOfTruth: "verifier",
      reason: `schema-contract verifier outcome: ${packet?.verification.overall ?? "no packet"}`,
    };
  }

  // ── Precedence 5: Verifier result ─────────────────────────────────────
  const verdict = finalVerdictFromVerifier(finalAction, packet);
  return {
    finalVerdict: verdict,
    sourceOfTruth: "verifier",
    reason: `verifier result: overall=${packet?.verification.overall ?? "none"}, action=${finalAction}`,
  };
}

// ── Sub-functions (exported for testing) ────────────────────────────────────

export function finalVerdictFromValidation(validation: TaskValidationResult): FinalVerdict {
  if (validation.status === "pass") return "pass";
  if (validation.status === "fail") return "fail";
  if (validation.status === "error") return "unknown";
  return "unknown"; // skipped or unrecognized — epistemic uncertainty
}

export function finalVerdictFromVerifier(
  finalAction: "accept" | "escalate",
  packet?: ReducedStatePacket,
): FinalVerdict {
  if (finalAction === "accept" && packet?.verification.overall === "pass") return "pass";
  if (finalAction === "escalate" && (!packet || packet.verification.overall !== "fail"))
    return "unknown";
  return "fail";
}

/**
 * Maps validation status to a memory-friendly outcome string.
 * Error/infrastructure-status is distinguished from task pass/fail.
 */
export function validationOutcomeForMemory(
  validation: TaskValidationResult,
): "pass" | "fail" | "error" {
  if (validation.status === "pass") return "pass";
  if (validation.status === "fail") return "fail";
  return "error";
}
