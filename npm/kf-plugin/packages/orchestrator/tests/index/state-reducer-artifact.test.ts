import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { StateReducer } from "../../src/reducer.js";

describe("StateReducer: artifact.blocked enforcement", () => {
  it("fails closed when artifact.blocked signal exists", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s5",
      taskId: "t5",
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
      streamId: "s5",
      taskId: "t5",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s5",
      taskId: "t5",
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
      streamId: "s5",
      taskId: "t5",
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
      kind: "artifact.blocked",
      schemaVersion: "v3",
      sequence: 5,
      streamId: "s5",
      taskId: "t5",
      value: { blockedPaths: [{ path: "output.ts", reason: "python task cannot emit output.ts" }] },
      timestamp: "now",
    });
    const packet = reducer.reduce("t5", 0);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.artifactEnforcement).toBeDefined();
    expect(packet.artifactEnforcement?.status).toBe("fail");
    expect(packet.artifactEnforcement?.blocked).toBe(1);
    expect(packet.artifactEnforcement?.blockedPaths[0]?.path).toBe("output.ts");
    expect(packet.artifactEnforcement?.blockedPaths[0]?.reason).toBe(
      "python task cannot emit output.ts",
    );
  });
});
