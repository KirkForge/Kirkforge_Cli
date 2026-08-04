import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { StateReducer } from "../../src/reducer.js";
import type { VerifierPolicy } from "@kirkforge/correction-core";

describe("StateReducer: advisory verifiers and no-policy behavior", () => {
  it("advisory skipped graph does not fail by itself when required verifiers pass", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = { required: ["lint", "types", "security"], advisory: ["graph"] };
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s9",
      taskId: "t9",
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
      streamId: "s9",
      taskId: "t9",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s9",
      taskId: "t9",
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
      streamId: "s9",
      taskId: "t9",
      value: {
        status: "skipped",
        edgeCount: 0,
        newEdges: 0,
        brokenEdges: 0,
        cycles: 0,
        durationMs: 0,
      },
      timestamp: "now",
    });
    const packet = reducer.reduce("t9", 0, policy);
    expect(packet.verification.overall).toBe("pass");
    expect(packet.verifierPolicy?.missingRequired).toEqual([]);
    expect(packet.verifierPolicy?.skippedRequired).toEqual([]);
  });

  it("no policy preserves old fail-closed behavior (missing verifiers cause error status)", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s10",
      taskId: "t10",
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
    const packet = reducer.reduce("t10", 0);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.verifierPolicy).toBeUndefined();
  });

  it("missing advisory slots do not force fail when policy is provided", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = { required: ["lint", "types"], advisory: ["security", "graph"] };
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s11",
      taskId: "t11",
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
      streamId: "s11",
      taskId: "t11",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    const packet = reducer.reduce("t11", 0, policy);
    expect(packet.verification.overall).toBe("pass");
    expect(packet.verifierPolicy?.missingRequired).toEqual([]);
    expect(packet.verifierPolicy?.skippedRequired).toEqual([]);
    expect(packet.verification.security.status).toBe("skipped");
    expect(packet.verification.lint.errors).toBe(0);
    expect(packet.verification.types.errors).toBe(0);
  });

  it("advisory verifier with critical findings still fails even when advisory", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = { required: ["lint", "types"], advisory: ["security"] };
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s12",
      taskId: "t12",
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
      streamId: "s12",
      taskId: "t12",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s12",
      taskId: "t12",
      value: {
        status: "fail",
        findings: 3,
        critical: 1,
        high: 0,
        filesScanned: 1,
        durationMs: 10,
        details: [],
      },
      timestamp: "now",
    });
    const packet = reducer.reduce("t12", 0, policy);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.verification.security.critical).toBe(1);
  });

  it("no policy with only passing lint/types still fails (fail-closed)", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s13",
      taskId: "t13",
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
      streamId: "s13",
      taskId: "t13",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    const packet = reducer.reduce("t13", 0);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.verifierPolicy).toBeUndefined();
  });
});
