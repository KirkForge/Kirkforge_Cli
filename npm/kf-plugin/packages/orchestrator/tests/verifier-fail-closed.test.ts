// packages/orchestrator/tests/verifier-fail-closed.test.ts
// Integration test: verifies that a verifier reporting status:"error"
// (e.g. from a missing binary) is treated as a hard failure by the
// reducer, not silently passed through as 0 errors.
//
// Regression test for the fail-open defect: prior to 2026-06-07, the
// tool-tsc and tool-pyright packages returned Result.ok({ errors: 0 })
// when the underlying binary was missing, with status:"skipped" in the
// emitted event. The reducer would then NOT fail the overall verdict
// if the slot was not in policy.required. Now, those emitters return
// status:"error" and Result.err, which the reducer must surface as
// overall = "fail".

import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { StateReducer } from "../src/reducer.js";
import type { VerifierPolicy } from "@kirkforge/correction-core";

const noPolicy: VerifierPolicy = { required: [], advisory: [] };
const typesRequired: VerifierPolicy = { required: ["types"], advisory: [] };

async function emitTypesStatus(
  bus: EventBus,
  taskId: string,
  status: "pass" | "fail" | "skipped" | "error",
  errors: number,
) {
  await bus.emit({
    kind: "verify.types",
    schemaVersion: "v3",
    sequence: 1,
    streamId: taskId,
    taskId,
    value: {
      status,
      errors,
      durationMs: 0,
      details:
        status === "error"
          ? [{ file: "<tsc>", line: 0, code: "VERIFIER_MISSING_BINARY", message: "tsc not found" }]
          : [],
    },
    timestamp: "now",
  });
}

async function emitLintPass(bus: EventBus, taskId: string) {
  await bus.emit({
    kind: "verify.lint",
    schemaVersion: "v3",
    sequence: 1,
    streamId: taskId,
    taskId,
    value: {
      status: "pass",
      errors: 0,
      warnings: 0,
      filesScanned: 1,
      durationMs: 0,
      details: [],
    },
    timestamp: "now",
  });
}

async function emitSecurityPass(bus: EventBus, taskId: string) {
  await bus.emit({
    kind: "verify.security",
    schemaVersion: "v3",
    sequence: 1,
    streamId: taskId,
    taskId,
    value: {
      status: "pass",
      findings: 0,
      critical: 0,
      high: 0,
      filesScanned: 1,
      durationMs: 0,
      details: [],
    },
    timestamp: "now",
  });
}

describe("StateReducer fail-closed semantics", () => {
  it("status:'skipped' with errors:0 + policy.required not in [] -> overall=pass (legitimate skip)", async () => {
    // When the slot is NOT in policy.required, a skip is fine.
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await emitLintPass(bus, "t1");
    await emitTypesStatus(bus, "t1", "skipped", 0);
    await emitSecurityPass(bus, "t1");
    const packet = reducer.reduce("t1", 0, noPolicy);
    expect(packet.verification.overall).toBe("pass");
  });

  it("status:'skipped' with errors:0 + policy.required includes 'types' -> overall=fail", async () => {
    // When the slot IS in policy.required, a skip is a failure.
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await emitLintPass(bus, "t2");
    await emitTypesStatus(bus, "t2", "skipped", 0);
    await emitSecurityPass(bus, "t2");
    const packet = reducer.reduce("t2", 0, typesRequired);
    expect(packet.verification.overall).toBe("fail");
  });

  it("status:'error' (missing binary) -> overall=fail even when not in policy.required", async () => {
    // The fail-closed contract: a verifier that reports status:"error"
    // (e.g. from a missing tsc binary) is treated as a hard failure
    // regardless of policy. This is the defect fix: previously, missing
    // tsc emitted status:"skipped" which was indistinguishable from a
    // legitimate "no files to check" skip.
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await emitLintPass(bus, "t3");
    await emitTypesStatus(bus, "t3", "error", 1);
    await emitSecurityPass(bus, "t3");
    const packet = reducer.reduce("t3", 0, noPolicy);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.verification.types.status).toBe("error");
    expect(packet.verification.types.errors).toBe(1);
  });

  it("status:'error' (missing binary) + policy.required includes 'types' -> overall=fail (defense in depth)", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await emitLintPass(bus, "t4");
    await emitTypesStatus(bus, "t4", "error", 1);
    await emitSecurityPass(bus, "t4");
    const packet = reducer.reduce("t4", 0, typesRequired);
    expect(packet.verification.overall).toBe("fail");
  });

  it("status:'pass' -> overall=pass (happy path)", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await emitLintPass(bus, "t5");
    await emitTypesStatus(bus, "t5", "pass", 0);
    await emitSecurityPass(bus, "t5");
    const packet = reducer.reduce("t5", 0, typesRequired);
    expect(packet.verification.overall).toBe("pass");
  });

  it("status:'fail' -> overall=fail (real type errors)", async () => {
    const bus = new EventBus();
    const reducer = new StateReducer(bus);
    await emitLintPass(bus, "t6");
    await emitTypesStatus(bus, "t6", "fail", 3);
    await emitSecurityPass(bus, "t6");
    const packet = reducer.reduce("t6", 0, typesRequired);
    expect(packet.verification.overall).toBe("fail");
    expect(packet.verification.types.errors).toBe(3);
  });
});
