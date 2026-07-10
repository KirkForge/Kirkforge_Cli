import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { StateReducer } from "../../src/reducer.js";
import type { VerifierPolicy } from "@kirkforge/correction-core";

describe("StateReducer: graph verifier scenarios", () => {
  it("no policy with graph brokenEdges still fails (backward compatible)", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s14",
      taskId: "t14",
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
      streamId: "s14",
      taskId: "t14",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s14",
      taskId: "t14",
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
      streamId: "s14",
      taskId: "t14",
      value: {
        status: "fail",
        edgeCount: 5,
        newEdges: 0,
        brokenEdges: 2,
        cycles: 0,
        durationMs: 10,
      },
      timestamp: "now",
    });
    const packet = reducer.reduce("t14", 0);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.graph.brokenEdges).toBe(2);
  });

  it("graph advisory with brokenEdges produces warn, not fail", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = { required: ["lint", "types", "security"], advisory: ["graph"] };
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s15",
      taskId: "t15",
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
      streamId: "s15",
      taskId: "t15",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s15",
      taskId: "t15",
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
      streamId: "s15",
      taskId: "t15",
      value: {
        status: "fail",
        edgeCount: 5,
        newEdges: 0,
        brokenEdges: 3,
        cycles: 0,
        durationMs: 10,
      },
      timestamp: "now",
    });
    const packet = reducer.reduce("t15", 0, policy);
    expect(packet.verification.overall).toBe("warn");
    expect(packet.graph.brokenEdges).toBe(3);
  });

  it("graph required with brokenEdges fails", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = {
      required: ["lint", "types", "security", "graph"],
      advisory: [],
    };
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s16",
      taskId: "t16",
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
      streamId: "s16",
      taskId: "t16",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s16",
      taskId: "t16",
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
      streamId: "s16",
      taskId: "t16",
      value: {
        status: "fail",
        edgeCount: 5,
        newEdges: 0,
        brokenEdges: 2,
        cycles: 0,
        durationMs: 10,
      },
      timestamp: "now",
    });
    const packet = reducer.reduce("t16", 0, policy);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.graph.brokenEdges).toBe(2);
  });

  it("graph absent from policy with brokenEdges does not affect overall", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    const policy: VerifierPolicy = { required: ["lint", "types"], advisory: [] };
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s17",
      taskId: "t17",
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
      streamId: "s17",
      taskId: "t17",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "state.graph",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s17",
      taskId: "t17",
      value: {
        status: "fail",
        edgeCount: 5,
        newEdges: 0,
        brokenEdges: 4,
        cycles: 0,
        durationMs: 10,
      },
      timestamp: "now",
    });
    const packet = reducer.reduce("t17", 0, policy);
    expect(packet.verification.overall).toBe("pass");
    expect(packet.graph.brokenEdges).toBe(4);
  });
});
