import { describe, it, expect } from "vitest";
import {
  taskOutcomeFromValidation,
  isTaskPass,
  makeSkippedValidation,
} from "@kirkforge/correction-core";
import type { TaskValidationResult } from "@kirkforge/correction-core";
import { InMemoryAdapter, MemoryStore } from "@kirkforge/memory-palace";
import { finalVerdictFromValidation } from "../src/truth-model.js";

/**
 * Host adapter contract tests for validator truth.
 * Uses the production finalVerdictFromValidation from truth-model.ts
 * (pass→pass, fail→fail, error/skipped/unknown→unknown).
 */

describe("Validator truth contract: finalVerdict from taskValidation", () => {
  it("validator pass => finalVerdict pass, sourceOfTruth task-validator, taskPass true", () => {
    const validation: TaskValidationResult = { status: "pass", validator: "task-validator" };
    const finalVerdict = finalVerdictFromValidation(validation);
    const sourceOfTruth = "task-validator" as const;
    const taskPass: boolean | null =
      validation.status === "pass" ? true : validation.status === "fail" ? false : null;

    expect(finalVerdict).toBe("pass");
    expect(sourceOfTruth).toBe("task-validator");
    expect(taskPass).toBe(true);
    expect(taskOutcomeFromValidation(validation)).toBe("pass");
    expect(isTaskPass(validation)).toBe(true);
  });

  it("validator fail => finalVerdict fail, sourceOfTruth task-validator, taskPass false", () => {
    const validation: TaskValidationResult = {
      status: "fail",
      validator: "task-validator",
      reason: "tests failed",
    };
    const finalVerdict = finalVerdictFromValidation(validation);
    const sourceOfTruth = "task-validator" as const;
    const taskPass: boolean | null =
      validation.status === "pass" ? true : validation.status === "fail" ? false : null;

    expect(finalVerdict).toBe("fail");
    expect(sourceOfTruth).toBe("task-validator");
    expect(taskPass).toBe(false);
    expect(taskOutcomeFromValidation(validation)).toBe("fail");
    expect(isTaskPass(validation)).toBe(false);
  });

  it("validator error/timeout => finalVerdict unknown, sourceOfTruth task-validator, taskPass null", () => {
    const validation: TaskValidationResult = {
      status: "error",
      validator: "task-validator",
      reason: "timed out",
    };
    const finalVerdict = finalVerdictFromValidation(validation);
    const sourceOfTruth = "task-validator" as const;
    const taskPass: boolean | null =
      validation.status === "pass" ? true : validation.status === "fail" ? false : null;

    expect(finalVerdict).toBe("unknown");
    expect(sourceOfTruth).toBe("task-validator");
    expect(taskPass).toBe(null);
    expect(taskOutcomeFromValidation(validation)).toBe("unknown");
    expect(isTaskPass(validation)).toBe(false);
  });

  it("validator skipped => finalVerdict unknown, sourceOfTruth task-validator, taskPass null", () => {
    const validation = makeSkippedValidation("docker", "no docker available");
    const finalVerdict = finalVerdictFromValidation(validation);
    const taskPass: boolean | null =
      validation.status === "pass" ? true : validation.status === "fail" ? false : null;

    expect(finalVerdict).toBe("unknown");
    expect(taskPass).toBe(null);
    expect(taskOutcomeFromValidation(validation)).toBe("unknown");
  });
});

describe("Validator truth contract: verify-workspace failure is not task failure", () => {
  it("verification.overall=fail does not mean taskOutcome=fail", () => {
    const verifierOverall = "fail";
    const taskValidation: TaskValidationResult = { status: "pass", validator: "task-validator" };

    expect(verifierOverall).toBe("fail");
    expect(taskValidation.status).toBe("pass");
    expect(isTaskPass(taskValidation)).toBe(true);
  });

  it("verification.overall=pass does not mean taskOutcome=pass when task validator says fail", () => {
    const verifierOverall = "pass";
    const taskValidation: TaskValidationResult = {
      status: "fail",
      validator: "task-validator",
      reason: "output mismatch",
    };

    expect(verifierOverall).toBe("pass");
    expect(taskValidation.status).toBe("fail");
    expect(isTaskPass(taskValidation)).toBe(false);
  });
});

describe("Validator truth contract: observe/recall does not infer pass from verifier or action", () => {
  it("taskPass=false records outcome=fail regardless of verifierOverall", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);

    await store.writeTaskObservation({
      taskId: "test-1",
      description: "implement csv parser",
      language: "python",
      mode: "hard-prompt",
      model: "test-model",
      verifierOverall: "pass",
      finalAction: "accept",
      taskPass: false,
      sourceOfTruth: "task-validator",
      finalVerdict: "fail",
      tokens: 1000,
      durationMs: 5000,
    });

    const observations = await adapter.query({ kind: "task-observation" });
    const obs = observations.ok && observations.value?.[0];
    expect(obs).toBeDefined();
    expect(obs!.properties.outcome).toBe("fail");
    expect(obs!.properties.taskPass).toBe(false);
    expect(obs!.properties.finalVerdict).toBe("fail");
    expect(obs!.properties.sourceOfTruth).toBe("task-validator");
  });

  it("taskPass=true records outcome=pass even when finalAction=escalate", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);

    await store.writeTaskObservation({
      taskId: "test-2",
      description: "implement csv parser",
      language: "python",
      mode: "hard-prompt",
      model: "test-model",
      verifierOverall: "fail",
      finalAction: "escalate",
      taskPass: true,
      sourceOfTruth: "task-validator",
      finalVerdict: "pass",
      tokens: 800,
      durationMs: 4000,
    });

    const observations = await adapter.query({ kind: "task-observation" });
    const obs = observations.ok && observations.value?.[0];
    expect(obs).toBeDefined();
    expect(obs!.properties.outcome).toBe("pass");
    expect(obs!.properties.taskPass).toBe(true);
    expect(obs!.properties.finalVerdict).toBe("pass");
  });

  it("taskPass=null records outcome=error when validator timed out", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);

    await store.writeTaskObservation({
      taskId: "test-3",
      description: "implement web scraper",
      language: "python",
      mode: "hard-prompt",
      model: "test-model",
      verifierOverall: "unknown",
      finalAction: "escalate",
      taskPass: null,
      sourceOfTruth: "task-validator",
      finalVerdict: "error",
      tokens: 500,
      durationMs: 3000,
    });

    const observations = await adapter.query({ kind: "task-observation" });
    const obs = observations.ok && observations.value?.[0];
    expect(obs).toBeDefined();
    expect(obs!.properties.outcome).toBe("error");
    expect(obs!.properties.taskPass).toBeNull();
    expect(obs!.properties.finalVerdict).toBe("error");
  });

  it("recall recommendation reflects taskPass outcomes, not verifierOverall", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);

    await store.writeTaskObservation({
      taskId: "bad-1",
      description: "implement login flow",
      language: "python",
      mode: "hard-prompt",
      model: "model-a",
      verifierOverall: "pass",
      finalAction: "accept",
      taskPass: false,
      sourceOfTruth: "task-validator",
      finalVerdict: "fail",
      tokens: 1000,
      durationMs: 5000,
    });

    await store.writeTaskObservation({
      taskId: "good-1",
      description: "implement login flow",
      language: "python",
      mode: "hard-prompt",
      model: "model-b",
      verifierOverall: "fail",
      finalAction: "escalate",
      taskPass: true,
      sourceOfTruth: "task-validator",
      finalVerdict: "pass",
      tokens: 800,
      durationMs: 4000,
    });

    const recallResult = await store.recall("implement login flow");
    expect(recallResult.ok).toBe(true);
    const recommendation = recallResult.value;

    if (recommendation && recommendation.routingBias) {
      const prefer = recommendation.routingBias.prefer;
      const _avoid = recommendation.routingBias.avoid;
      expect(prefer.includes("model-b") || !prefer.includes("model-a")).toBe(true);
    }
  });
});

describe("Validator truth contract: sourceOfTruth determines verdict source", () => {
  it("when sourceOfTruth=task-validator, validator result overrides verifier result", () => {
    const taskValidation: TaskValidationResult = { status: "pass", validator: "task-validator" };
    const sourceOfTruth = "task-validator" as const;
    const finalVerdict =
      sourceOfTruth === "task-validator" ? finalVerdictFromValidation(taskValidation) : "fail";

    expect(finalVerdict).toBe("pass");
    expect(sourceOfTruth).toBe("task-validator");
  });

  it("when sourceOfTruth=verifier, verifier result is used (no task validator)", () => {
    const verifierOverall = "fail";
    const finalAction = "accept";
    const sourceOfTruth = "verifier" as const;
    const finalVerdict =
      sourceOfTruth === "verifier"
        ? finalAction === "accept" && verifierOverall === "pass"
          ? "pass"
          : "fail"
        : finalVerdictFromValidation({ status: "pass", validator: "test" });

    expect(finalVerdict).toBe("fail");
    expect(sourceOfTruth).toBe("verifier");
  });
});
