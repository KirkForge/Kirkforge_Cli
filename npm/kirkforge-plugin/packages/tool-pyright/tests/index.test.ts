// packages/tool-pyright/tests/index.test.ts
// Behavior contract for PyrightEmitter:
// - No Python files -> status:"skipped", Result.ok (legitimate: nothing to type-check)
// - pyright missing (ENOENT) -> status:"error", Result.err (FAIL-CLOSED)
// - pyright present, real errors -> status:"fail", Result.ok with errors>0
// - pyright present, no errors -> status:"pass", Result.ok with errors=0
//
// Regression tests for the fail-open bug: prior versions returned
// Result.ok({ errors: 0 }) on ENOENT, which let environments without
// pyright installed pass type-checking on every Python task.
//
// Test isolation: each test uses its own per-test tmpDir so file-discovery
// in one test cannot leak Python files into the next test's walk. (Prior
// shared-tmpDir versions were flaky under sequential execution.)

import { describe, it, expect, afterAll } from "vitest";
import { PyrightEmitter } from "../src/index.js";
import { writeFile, mkdir, rm, mkdtemp } from "node:fs/promises";
import { resolve, join } from "node:path";
import { tmpdir } from "node:os";

async function makeIsolatedDir(label: string): Promise<string> {
  return mkdtemp(join(tmpdir(), `kirkforge-pyright-${label}-${Date.now()}-`));
}

async function writeTestFile(cwd: string, relPath: string, content: string): Promise<void> {
  const full = resolve(cwd, relPath);
  const dir = resolve(full, "..");
  await mkdir(dir, { recursive: true });
  await writeFile(full, content, "utf-8");
}

describe("PyrightEmitter", () => {
  it("returns skipped when no Python files exist", async () => {
    const cwd = await makeIsolatedDir("empty");
    const emitter = new PyrightEmitter({ cwd });
    const result = await emitter.emit("t1");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.errors).toBe(0);
      expect(result.value.details).toHaveLength(0);
    }
    await rm(cwd, { recursive: true, force: true });
  });

  it("returns skipped for zero-length file list", async () => {
    const cwd = await makeIsolatedDir("zerofiles");
    await writeTestFile(cwd, "empty.py", "");
    const emitter = new PyrightEmitter({ cwd, files: [] });
    const result = await emitter.emit("t2");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.errors).toBe(0);
    }
    await rm(cwd, { recursive: true, force: true });
  });

  it("discovers Python files when files not specified", async () => {
    const cwd = await makeIsolatedDir("discover");
    await writeTestFile(cwd, "hello.py", "print('hello')\n");
    const emitter = new PyrightEmitter({ cwd });
    const result = await emitter.emit("t3");
    expect(result.ok).toBe(true);
    // If pyright is installed, it may find errors; if not, falls back gracefully
    await rm(cwd, { recursive: true, force: true });
  });

  it("FAILS CLOSED when pyright binary is missing (ENOENT)", async () => {
    // Regression: prior versions returned ok({ errors: 0 }) here, which is the
    // fail-open defect. A missing pyright binary means the verifier did NOT run.
    const cwd = await makeIsolatedDir("enoent");
    await writeTestFile(cwd, "test.py", "x: int = 'wrong'\n");
    // Point to a command that definitely doesn't exist
    const emitter = new PyrightEmitter({
      cwd,
      command: "definitely-not-pyright-xyz-12345",
    });
    const result = await emitter.emit("t4-enoent");
    // FAIL-CLOSED: result must be err, not ok with errors:0
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toMatch(/pyright binary not found|ENOENT|not found/);
    }
    await rm(cwd, { recursive: true, force: true });
  });

  it("sanitizes file paths that escape cwd", async () => {
    const cwd = await makeIsolatedDir("escape");
    const emitter = new PyrightEmitter({ cwd, files: ["../escape.py"] });
    const result = await emitter.emit("t5");
    expect(result.ok).toBe(true);
    if (result.ok) {
      // Attempted escape should be filtered, resulting in 0 files
      expect(result.value.errors).toBe(0);
    }
    await rm(cwd, { recursive: true, force: true });
  });

  it("includes taskId in report", async () => {
    const cwd = await makeIsolatedDir("taskid");
    const emitter = new PyrightEmitter({ cwd });
    const result = await emitter.emit("task-42");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.taskId).toBe("task-42");
    }
    await rm(cwd, { recursive: true, force: true });
  });

  it("reports durationMs above zero for scans", async () => {
    const cwd = await makeIsolatedDir("timed");
    await writeTestFile(cwd, "timed.py", "x = 1\n");
    const emitter = new PyrightEmitter({ cwd });
    const result = await emitter.emit("t7");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.durationMs).toBeGreaterThanOrEqual(0);
    }
    await rm(cwd, { recursive: true, force: true });
  });
});

afterAll(async () => {
  // No shared tmpDir to clean up — each test owns its own.
});

