import type { TaskValidationResult, TaskOutcome } from "./task-validator.js";
import { taskOutcomeFromValidation } from "./task-validator.js";

export interface BenchValidation {
  pass: boolean | null;
  kind: string;
  exitCode: number | null;
  output: string;
}

export interface BenchmarkRow {
  verifierOverall: string;
  taskValidation: TaskValidationResult;
  taskOutcome: TaskOutcome;
}

export function normalizeTaskValidation(validation: BenchValidation): TaskValidationResult {
  if (validation.pass === true) {
    return {
      status: "pass",
      validator: validation.kind,
      durationMs: undefined,
      details: validation.exitCode !== null ? { exitCode: validation.exitCode } : undefined,
    };
  }

  if (validation.pass === false) {
    return {
      status: "fail",
      validator: validation.kind,
      reason: firstLine(validation.output) || "task tests failed",
      durationMs: undefined,
      details:
        validation.exitCode !== null
          ? { exitCode: validation.exitCode, output: validation.output }
          : undefined,
    };
  }

  if (
    validation.kind === "skipped" ||
    validation.kind === "missing-validator" ||
    validation.kind === "missing-local-validator" ||
    validation.kind === "docker-unavailable"
  ) {
    return {
      status: "skipped",
      validator: validation.kind,
      reason: firstLine(validation.output) || `validator ${validation.kind} was not available`,
    };
  }

  return {
    status: "error",
    validator: validation.kind,
    reason:
      firstLine(validation.output) || `validation produced no result (kind=${validation.kind})`,
    details:
      validation.exitCode !== null
        ? { exitCode: validation.exitCode, output: validation.output }
        : undefined,
  };
}

export function makeBenchmarkRow(
  verifierOverall: string,
  validation: BenchValidation,
): BenchmarkRow {
  const taskValidation = normalizeTaskValidation(validation);
  return {
    verifierOverall,
    taskValidation,
    taskOutcome: taskOutcomeFromValidation(taskValidation),
  };
}

function firstLine(text: string): string {
  return text.split("\n")[0]!.trimEnd().slice(0, 200);
}
