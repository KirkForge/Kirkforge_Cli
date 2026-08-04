import { describe, it, expect } from "vitest";
import { taskOutcomeFromValidation, isTaskPass, makeSkippedValidation } from "../src/index.js";
import type { TaskValidationResult } from "../src/index.js";

describe("taskOutcomeFromValidation()", () => {
  it("pass maps to pass", () => {
    const result: TaskValidationResult = { status: "pass", validator: "test-suite" };
    expect(taskOutcomeFromValidation(result)).toBe("pass");
  });

  it("fail maps to fail", () => {
    const result: TaskValidationResult = { status: "fail", validator: "test-suite" };
    expect(taskOutcomeFromValidation(result)).toBe("fail");
  });

  it("error maps to unknown (not escalate — infrastructure failure is distinct from model failure)", () => {
    const result: TaskValidationResult = { status: "error", validator: "test-runner" };
    expect(taskOutcomeFromValidation(result)).toBe("unknown");
  });

  it("skipped maps to unknown (not escalate — no validator is distinct from model failure)", () => {
    const result: TaskValidationResult = { status: "skipped", validator: "lint" };
    expect(taskOutcomeFromValidation(result)).toBe("unknown");
  });
});

describe("isTaskPass()", () => {
  it("returns true only for pass", () => {
    expect(isTaskPass({ status: "pass", validator: "t" })).toBe(true);
    expect(isTaskPass({ status: "fail", validator: "t" })).toBe(false);
    expect(isTaskPass({ status: "error", validator: "t" })).toBe(false);
    expect(isTaskPass({ status: "skipped", validator: "t" })).toBe(false);
  });
});

describe("makeSkippedValidation()", () => {
  it("creates status skipped with validator and reason", () => {
    const result = makeSkippedValidation("jest", "no test files found");
    expect(result.status).toBe("skipped");
    expect(result.validator).toBe("jest");
    expect(result.reason).toBe("no test files found");
    expect(result.durationMs).toBeUndefined();
    expect(result.details).toBeUndefined();
  });
});

describe("TaskValidationResult type", () => {
  it("allows details without type narrowing", () => {
    const result: TaskValidationResult = {
      status: "fail",
      validator: "custom",
      reason: "output mismatch",
      durationMs: 1200,
      details: { expected: 42, actual: 37 },
    };
    expect(result.status).toBe("fail");
    expect(result.validator).toBe("custom");
    expect(result.reason).toBe("output mismatch");
    expect(result.durationMs).toBe(1200);
    expect(result.details).toEqual({ expected: 42, actual: 37 });
  });
});
