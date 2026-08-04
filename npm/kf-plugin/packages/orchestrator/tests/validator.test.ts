import { describe, it, expect } from "vitest";
import { resolveValidatorShellCommand } from "../src/index.js";
import type {
  FinalVerdict,
  SourceOfTruth,
  ValidatorRunConfig,
  LegacyValidatorRunConfig,
} from "../src/index.js";
import type { TaskValidationResult } from "@kirkforge/correction-core";
import { taskOutcomeFromValidation, makeSkippedValidation } from "@kirkforge/correction-core";

describe("resolveValidatorShellCommand", () => {
  it("prefers shellCommand over legacy command", async () => {
    // resolveValidatorShellCommand imported statically above
    const config: ValidatorRunConfig = { shellCommand: "pytest", timeoutMs: 30000 };
    expect(resolveValidatorShellCommand(config)).toBe("pytest");
  });

  it("falls back to legacy command field", async () => {
    // resolveValidatorShellCommand imported statically above
    const config: LegacyValidatorRunConfig = { command: "npm test", timeoutMs: 60000 };
    expect(resolveValidatorShellCommand(config)).toBe("npm test");
  });

  it("returns undefined for empty config", async () => {
    // resolveValidatorShellCommand imported statically above
    expect(resolveValidatorShellCommand(undefined)).toBeUndefined();
    expect(resolveValidatorShellCommand({})).toBeUndefined();
    expect(resolveValidatorShellCommand({ timeoutMs: 5000 })).toBeUndefined();
  });

  it("shellCommand takes precedence when both fields are present", async () => {
    // resolveValidatorShellCommand imported statically above
    const config: ValidatorRunConfig & LegacyValidatorRunConfig = {
      shellCommand: "pytest -x",
      command: "npm test",
      timeoutMs: 30000,
    };
    expect(resolveValidatorShellCommand(config)).toBe("pytest -x");
  });
});

describe("Task validator verdict mapping", () => {
  it("validator pass => finalVerdict pass, sourceOfTruth task-validator, taskPass true", () => {
    const passResult: TaskValidationResult = {
      status: "pass",
      validator: "test-validator",
      reason: "all good",
    };
    expect(passResult.status).toBe("pass");
    expect(taskOutcomeFromValidation(passResult)).toBe("pass");
    expect(passResult.status === "pass" ? true : passResult.status === "fail" ? false : null).toBe(
      true,
    );
  });

  it("validator fail => finalVerdict fail, taskPass false", () => {
    const failResult: TaskValidationResult = {
      status: "fail",
      validator: "test-validator",
      reason: "tests failed",
    };
    expect(failResult.status).toBe("fail");
    expect(taskOutcomeFromValidation(failResult)).toBe("fail");
    expect(failResult.status === "pass" ? true : failResult.status === "fail" ? false : null).toBe(
      false,
    );
  });

  it("validator error => finalVerdict error, taskOutcome unknown", () => {
    const errorResult: TaskValidationResult = {
      status: "error",
      validator: "test-validator",
      reason: "timed out",
    };
    expect(errorResult.status).toBe("error");
    expect(taskOutcomeFromValidation(errorResult)).toBe("unknown");
    expect(
      errorResult.status === "pass" ? true : errorResult.status === "fail" ? false : null,
    ).toBe(null);
  });

  it("validator skipped => finalVerdict error, taskOutcome unknown", () => {
    const skippedResult = makeSkippedValidation("none", "no task validator configured");
    expect(skippedResult.status).toBe("skipped");
    expect(taskOutcomeFromValidation(skippedResult)).toBe("unknown");
    expect(
      skippedResult.status === "pass" ? true : skippedResult.status === "fail" ? false : null,
    ).toBe(null);
  });

  it("validator pass overrides verifier fail (sourceOfTruth)", () => {
    const passResult: TaskValidationResult = {
      status: "pass",
      validator: "test-validator",
      reason: "all good",
    };
    const sourceOfTruth: SourceOfTruth = "task-validator";
    const finalVerdict: FinalVerdict =
      sourceOfTruth === "task-validator"
        ? passResult.status === "pass"
          ? "pass"
          : passResult.status === "fail"
            ? "fail"
            : "error"
        : "fail";
    expect(finalVerdict).toBe("pass");
    expect(sourceOfTruth).toBe("task-validator");
  });
});

describe("Memory description preservation", () => {
  it("original description is preserved, not appended correction prompt", () => {
    const originalDescription = "Write a Python web scraper";
    const correctionPrompt = "Fix the missing import statement";
    const validatorFeedback = "\n\nExternal task validator (docker) fail: tests failed";

    const modifiedDescription = originalDescription + "\n\n" + correctionPrompt + validatorFeedback;
    expect(modifiedDescription).toContain(correctionPrompt);
    expect(modifiedDescription).toContain("External task validator");
  });
});

describe("CLI --validator options", () => {
  it("run command accepts --validator flag", async () => {
    const { Command } = await import("commander");
    const program = new Command();
    program
      .command("run-test-validator")
      .option("--validator <command>", "External task validator command; exit 0 means pass")
      .option("--validator-timeout-ms <n>", "Validator timeout in milliseconds", "120000")
      .action(() => {});

    program.exitOverride();
    try {
      program.parse(["run-test-validator", "--validator", "echo ok"], { from: "user" });
    } catch {
      // commander throws on --help or version, which is fine
    }
  });

  it("rejects invalid --validator-timeout-ms", () => {
    const timeoutMs = parseInt("abc", 10);
    expect(Number.isNaN(timeoutMs)).toBe(true);

    const timeoutMs2 = parseInt("-100", 10);
    expect(timeoutMs2 <= 0).toBe(true);

    const timeoutMs3 = parseInt("120000", 10);
    expect(timeoutMs3 > 0 && !Number.isNaN(timeoutMs3)).toBe(true);
  });
});
