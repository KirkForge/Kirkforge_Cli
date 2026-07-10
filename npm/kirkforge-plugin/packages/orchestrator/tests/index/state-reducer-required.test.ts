import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { StateReducer } from "../../src/reducer.js";
import type { VerifierPolicy } from "@kirkforge/correction-core";

describe("StateReducer: required verifier (with policy)", () => {
  it("fails when required verifier is missing (with policy)", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = { required: ["lint", "types"], advisory: ["graph"] };
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s7",
      taskId: "t7",
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
    const packet = reducer.reduce("t7", 0, policy);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.verifierPolicy).toBeDefined();
    expect(packet.verifierPolicy?.missingRequired).toContain("types");
    expect(packet.verifierPolicy?.skippedRequired).toEqual([]);
  });

  it("fails when required verifier is skipped (with policy)", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = { required: ["lint", "types"], advisory: ["graph"] };
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s8",
      taskId: "t8",
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
      streamId: "s8",
      taskId: "t8",
      value: { status: "skipped", errors: 0, durationMs: 0, details: [] },
      timestamp: "now",
    });
    const packet = reducer.reduce("t8", 0, policy);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.verifierPolicy?.skippedRequired).toContain("types");
    expect(packet.verifierPolicy?.missingRequired).toEqual([]);
  });
});
