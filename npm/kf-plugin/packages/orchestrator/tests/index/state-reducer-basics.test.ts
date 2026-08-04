import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { StateReducer } from "../../src/reducer.js";

describe("StateReducer: signal reduction basics", () => {
  it("reduces signals to packet", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s1",
      taskId: "t1",
      value: {
        status: "fail",
        errors: 2,
        warnings: 1,
        filesScanned: 5,
        durationMs: 100,
        details: [],
      },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.types",
      schemaVersion: "v3",
      sequence: 2,
      streamId: "s1",
      taskId: "t1",
      value: { status: "pass", errors: 0, durationMs: 50, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "state.changes",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s1",
      taskId: "t1",
      value: {
        filesChanged: 3,
        paths: ["a.ts", "b.ts"],
        insertions: 10,
        deletions: 2,
        durationMs: 30,
      },
      timestamp: "now",
    });
    const p = reducer.reduce("t1", 0);
    expect(p.verification.lint.errors).toBe(2);
    expect(p.changes.filesChanged).toBe(3);
    expect(p.verification.overall).toBe("fail");
  });

  it("returns pass when clean", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s2",
      taskId: "t2",
      value: {
        status: "pass",
        errors: 0,
        warnings: 0,
        filesScanned: 3,
        durationMs: 10,
        details: [],
      },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.types",
      schemaVersion: "v3",
      sequence: 2,
      streamId: "s2",
      taskId: "t2",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s2",
      taskId: "t2",
      value: {
        status: "pass",
        findings: 0,
        critical: 0,
        high: 0,
        filesScanned: 3,
        durationMs: 10,
        details: [],
      },
      timestamp: "now",
    });
    await bus.emit({
      kind: "state.graph",
      schemaVersion: "v3",
      sequence: 4,
      streamId: "s2",
      taskId: "t2",
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
    expect(reducer.reduce("t2", 0).verification.overall).toBe("pass");
  });

  it("fails closed when verifier signals are missing", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s3",
      taskId: "t3",
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
    expect(reducer.reduce("t3", 0).verification.overall).toBe("fail");
  });

  it("fails closed on explicit verifier error", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s4",
      taskId: "t4",
      value: {
        status: "error",
        error: "eslint config exploded",
        errors: 1,
        warnings: 0,
        filesScanned: 0,
        durationMs: 10,
        details: [],
      },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.types",
      schemaVersion: "v3",
      sequence: 2,
      streamId: "s4",
      taskId: "t4",
      value: { status: "pass", errors: 0, durationMs: 10, details: [] },
      timestamp: "now",
    });
    await bus.emit({
      kind: "verify.security",
      schemaVersion: "v3",
      sequence: 3,
      streamId: "s4",
      taskId: "t4",
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
      streamId: "s4",
      taskId: "t4",
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
    expect(reducer.reduce("t4", 0).verification.overall).toBe("fail");
  });
});
