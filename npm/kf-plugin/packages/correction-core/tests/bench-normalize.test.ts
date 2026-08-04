import { describe, it, expect } from "vitest";
import { normalizeTaskValidation, makeBenchmarkRow } from "../src/index.js";

describe("normalizeTaskValidation()", () => {
  it("maps pass=true to status pass", () => {
    const result = normalizeTaskValidation({
      pass: true,
      kind: "tbench",
      exitCode: 0,
      output: "ok",
    });
    expect(result.status).toBe("pass");
    expect(result.validator).toBe("tbench");
  });

  it("maps pass=false to status fail", () => {
    const result = normalizeTaskValidation({
      pass: false,
      kind: "tbench",
      exitCode: 1,
      output: "1 test failed",
    });
    expect(result.status).toBe("fail");
    expect(result.validator).toBe("tbench");
    expect(result.reason).toContain("1 test failed");
  });

  it("maps pass=null with kind missing-validator to status skipped", () => {
    const result = normalizeTaskValidation({
      pass: null,
      kind: "missing-validator",
      exitCode: null,
      output: "no docker-compose",
    });
    expect(result.status).toBe("skipped");
    expect(result.validator).toBe("missing-validator");
    expect(result.reason).toContain("no docker-compose");
  });

  it("maps pass=null with kind skipped to status skipped", () => {
    const result = normalizeTaskValidation({
      pass: null,
      kind: "skipped",
      exitCode: null,
      output: "",
    });
    expect(result.status).toBe("skipped");
  });

  it("maps pass=null with kind docker-unavailable to status skipped", () => {
    const result = normalizeTaskValidation({
      pass: null,
      kind: "docker-unavailable",
      exitCode: null,
      output: "docker not found",
    });
    expect(result.status).toBe("skipped");
  });

  it("maps pass=null with kind missing-local-validator to status skipped", () => {
    const result = normalizeTaskValidation({
      pass: null,
      kind: "missing-local-validator",
      exitCode: null,
      output: "",
    });
    expect(result.status).toBe("skipped");
  });

  it("maps pass=null with other kinds to status error", () => {
    const result = normalizeTaskValidation({
      pass: null,
      kind: "infra-error",
      exitCode: null,
      output: "docker build failed",
    });
    expect(result.status).toBe("error");
    expect(result.validator).toBe("infra-error");
    expect(result.reason).toContain("docker build failed");
  });

  it("includes exitCode in details for pass", () => {
    const result = normalizeTaskValidation({
      pass: true,
      kind: "tbench",
      exitCode: 0,
      output: "all pass",
    });
    expect(result.details).toEqual({ exitCode: 0 });
  });

  it("includes exitCode and output in details for fail", () => {
    const result = normalizeTaskValidation({
      pass: false,
      kind: "tbench",
      exitCode: 1,
      output: "assertion error",
    });
    expect(result.status).toBe("fail");
    if (result.details && typeof result.details === "object") {
      expect(result.details).toHaveProperty("exitCode", 1);
    }
  });

  it("omits exitCode from details when null", () => {
    const result = normalizeTaskValidation({
      pass: true,
      kind: "local-tests",
      exitCode: null,
      output: "pass",
    });
    expect(result.details).toBeUndefined();
  });

  it("truncates long output to first line in reason", () => {
    const longOutput = "first line\nsecond line\nthird line";
    const result = normalizeTaskValidation({
      pass: false,
      kind: "tbench",
      exitCode: 1,
      output: longOutput,
    });
    expect(result.reason).toBe("first line");
  });

  it("uses default reason when output is empty for fail", () => {
    const result = normalizeTaskValidation({
      pass: false,
      kind: "tbench",
      exitCode: 1,
      output: "",
    });
    expect(result.reason).toBe("task tests failed");
  });

  it("uses default reason when output is empty for skipped validators", () => {
    const result = normalizeTaskValidation({
      pass: null,
      kind: "missing-validator",
      exitCode: null,
      output: "",
    });
    expect(result.reason).toContain("missing-validator");
  });
});

describe("makeBenchmarkRow()", () => {
  it("distinguishes verifierOverall from taskOutcome", () => {
    const row = makeBenchmarkRow("fail", {
      pass: true,
      kind: "tbench",
      exitCode: 0,
      output: "all pass",
    });
    expect(row.verifierOverall).toBe("fail");
    expect(row.taskValidation.status).toBe("pass");
    expect(row.taskOutcome).toBe("pass");
  });

  it("maps fail validation to fail outcome", () => {
    const row = makeBenchmarkRow("pass", {
      pass: false,
      kind: "tbench",
      exitCode: 1,
      output: "test failed",
    });
    expect(row.verifierOverall).toBe("pass");
    expect(row.taskValidation.status).toBe("fail");
    expect(row.taskOutcome).toBe("fail");
  });

  it("maps skipped validation to unknown outcome (infrastructure gap, not model failure)", () => {
    const row = makeBenchmarkRow("warn", {
      pass: null,
      kind: "missing-validator",
      exitCode: null,
      output: "",
    });
    expect(row.verifierOverall).toBe("warn");
    expect(row.taskValidation.status).toBe("skipped");
    expect(row.taskOutcome).toBe("unknown");
  });

  it("maps error validation to unknown outcome (infrastructure failure, not model failure)", () => {
    const row = makeBenchmarkRow("fail", {
      pass: null,
      kind: "infra-error",
      exitCode: null,
      output: "timeout",
    });
    expect(row.verifierOverall).toBe("fail");
    expect(row.taskValidation.status).toBe("error");
    expect(row.taskOutcome).toBe("unknown");
  });
});
