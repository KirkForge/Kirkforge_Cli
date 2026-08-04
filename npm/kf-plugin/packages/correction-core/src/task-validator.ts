export type TaskValidationStatus = "pass" | "fail" | "error" | "skipped";

export type TaskOutcome = "pass" | "fail" | "escalate" | "unknown";

export interface TaskValidationResult {
  status: TaskValidationStatus;
  validator: string;
  reason?: string;
  durationMs?: number;
  details?: unknown;
}

export function taskOutcomeFromValidation(result: TaskValidationResult): TaskOutcome {
  switch (result.status) {
    case "pass":
      return "pass";
    case "fail":
      return "fail";
    case "error":
      return "unknown";
    case "skipped":
      return "unknown";
  }
}

export function isTaskPass(result: TaskValidationResult): boolean {
  return result.status === "pass";
}

export function makeSkippedValidation(
  validator = "none",
  reason = "no validator configured",
): TaskValidationResult {
  return { status: "skipped", validator, reason };
}
