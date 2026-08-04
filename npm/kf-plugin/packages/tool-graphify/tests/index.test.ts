import { describe, it, expect } from "vitest";
import { GraphifyEmitter } from "../src/index.js";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

describe("GraphifyEmitter", () => {
  it("constructs with cwd option", () => {
    const emitter = new GraphifyEmitter({ cwd: process.cwd() });
    expect(emitter).toBeInstanceOf(GraphifyEmitter);
  });

  it("returns skipped report when no files provided", async () => {
    const emitter = new GraphifyEmitter({ cwd: process.cwd(), files: [] });
    const result = await emitter.emit("test-task-1");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.edgeCount).toBe(0);
      expect(result.value.newEdges).toBe(0);
      expect(result.value.brokenEdges).toBe(0);
    }
  });

  it("detects static imports from TypeScript files", async () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "graphify-test-"));
    try {
      writeFileSync(join(tmpDir, "a.ts"), `import { foo } from "./b";\nexport const x = 1;\n`);
      writeFileSync(join(tmpDir, "b.ts"), `export const foo = 42;\n`);
      const emitter = new GraphifyEmitter({ cwd: tmpDir, files: ["a.ts"] });
      const result = await emitter.emit("test-task-2");
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.edgeCount).toBeGreaterThanOrEqual(1);
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("detects type-only imports", async () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "graphify-test-"));
    try {
      writeFileSync(join(tmpDir, "types.ts"), `export type Foo = { x: number };\n`);
      writeFileSync(
        join(tmpDir, "consumer.ts"),
        `import type { Foo } from "./types";\nconst a: Foo = { x: 1 };\n`,
      );
      const emitter = new GraphifyEmitter({ cwd: tmpDir, files: ["consumer.ts"] });
      const result = await emitter.emit("test-task-3");
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.edgeCount).toBeGreaterThanOrEqual(1);
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("marks node_modules imports as non-broken", async () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "graphify-test-"));
    try {
      writeFileSync(
        join(tmpDir, "app.ts"),
        `import { z } from "zod";\nexport const schema = z.string();\n`,
      );
      const emitter = new GraphifyEmitter({ cwd: tmpDir, files: ["app.ts"] });
      const result = await emitter.emit("test-task-4");
      expect(result.ok).toBe(true);
      if (result.ok) {
        // node_modules imports should not be counted as broken
        expect(result.value.brokenEdges).toBe(0);
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("includes durationMs in report", async () => {
    const emitter = new GraphifyEmitter({ cwd: process.cwd(), files: [] });
    const result = await emitter.emit("test-task-duration");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.durationMs).toBeGreaterThanOrEqual(0);
    }
  });
});
