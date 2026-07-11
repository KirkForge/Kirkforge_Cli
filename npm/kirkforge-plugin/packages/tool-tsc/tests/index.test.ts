// packages/tool-tsc/tests/index.test.ts
// Behavior contract for TscEmitter:
// - No tsconfig.json  -> status:"skipped", Result.ok (legitimate: nothing to type-check)
// - tsconfig.json present, tsc missing (ENOENT) -> status:"error", Result.err (FAIL-CLOSED)
// - tsc present, real errors -> status:"fail", Result.ok with errors>0
// - tsc present, no errors -> status:"pass", Result.ok with errors=0
//
// Regression tests for the fail-open bug: prior versions returned
// Result.ok({ errors: 0 }) on ENOENT, which let environments without
// tsc installed pass type-checking on every task. That is the
// "verifiers fail OPEN when the tool binary is missing" defect.

import { describe, it, expect } from "vitest";
import { TscEmitter } from "../src/index.js";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

describe("TscEmitter", () => {
  it("constructs with cwd option", () => {
    const emitter = new TscEmitter({ cwd: process.cwd() });
    expect(emitter).toBeInstanceOf(TscEmitter);
  });

  it("returns skipped report when tsconfig.json is missing", async () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "tsc-test-"));
    try {
      const emitter = new TscEmitter({ cwd: tmpDir });
      const result = await emitter.emit("test-task-1");
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.errors).toBe(0);
        expect(result.value.details).toEqual([]);
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("FAILS CLOSED when tsc binary is missing (ENOENT)", async () => {
    // Regression: prior versions returned ok({ errors: 0 }) here, which is the
    // fail-open defect. A missing tsc binary means the verifier did NOT run.
    const tmpDir = mkdtempSync(join(tmpdir(), "tsc-test-"));
    try {
      // Create a real tsconfig.json so the emitter attempts the spawn
      writeFileSync(join(tmpDir, "tsconfig.json"), '{"compilerOptions":{"strict":true}}');
      // Force a command that does not exist; even if a bundled `typescript` is
      // available, the explicit command override must be honored and produce ENOENT.
      const emitter = new TscEmitter({
        cwd: tmpDir,
        command: "definitely-not-tsc-xyz-12345",
      });
      const result = await emitter.emit("test-task-enoent");
      // FAIL-CLOSED: result must be err, not ok with errors:0
      expect(result.ok).toBe(false);
      if (!result.ok) {
        expect(result.error.message).toMatch(/tsc binary not found|ENOENT/);
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("includes durationMs in report", async () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "tsc-test-"));
    try {
      const emitter = new TscEmitter({ cwd: tmpDir });
      const result = await emitter.emit("test-task-duration");
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.durationMs).toBeGreaterThanOrEqual(0);
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("returns taskId in report", async () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "tsc-test-"));
    try {
      const emitter = new TscEmitter({ cwd: tmpDir });
      const result = await emitter.emit("my-custom-task-id");
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.taskId).toBe("my-custom-task-id");
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });
});
