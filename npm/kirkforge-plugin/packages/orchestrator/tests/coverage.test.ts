import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { StateReducer } from "../src/reducer.js";
import { classifyTask } from "../src/classifier.js";
import { decideCorrection } from "../src/correction-loop.js";
import {
  computeFinalVerdict,
  finalVerdictFromValidation,
  finalVerdictFromVerifier,
  validationOutcomeForMemory,
} from "../src/truth-model.js";
import { detectTaskProfile, profileForLanguage } from "../src/task-profile.js";
import { isInsideCwd, safeRelativePath, isBinaryLikeContent } from "../src/path-safety.js";
import type { VerifierPolicy } from "@kirkforge/correction-core";
import { makeSkippedValidation } from "@kirkforge/correction-core";

// ── Truth model ────────────────────────────────────────────────────────────

describe("truth-model", () => {
  it("protocolBroken=true forces fail regardless of all other signals", () => {
    const result = computeFinalVerdict({
      taskValidation: makeSkippedValidation(),
      hasValidator: true,
      finalAction: "accept",
      profile: { language: "typescript" },
      actualMode: "artifact",
      protocolBroken: true,
    });
    expect(result.finalVerdict).toBe("fail");
    expect(result.reason).toContain("protocol integrity broken");
  });

  it("validator pass overrides verifier fail", () => {
    const result = computeFinalVerdict({
      taskValidation: { status: "pass", validator: "test", reason: "ok" },
      hasValidator: true,
      finalAction: "escalate",
      profile: { language: "typescript" },
      actualMode: "hard-prompt",
    });
    expect(result.finalVerdict).toBe("pass");
    expect(result.sourceOfTruth).toBe("task-validator");
  });

  it("validator fail overrides verifier pass", () => {
    const result = computeFinalVerdict({
      taskValidation: { status: "fail", validator: "test", reason: "bad" },
      hasValidator: true,
      finalAction: "accept",
      profile: { language: "typescript" },
      actualMode: "hard-prompt",
    });
    expect(result.finalVerdict).toBe("fail");
    expect(result.sourceOfTruth).toBe("task-validator");
  });

  it("validator error produces unknown verdict", () => {
    const result = computeFinalVerdict({
      taskValidation: { status: "error", validator: "test", reason: "timeout" },
      hasValidator: true,
      finalAction: "accept",
      profile: { language: "typescript" },
      actualMode: "hard-prompt",
    });
    expect(result.finalVerdict).toBe("unknown");
  });

  it("validator skipped produces unknown verdict", () => {
    const r = finalVerdictFromValidation({ status: "skipped", validator: "test" });
    expect(r).toBe("unknown");
  });

  it("unrecognized status produces unknown", () => {
    const r = finalVerdictFromValidation({ status: "weird" as any, validator: "test" });
    expect(r).toBe("unknown");
  });

  it("validator recommended but not configured → unknown", () => {
    const result = computeFinalVerdict({
      taskValidation: makeSkippedValidation(),
      hasValidator: false,
      finalAction: "accept",
      profile: { language: "python", validatorRequired: true },
      actualMode: "hard-prompt",
    });
    expect(result.finalVerdict).toBe("unknown");
    expect(result.reason).toContain("validator required");
  });

  it("schema-contract pass returns unknown (no file writes)", () => {
    const result = computeFinalVerdict({
      taskValidation: makeSkippedValidation(),
      hasValidator: false,
      finalAction: "accept",
      packet: {
        taskId: "t",
        turn: 0,
        ts: "now",
        changes: { filesChanged: 0, paths: [], insertions: 0, deletions: 0 },
        graph: { edgeCount: 0, newEdges: 0, brokenEdges: 0, cycles: 0, status: "pass" },
        verification: {
          lint: { errors: 0, warnings: 0, status: "pass" },
          types: { errors: 0, status: "pass" },
          security: { findings: 0, critical: 0, high: 0, status: "pass" },
          overall: "pass",
        },
        contributingSignals: [],
      },
      profile: { language: "typescript" },
      actualMode: "schema-contract",
    });
    expect(result.finalVerdict).toBe("unknown");
    expect(result.reason).toContain("schema-contract");
  });

  it("escalate with no verifier packet returns unknown", () => {
    const result = computeFinalVerdict({
      taskValidation: makeSkippedValidation(),
      hasValidator: false,
      finalAction: "escalate",
      profile: { language: "typescript" },
      actualMode: "hard-prompt",
    });
    expect(result.finalVerdict).toBe("unknown");
  });

  it("verifier accept+pass → pass", () => {
    const r = finalVerdictFromVerifier("accept", {
      taskId: "t",
      turn: 0,
      ts: "now",
      changes: { filesChanged: 0, paths: [], insertions: 0, deletions: 0 },
      graph: { edgeCount: 0, newEdges: 0, brokenEdges: 0, cycles: 0, status: "pass" },
      verification: {
        lint: { errors: 0, warnings: 0, status: "pass" },
        types: { errors: 0, status: "pass" },
        security: { findings: 0, critical: 0, high: 0, status: "pass" },
        overall: "pass",
      },
      contributingSignals: [],
    });
    expect(r).toBe("pass");
  });

  it("escalate with no packet → unknown", () => {
    const r = finalVerdictFromVerifier("escalate", undefined);
    expect(r).toBe("unknown");
  });

  it("anything else → fail", () => {
    const r = finalVerdictFromVerifier("escalate", {
      taskId: "t",
      turn: 0,
      ts: "now",
      changes: { filesChanged: 0, paths: [], insertions: 0, deletions: 0 },
      graph: { edgeCount: 0, newEdges: 0, brokenEdges: 0, cycles: 0, status: "pass" },
      verification: {
        lint: { errors: 1, warnings: 0, status: "fail" },
        types: { errors: 0, status: "pass" },
        security: { findings: 0, critical: 0, high: 0, status: "pass" },
        overall: "fail",
      },
      contributingSignals: [],
    });
    expect(r).toBe("fail");
  });

  it("validationOutcomeForMemory maps pass/fail/error", () => {
    expect(validationOutcomeForMemory({ status: "pass", validator: "x" })).toBe("pass");
    expect(validationOutcomeForMemory({ status: "fail", validator: "x" })).toBe("fail");
    expect(validationOutcomeForMemory({ status: "error", validator: "x" })).toBe("error");
    expect(validationOutcomeForMemory({ status: "skipped", validator: "x" })).toBe("error");
  });
});

// ── Classifier ─────────────────────────────────────────────────────────────

describe("classifier (extended)", () => {
  it("empty description defaults to hard-prompt", () => {
    const r = classifyTask({ description: "" });
    expect(r.mode).toBe("hard-prompt");
    expect(r.autoRouted).toBe(true);
  });

  it("generate file → artifact", () => {
    const r = classifyTask({ description: "generate a new module file" });
    expect(r.mode).toBe("artifact");
  });

  it("create component → artifact", () => {
    const r = classifyTask({ description: "create a React component" });
    expect(r.mode).toBe("artifact");
  });

  it("write file → artifact", () => {
    const r = classifyTask({ description: "write a config file" });
    expect(r.mode).toBe("artifact");
  });

  it("build service → artifact", () => {
    const r = classifyTask({ description: "build a REST service" });
    expect(r.mode).toBe("artifact");
  });

  it("make script → artifact", () => {
    const r = classifyTask({ description: "make a deployment script" });
    expect(r.mode).toBe("artifact");
  });

  it("audit → schema-contract", () => {
    const r = classifyTask({ description: "audit the security posture" });
    expect(r.mode).toBe("schema-contract");
  });

  it("assess → schema-contract", () => {
    const r = classifyTask({ description: "assess the code quality" });
    expect(r.mode).toBe("schema-contract");
  });

  it("evaluate → schema-contract", () => {
    const r = classifyTask({ description: "evaluate the architecture" });
    expect(r.mode).toBe("schema-contract");
  });

  it("review the → schema-contract", () => {
    const r = classifyTask({ description: "review the pull request" });
    expect(r.mode).toBe("schema-contract");
  });

  it("validate → schema-contract", () => {
    const r = classifyTask({ description: "validate the input" });
    expect(r.mode).toBe("schema-contract");
  });

  it("verify → schema-contract", () => {
    const r = classifyTask({ description: "verify the config" });
    expect(r.mode).toBe("schema-contract");
  });

  it("fix → hard-prompt", () => {
    const r = classifyTask({ description: "fix the lint errors" });
    expect(r.mode).toBe("hard-prompt");
  });

  it("refactor → hard-prompt", () => {
    const r = classifyTask({ description: "refactor the auth module" });
    expect(r.mode).toBe("hard-prompt");
  });

  it("repair → hard-prompt", () => {
    const r = classifyTask({ description: "repair the broken tests" });
    expect(r.mode).toBe("hard-prompt");
  });

  it("artifact wins ties over schema-contract", () => {
    const r = classifyTask({ description: "create file and audit results" });
    expect(r.mode).toBe("artifact");
  });

  it("modeOverride bypasses classifier", () => {
    const r = classifyTask({ description: "generate a file", modeOverride: "schema-contract" });
    expect(r.mode).toBe("schema-contract");
    expect(r.autoRouted).toBe(false);
  });
});

// ── Correction loop ────────────────────────────────────────────────────────

describe("correction-loop (extended)", () => {
  function basePacket() {
    return {
      taskId: "t1",
      turn: 0,
      ts: "now",
      changes: { filesChanged: 0, paths: [], insertions: 0, deletions: 0 },
      graph: { edgeCount: 0, newEdges: 0, brokenEdges: 0, cycles: 0, status: "pass" as const },
      verification: {
        lint: { errors: 1, warnings: 0, status: "fail" as const },
        types: { errors: 1, status: "fail" as const },
        security: { findings: 0, critical: 0, high: 0, status: "pass" as const },
        overall: "fail" as const,
      },
      contributingSignals: [],
    };
  }

  it("taskPass=true → accept", () => {
    const d = decideCorrection(basePacket(), 0, 3, 100, 100, 0, undefined, "typescript", true);
    expect(d.action).toBe("accept");
  });

  it("taskPass=false with remaining corrections → correct", () => {
    const d = decideCorrection(basePacket(), 0, 3, 100, 100, 0, undefined, "typescript", false);
    expect(d.action).toBe("correct");
  });

  it("taskPass=false with max corrections reached → escalate", () => {
    const d = decideCorrection(basePacket(), 3, 3, 100, 100, 0, 10, "typescript", false);
    expect(d.action).toBe("escalate");
  });

  it("taskPass=false with cost exceeded → escalate", () => {
    const d = decideCorrection(basePacket(), 0, 3, 100, 100, 5.01, 5, "typescript", false);
    expect(d.action).toBe("escalate");
  });

  it("critical security finding with required security → escalate", () => {
    const p = {
      ...basePacket(),
      verification: {
        ...basePacket().verification,
        security: { findings: 1, critical: 1, high: 0, status: "fail" as const },
      },
      verifierPolicy: {
        required: ["lint", "types", "security"] as const,
        advisory: [] as const,
        missingRequired: [] as const,
        skippedRequired: [] as const,
      },
    };
    const d = decideCorrection(p, 0, 3, 100, 100, 0, undefined, "typescript", null);
    expect(d.action).toBe("escalate");
  });

  it("broken import edges with required graph → escalate", () => {
    const p = {
      ...basePacket(),
      graph: { edgeCount: 5, newEdges: 0, brokenEdges: 3, cycles: 0, status: "fail" as const },
      verifierPolicy: {
        required: ["lint", "types", "graph"] as const,
        advisory: [] as const,
        missingRequired: [] as const,
        skippedRequired: [] as const,
      },
    };
    const d = decideCorrection(p, 0, 3, 100, 100, 0, undefined, "typescript", null);
    expect(d.action).toBe("escalate");
  });

  it("max corrections exceeded → escalate", () => {
    const d = decideCorrection(basePacket(), 3, 3, 100, 100, 0, undefined, "typescript", null);
    expect(d.action).toBe("escalate");
  });

  it("verifier pass → accept", () => {
    const p = {
      ...basePacket(),
      verification: {
        ...basePacket().verification,
        overall: "pass" as const,
        lint: { errors: 0, warnings: 0, status: "pass" as const },
        types: { errors: 0, status: "pass" as const },
      },
    };
    const d = decideCorrection(p, 0, 3, 100, 100, 0, undefined, "typescript", null);
    expect(d.action).toBe("accept");
  });

  it("correction prompt contains tool names for given language", () => {
    const d = decideCorrection(basePacket(), 0, 3, 100, 100, 0, undefined, "python", null);
    expect(d.action).toBe("correct");
    expect(d.correctionPrompt).toBeDefined();
    expect(d.correctionPrompt).toContain("KirkForge Python lint engine");
    expect(d.correctionPrompt).toContain("pyright");
  });
});

// ── Path safety ────────────────────────────────────────────────────────────

describe("path safety (extended)", () => {
  it("isInsideCwd rejects absolute paths", () => {
    expect(isInsideCwd("/etc/passwd", "/home/user/project")).toBe(false);
  });

  it("isInsideCwd rejects .. traversal", () => {
    expect(isInsideCwd("/home/user/../etc", "/home/user/project")).toBe(false);
  });

  it("isInsideCwd accepts valid subpath", () => {
    expect(isInsideCwd("/home/user/project/src/file.ts", "/home/user/project")).toBe(true);
  });

  it("safeRelativePath returns null for empty input", () => {
    expect(safeRelativePath("/home/user/project", "")).toBeNull();
  });

  it("safeRelativePath returns null for absolute path", () => {
    expect(safeRelativePath("/home/user/project", "/etc/passwd")).toBeNull();
  });

  it("safeRelativePath returns null for .. traversal", () => {
    expect(safeRelativePath("/home/user/project", "../etc")).toBeNull();
  });

  it("safeRelativePath returns null for hidden segments", () => {
    expect(safeRelativePath("/home/user/project", ".env")).toBeNull();
  });

  it("safeRelativePath allows hidden segments when opted in", () => {
    expect(
      safeRelativePath("/home/user/project", ".vscode/settings.json", { allowHidden: true }),
    ).not.toBeNull();
  });

  it("safeRelativePath returns valid relative path", () => {
    expect(safeRelativePath("/home/user/project", "src/file.ts")).toBe("src/file.ts");
  });

  it("isBinaryLikeContent detects binary content", () => {
    const buf = Buffer.alloc(1000, 0);
    expect(isBinaryLikeContent(buf.toString("binary"))).toBe(true);
  });

  it("isBinaryLikeContent accepts text content", () => {
    expect(isBinaryLikeContent("hello world\nfunction foo() {}\n")).toBe(false);
  });

  it("isBinaryLikeContent returns false for empty string", () => {
    expect(isBinaryLikeContent("")).toBe(false);
  });
});

// ── Task profile ───────────────────────────────────────────────────────────

describe("task profile (extended)", () => {
  it("detects python from .py extension", () => {
    const profile = profileForLanguage("python");
    expect(profile.language).toBe("python");
    expect(profile.allowedExtensions).toContain(".py");
  });

  it("detects typescript from .ts extension", () => {
    const profile = profileForLanguage("typescript");
    expect(profile.language).toBe("typescript");
    expect(profile.allowedExtensions).toContain(".ts");
  });

  it("detects go from .go extension", () => {
    const profile = profileForLanguage("go");
    expect(profile.language).toBe("go");
    expect(profile.allowedExtensions).toContain(".go");
  });

  it("detects rust from .rs extension", () => {
    const profile = profileForLanguage("rust");
    expect(profile.language).toBe("rust");
    expect(profile.allowedExtensions).toContain(".rs");
  });

  it("detects cpp from .cpp extension", () => {
    const profile = profileForLanguage("cpp");
    expect(profile.language).toBe("cpp");
    expect(profile.allowedExtensions).toContain(".cpp");
  });

  it("detects shell from .sh extension", () => {
    const profile = profileForLanguage("shell");
    expect(profile.language).toBe("shell");
    expect(profile.allowedExtensions).toContain(".sh");
  });

  it("detects sql from .sql extension", () => {
    const profile = profileForLanguage("sql");
    expect(profile.language).toBe("sql");
    expect(profile.allowedExtensions).toContain(".sql");
  });

  it("detects javascript from .js extension", () => {
    const profile = profileForLanguage("javascript");
    expect(profile.language).toBe("javascript");
  });

  it("detects c from .c extension", () => {
    const profile = profileForLanguage("c");
    expect(profile.language).toBe("c");
  });

  it("detectTaskProfile for python returns valid profile", () => {
    const profile = detectTaskProfile("Write a Python script", "python");
    expect(profile.language).toBe("python");
    expect(profile.allowedExtensions).toContain(".py");
    expect(profile.verifierPolicy).toBeDefined();
  });

  it("text profile allows .txt and .md", () => {
    const profile = profileForLanguage("text");
    expect(profile.allowedExtensions).toContain(".txt");
    expect(profile.allowedExtensions).toContain(".md");
  });
});

// ── Reducer: verifierPolicy edge cases ─────────────────────────────────────

describe("StateReducer verifierPolicy", () => {
  it("empty policy with no required slots → pass", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = { required: [], advisory: [] };
    const packet = reducer.reduce("t-no-req", 0, policy);
    expect(packet.verification.overall).toBe("pass");
  });

  it("missing security in required → fail", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = { required: ["lint", "types", "security"], advisory: [] };
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s",
      taskId: "t-sec",
      value: {
        status: "pass",
        errors: 0,
        warnings: 0,
        filesScanned: 1,
        durationMs: 10,
        details: [],
      },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.types",
      schemaVersion: "v3",
      sequence: 2,
      streamId: "s",
      taskId: "t-sec",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    const packet = reducer.reduce("t-sec", 0, policy);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.verifierPolicy?.missingRequired).toContain("security");
  });
});

// ── Reducer: emission aggregation ──────────────────────────────────────────

describe("StateReducer emission aggregation", () => {
  it("aggregates multiple artifact.emitted signals", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s",
      taskId: "t-em",
      value: {
        status: "pass",
        errors: 0,
        warnings: 0,
        filesScanned: 1,
        durationMs: 10,
        details: [],
      },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.types",
      schemaVersion: "v3",
      sequence: 2,
      streamId: "s",
      taskId: "t-em",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s",
      taskId: "t-em",
      value: {
        status: "pass",
        findings: 0,
        critical: 0,
        high: 0,
        filesScanned: 1,
        durationMs: 10,
        details: [],
      },
      timestamp: "now",
    });
    await bus.emit({
      kind: "state.graph",
      schemaVersion: "v3",
      sequence: 4,
      streamId: "s",
      taskId: "t-em",
      value: {
        status: "pass",
        edgeCount: 0,
        newEdges: 0,
        brokenEdges: 0,
        cycles: 0,
        durationMs: 10,
      },
      timestamp: "now",
    });
    await bus.emit({
      kind: "artifact.emitted",
      schemaVersion: "v3",
      sequence: 5,
      streamId: "s",
      taskId: "t-em",
      value: {
        filesWritten: 2,
        totalBytes: 100,
        files: [
          { path: "a.ts", sha256: "abc", bytes: 50, beforeHash: null, existed: false },
          { path: "b.ts", sha256: "def", bytes: 50, beforeHash: null, existed: false },
        ],
        language: "typescript",
      },
      timestamp: "now",
    });
    await bus.emit({
      kind: "artifact.emitted",
      schemaVersion: "v3",
      sequence: 6,
      streamId: "s",
      taskId: "t-em",
      value: {
        filesWritten: 1,
        totalBytes: 30,
        files: [{ path: "c.ts", sha256: "ghi", bytes: 30, beforeHash: null, existed: false }],
        language: "typescript",
      },
      timestamp: "now",
    });
    const packet = reducer.reduce("t-em", 0);
    expect(packet.emissions?.filesWritten).toBe(3);
    expect(packet.emissions?.totalBytes).toBe(130);
    expect(packet.emissions?.files).toHaveLength(3);
  });
});

// ── Reducer: contributor signals tracking ─────────────────────────────────

describe("StateReducer contributing signals", () => {
  it("tracks signal sources correctly", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s",
      taskId: "t-cs",
      value: {
        status: "pass",
        errors: 0,
        warnings: 0,
        filesScanned: 1,
        durationMs: 10,
        details: [],
      },
      timestamp: "t1",
      source: "eslint",
    });
    await bus.emit({
      kind: "verify.types",
      schemaVersion: "v3",
      sequence: 2,
      streamId: "s",
      taskId: "t-cs",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "t2",
      source: "tsc",
    });
    const packet = reducer.reduce("t-cs", 0);
    expect(packet.contributingSignals).toHaveLength(2);
    expect(packet.contributingSignals[0]!.source).toBe("eslint");
    expect(packet.contributingSignals[1]!.source).toBe("tsc");
  });
});

// ── Reducer: resetTask ─────────────────────────────────────────────────────

describe("StateReducer resetTask", () => {
  it("clears stored signals for a task", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s",
      taskId: "t-rst",
      value: {
        status: "fail",
        errors: 5,
        warnings: 0,
        filesScanned: 1,
        durationMs: 10,
        details: [],
      },
      timestamp: "now",
    });
    let packet = reducer.reduce("t-rst", 0);
    expect(packet.verification.lint.errors).toBe(5);

    reducer.resetTask("t-rst");
    packet = reducer.reduce("t-rst", 0);
    expect(packet.verification.lint.errors).toBe(1); // error state default
  });
});
