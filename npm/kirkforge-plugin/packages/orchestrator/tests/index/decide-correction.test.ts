import { describe, it, expect } from "vitest";
import { decideCorrection } from "../../src/correction-loop.js";

describe("decideCorrection", () => {
  const okPacket = {
    taskId: "t1",
    turn: 0,
    ts: "now",
    changes: { filesChanged: 1, paths: ["a.ts"], insertions: 5, deletions: 0 },
    graph: { edgeCount: 3, newEdges: 0, brokenEdges: 0, cycles: 0 },
    verification: {
      lint: { errors: 0, warnings: 0 },
      types: { errors: 0 },
      security: { findings: 0, critical: 0, high: 0 },
      overall: "pass" as const,
    },
    contributingSignals: [],
  };
  it("accepts clean", () =>
    expect(decideCorrection(okPacket, 0, 3, 100, 100, 0).action).toBe("accept"));
  it("escalates critical security (no policy, backward compat)", () =>
    expect(
      decideCorrection(
        {
          ...okPacket,
          verification: {
            ...okPacket.verification,
            security: { findings: 1, critical: 1, high: 0 },
          },
        },
        0,
        3,
        100,
        100,
        0,
      ).action,
    ).toBe("escalate"));
  it("escalates critical security (security required)", () => {
    const packet = {
      ...okPacket,
      verification: { ...okPacket.verification, security: { findings: 1, critical: 1, high: 0 } },
      verifierPolicy: {
        required: ["lint", "types", "security"],
        advisory: [] as string[],
        missingRequired: [] as string[],
        skippedRequired: [] as string[],
      },
    };
    expect(decideCorrection(packet, 0, 3, 100, 100, 0).action).toBe("escalate");
  });
  it("corrects critical security instead of escalating (security advisory)", () => {
    const packet = {
      ...okPacket,
      verification: {
        ...okPacket.verification,
        security: { findings: 2, critical: 1, high: 0 },
        overall: "fail" as const,
      },
      verifierPolicy: {
        required: ["lint", "types"],
        advisory: ["security"],
        missingRequired: [] as string[],
        skippedRequired: [] as string[],
      },
    };
    const d = decideCorrection(packet, 0, 3, 100, 100, 0);
    expect(d.action).toBe("correct");
    expect(d.correctionPrompt).toBeTruthy();
  });
  it("corrects critical security instead of escalating (security absent from policy)", () => {
    const packet = {
      ...okPacket,
      verification: {
        ...okPacket.verification,
        security: { findings: 2, critical: 1, high: 0 },
        overall: "fail" as const,
      },
      verifierPolicy: {
        required: ["lint", "types"],
        advisory: ["graph"],
        missingRequired: [] as string[],
        skippedRequired: [] as string[],
      },
    };
    const d = decideCorrection(packet, 0, 3, 100, 100, 0);
    expect(d.action).toBe("correct");
    expect(d.correctionPrompt).toBeTruthy();
  });
  it("escalates broken edges (no policy)", () =>
    expect(
      decideCorrection(
        { ...okPacket, graph: { ...okPacket.graph, brokenEdges: 1 } },
        0,
        3,
        100,
        100,
        0,
      ).action,
    ).toBe("escalate"));
  it("escalates broken edges (graph required)", () => {
    const packet = {
      ...okPacket,
      graph: { ...okPacket.graph, brokenEdges: 2 },
      verifierPolicy: {
        required: ["lint", "types", "security", "graph"],
        advisory: [] as string[],
        missingRequired: [] as string[],
        skippedRequired: [] as string[],
      },
    };
    expect(decideCorrection(packet, 0, 3, 100, 100, 0).action).toBe("escalate");
  });
  it("corrects broken edges (graph advisory) instead of escalating", () => {
    const packet = {
      ...okPacket,
      graph: { ...okPacket.graph, brokenEdges: 3 },
      verification: { ...okPacket.verification, overall: "warn" as const },
      verifierPolicy: {
        required: ["lint", "types", "security"],
        advisory: ["graph"],
        missingRequired: [] as string[],
        skippedRequired: [] as string[],
      },
    };
    const d = decideCorrection(packet, 0, 3, 100, 100, 0);
    expect(d.action).toBe("correct");
    expect(d.correctionPrompt).toBeTruthy();
  });
  it("does not escalate broken edges when graph absent from policy", () => {
    const packet = {
      ...okPacket,
      graph: { ...okPacket.graph, brokenEdges: 4 },
      verification: { ...okPacket.verification, overall: "warn" as const },
      verifierPolicy: {
        required: ["lint", "types"],
        advisory: [] as string[],
        missingRequired: [] as string[],
        skippedRequired: [] as string[],
      },
    };
    const d = decideCorrection(packet, 0, 3, 100, 100, 0);
    expect(d.action).toBe("correct");
  });
  it("corrects lint errors", () => {
    const d = decideCorrection(
      {
        ...okPacket,
        verification: {
          ...okPacket.verification,
          lint: { errors: 2, warnings: 0 },
          overall: "fail" as const,
        },
      },
      0,
      3,
      100,
      100,
      0,
    );
    expect(d.action).toBe("correct");
    expect(d.correctionPrompt).toBeTruthy();
  });
  it("escalates max corrections", () =>
    expect(
      decideCorrection(
        {
          ...okPacket,
          verification: {
            ...okPacket.verification,
            lint: { errors: 1, warnings: 0 },
            overall: "fail" as const,
          },
        },
        3,
        3,
        100,
        100,
        0,
      ).action,
    ).toBe("escalate"));

  it("taskPass=true accepts even with other conditions", () => {
    expect(decideCorrection(okPacket, 0, 3, 100, 100, 0, undefined, undefined, true).action).toBe(
      "accept",
    );
  });
  it("taskPass=false corrects when verifier passes and corrections remain", () => {
    const result = decideCorrection(okPacket, 0, 3, 100, 100, 0, undefined, undefined, false);
    expect(result.action).toBe("correct");
    expect(result.rationale).toContain("external validator failed");
  });
  it("taskPass=false escalates when corrections exhausted", () => {
    const result = decideCorrection(okPacket, 3, 3, 100, 100, 0, undefined, undefined, false);
    expect(result.action).toBe("escalate");
    expect(result.rationale).toContain("external validator failed");
  });
  it("taskPass=false escalates when cost exceeded", () => {
    const result = decideCorrection(okPacket, 0, 3, 100, 100, 10, 5, undefined, false);
    expect(result.action).toBe("escalate");
    expect(result.rationale).toContain("external validator failed");
  });
  it("taskPass=false never accepts regardless of verifier pass", () => {
    const result = decideCorrection(okPacket, 0, 3, 100, 100, 0, undefined, undefined, false);
    expect(result.action).not.toBe("accept");
  });
  it("taskPass=undefined falls through to verifier logic (no validator)", () => {
    expect(
      decideCorrection(okPacket, 0, 3, 100, 100, 0, undefined, undefined, undefined).action,
    ).toBe("accept");
  });
});
