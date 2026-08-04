import { describe, it, expect } from "vitest";
import { toolNames, buildCorrectionPrompt } from "../src/index.js";
import type {
  ReducedStatePacket,
  TaskLanguage,
  CorrectionConfig,
  CorrectionDecision,
} from "../src/index.js";

function makePacket(overrides?: Partial<ReducedStatePacket>): ReducedStatePacket {
  return {
    taskId: "test-task",
    turn: 0,
    ts: new Date().toISOString(),
    verification: {
      lint: { errors: 3, warnings: 1 },
      types: { errors: 2 },
      security: { findings: 2, critical: 0, high: 1 },
      overall: "fail",
    },
    changes: {
      filesChanged: 2,
      paths: ["src/index.ts", "src/utils.ts"],
      insertions: 10,
      deletions: 5,
    },
    graph: { edgeCount: 5, newEdges: 0, brokenEdges: 0, cycles: 0 },
    contributingSignals: [],
    ...overrides,
  };
}

describe("toolNames()", () => {
  it("returns eslint/tsc/secdev for typescript", () => {
    expect(toolNames("typescript")).toEqual({
      lint: "KirkForge TypeScript lint engine",
      types: "tsc",
      security: "KirkForge TypeScript lint engine (safety rules)",
    });
  });

  it("returns eslint/tsc/secdev for javascript", () => {
    expect(toolNames("javascript")).toEqual({
      lint: "KirkForge TypeScript lint engine",
      types: "tsc",
      security: "KirkForge TypeScript lint engine (safety rules)",
    });
  });

  it("returns ruff/pyright/bandit for python", () => {
    expect(toolNames("python")).toEqual({
      lint: "KirkForge Python lint engine",
      types: "pyright",
      security: "KirkForge Python lint engine (safety rules)",
    });
  });

  it("returns shellcheck/bash -n/secdev for shell", () => {
    expect(toolNames("shell")).toEqual({
      lint: "KirkForge shell lint engine",
      types: "bash -n",
      security: "KirkForge shell lint engine (safety rules)",
    });
  });

  it("returns generic names for undefined/unknown language", () => {
    expect(toolNames(undefined)).toEqual({
      lint: "lint",
      types: "type-check",
      security: "security scanner",
    });
  });
});

describe("buildCorrectionPrompt()", () => {
  it("includes correct tool names for python", () => {
    const prompt = buildCorrectionPrompt(makePacket(), "python");
    expect(prompt).toContain("KirkForge Python lint engine");
    expect(prompt).toContain("pyright");
    expect(prompt).toContain("safety rules");
  });

  it("includes correct tool names for typescript", () => {
    const prompt = buildCorrectionPrompt(makePacket(), "typescript");
    expect(prompt).toContain("KirkForge TypeScript lint engine");
    expect(prompt).toContain("tsc");
    expect(prompt).toContain("safety rules");
  });

  it("includes lint error count", () => {
    const prompt = buildCorrectionPrompt(
      makePacket({
        verification: { ...makePacket().verification, lint: { errors: 7, warnings: 0 } },
      }),
      "typescript",
    );
    expect(prompt).toContain("7 lint errors");
  });

  it("includes type error count", () => {
    const prompt = buildCorrectionPrompt(
      makePacket({ verification: { ...makePacket().verification, types: { errors: 4 } } }),
      "typescript",
    );
    expect(prompt).toContain("4 type errors");
  });

  it("includes security finding count", () => {
    const prompt = buildCorrectionPrompt(
      makePacket({
        verification: {
          ...makePacket().verification,
          security: { findings: 5, critical: 0, high: 3 },
        },
      }),
      "typescript",
    );
    expect(prompt).toContain("3 high-severity security findings");
  });

  it("includes broken import count", () => {
    const prompt = buildCorrectionPrompt(
      makePacket({ graph: { edgeCount: 10, newEdges: 0, brokenEdges: 6, cycles: 0 } }),
      "typescript",
    );
    expect(prompt).toContain("6 broken import edges");
  });

  it("includes artifact enforcement when present", () => {
    const prompt = buildCorrectionPrompt(
      makePacket({
        artifactEnforcement: {
          blocked: 2,
          blockedPaths: [
            { path: "output.ts", reason: "python task cannot emit output.ts" },
            { path: "/etc/passwd", reason: "absolute path is not allowed" },
          ],
          status: "fail",
        },
      }),
      "python",
    );
    expect(prompt).toContain("Artifact policy blocked");
    expect(prompt).toContain("output.ts: python task cannot emit output.ts");
    expect(prompt).toContain("/etc/passwd: absolute path is not allowed");
    expect(prompt).toContain("file_write");
    expect(prompt).toContain("content_b64");
  });

  it("does not include artifact enforcement line when absent", () => {
    const prompt = buildCorrectionPrompt(makePacket(), "typescript");
    expect(prompt).not.toContain("Artifact policy blocked");
  });

  it("fallback still returns useful text when no issues exist", () => {
    const prompt = buildCorrectionPrompt(
      makePacket({
        verification: {
          lint: { errors: 0, warnings: 0 },
          types: { errors: 0 },
          security: { findings: 0, critical: 0, high: 0 },
          overall: "pass",
        },
        graph: { edgeCount: 0, newEdges: 0, brokenEdges: 0, cycles: 0 },
      }),
      "typescript",
    );
    expect(prompt).toContain("verification");
    expect(prompt.length).toBeGreaterThan(0);
    expect(prompt).not.toContain("lint errors");
    expect(prompt).not.toContain("type errors");
    expect(prompt).not.toContain("security findings");
  });

  it("lists changed file paths", () => {
    const prompt = buildCorrectionPrompt(makePacket(), "typescript");
    expect(prompt).toContain("src/index.ts");
    expect(prompt).toContain("src/utils.ts");
  });

  it("shows unknown when paths are empty", () => {
    const prompt = buildCorrectionPrompt(
      makePacket({
        changes: { filesChanged: 0, paths: [], insertions: 0, deletions: 0 },
      }),
      "typescript",
    );
    expect(prompt).toContain("unknown");
  });

  it("uses double dash, not em dash, in output", () => {
    const prompt = buildCorrectionPrompt(makePacket(), "typescript");
    expect(prompt).toContain("-- ");
    expect(prompt).not.toContain("\u2014");
  });

  it("includes missing required verifiers in prompt", () => {
    const packet = makePacket({
      verifierPolicy: {
        required: ["lint", "types", "security"],
        advisory: ["graph"],
        missingRequired: ["types"],
        skippedRequired: [],
      },
    });
    const prompt = buildCorrectionPrompt(packet, "typescript");
    expect(prompt).toContain("Required verifier");
    expect(prompt).toContain("missing: types");
  });

  it("includes skipped required verifiers in prompt", () => {
    const packet = makePacket({
      verifierPolicy: {
        required: ["lint", "types", "security"],
        advisory: ["graph"],
        missingRequired: [],
        skippedRequired: ["security"],
      },
    });
    const prompt = buildCorrectionPrompt(packet, "python");
    expect(prompt).toContain("Required verifier");
    expect(prompt).toContain("skipped: security");
  });

  it("includes both missing and skipped required verifiers in prompt", () => {
    const packet = makePacket({
      verifierPolicy: {
        required: ["lint", "types", "security"],
        advisory: ["graph"],
        missingRequired: ["lint", "types"],
        skippedRequired: ["security"],
      },
    });
    const prompt = buildCorrectionPrompt(packet, "typescript");
    expect(prompt).toContain("missing: lint, types");
    expect(prompt).toContain("skipped: security");
  });

  it("does not include verifier policy line when absent", () => {
    const prompt = buildCorrectionPrompt(makePacket(), "typescript");
    expect(prompt).not.toContain("Required verifier");
  });

  it("uses advisory wording for graph brokenEdges when graph is advisory", () => {
    const packet = makePacket({
      graph: { edgeCount: 5, newEdges: 0, brokenEdges: 3, cycles: 0 },
      verifierPolicy: {
        required: ["lint", "types", "security"],
        advisory: ["graph"],
        missingRequired: [],
        skippedRequired: [],
      },
    });
    const prompt = buildCorrectionPrompt(packet, "typescript");
    expect(prompt).toContain("Graph advisory finding: 3 broken import edges");
    expect(prompt).not.toContain("Fix 3 broken import edges");
  });

  it("uses blocker wording for graph brokenEdges when graph is required", () => {
    const packet = makePacket({
      graph: { edgeCount: 5, newEdges: 0, brokenEdges: 2, cycles: 0 },
      verifierPolicy: {
        required: ["lint", "types", "security", "graph"],
        advisory: [],
        missingRequired: [],
        skippedRequired: [],
      },
    });
    const prompt = buildCorrectionPrompt(packet, "typescript");
    expect(prompt).toContain("Fix 2 broken import edges");
    expect(prompt).not.toContain("Graph advisory finding");
  });

  it("uses blocker wording for graph brokenEdges when no policy", () => {
    const packet = makePacket({
      graph: { edgeCount: 5, newEdges: 0, brokenEdges: 2, cycles: 0 },
    });
    const prompt = buildCorrectionPrompt(packet, "typescript");
    expect(prompt).toContain("Fix 2 broken import edges");
    expect(prompt).not.toContain("Graph advisory finding");
  });

  it("includes critical security findings in prompt", () => {
    const packet = makePacket({
      verification: {
        ...makePacket().verification,
        security: { findings: 3, critical: 2, high: 1 },
      },
    });
    const prompt = buildCorrectionPrompt(packet, "python");
    expect(prompt).toContain("2 critical security findings");
    expect(prompt).toContain("safety rules");
  });

  it("includes both critical and high security findings when both present", () => {
    const packet = makePacket({
      verification: {
        ...makePacket().verification,
        security: { findings: 5, critical: 2, high: 3 },
      },
    });
    const prompt = buildCorrectionPrompt(packet, "typescript");
    expect(prompt).toContain("2 critical security findings");
    expect(prompt).toContain("3 high-severity security findings");
  });
});

describe("exported types compile-time", () => {
  it("ReducedStatePacket type is usable", () => {
    const packet: ReducedStatePacket = makePacket();
    expect(packet.taskId).toBe("test-task");
    expect(packet.verification.overall).toBe("fail");
  });

  it("TaskLanguage type narrows to known languages", () => {
    const langs: TaskLanguage[] = [
      "typescript",
      "javascript",
      "python",
      "shell",
      "cpp",
      "c",
      "rust",
      "go",
      "sql",
      "text",
    ];
    expect(langs.length).toBe(10);
    const ts: TaskLanguage = "typescript";
    expect(ts).toBe("typescript");
  });

  it("CorrectionConfig type is usable", () => {
    const config: CorrectionConfig = { maxCorrections: 3 };
    expect(config.maxCorrections).toBe(3);
  });

  it("CorrectionDecision type is usable", () => {
    const decision: CorrectionDecision = {
      action: "correct",
      rationale: "test",
      packet: makePacket(),
      correctionCount: 1,
      workerTokens: 100,
      sessionTokens: 200,
    };
    expect(decision.action).toBe("correct");
  });
});
