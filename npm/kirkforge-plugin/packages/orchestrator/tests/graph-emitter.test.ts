import { describe, it, expect } from "vitest";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { EventBus } from "@kirkforge/core-events";
import { ok } from "@kirkforge/core-types";
import type { StateGraphEvent } from "@kirkforge/core-types";
import { GraphEmitter } from "../src/graph-emitter.js";

// Gate (Task 8 sub-task 1): real graph emitter — a known import cycle yields
// cycles >= 1 and status != "skipped"; a referenced symbol that is removed/never
// exported yields brokenEdges >= 1.

async function captureGraph(files: string[], writtenFiles?: string[]): Promise<StateGraphEvent> {
  const bus = new EventBus();
  let captured: StateGraphEvent | undefined;
  bus.on<StateGraphEvent>("state.graph", (e) => {
    captured = e;
    return Promise.resolve(ok(undefined));
  });
  const emitter = new GraphEmitter({ eventBus: bus, files, writtenFiles });
  await emitter.emit("task-1");
  if (!captured) throw new Error("state.graph was not emitted");
  return captured;
}

describe("GraphEmitter", () => {
  it("detects an import cycle (cycles >= 1, status != skipped)", async () => {
    const dir = mkdtempSync(join(tmpdir(), "graph-cycle-"));
    try {
      writeFileSync(join(dir, "a.ts"), `import { b } from "./b";\nexport const a = 1;\n`);
      writeFileSync(join(dir, "b.ts"), `import { a } from "./a";\nexport const b = 2;\n`);
      const a = join(dir, "a.ts");
      const b = join(dir, "b.ts");
      const ev = await captureGraph([a, b]);
      expect(ev.value.status).not.toBe("skipped");
      expect(ev.value.cycles).toBeGreaterThanOrEqual(1);
      // both imports resolve to real exports -> no broken edges
      expect(ev.value.brokenEdges).toBe(0);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("flags a broken edge when a referenced symbol is not exported", async () => {
    const dir = mkdtempSync(join(tmpdir(), "graph-broken-"));
    try {
      writeFileSync(join(dir, "a.ts"), `import { missing } from "./b";\nexport const a = 1;\n`);
      writeFileSync(join(dir, "b.ts"), `export const b = 2;\n`); // exports `b`, not `missing`
      const a = join(dir, "a.ts");
      const b = join(dir, "b.ts");
      const ev = await captureGraph([a, b]);
      expect(ev.value.brokenEdges).toBeGreaterThanOrEqual(1);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("flags a broken edge when the import target is missing entirely", async () => {
    const dir = mkdtempSync(join(tmpdir(), "graph-missing-"));
    try {
      writeFileSync(join(dir, "a.ts"), `import { x } from "./does-not-exist";\n`);
      const a = join(dir, "a.ts");
      const ev = await captureGraph([a]);
      expect(ev.value.brokenEdges).toBeGreaterThanOrEqual(1);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("reports skipped with zero edges when there are no files", async () => {
    const ev = await captureGraph([]);
    expect(ev.value.status).toBe("skipped");
    expect(ev.value.edgeCount).toBe(0);
    expect(ev.value.cycles).toBe(0);
  });

  it("counts newEdges touching written files", async () => {
    const dir = mkdtempSync(join(tmpdir(), "graph-newedge-"));
    try {
      writeFileSync(join(dir, "a.ts"), `import { b } from "./b";\nexport const a = 1;\n`);
      writeFileSync(join(dir, "b.ts"), `export const b = 2;\n`);
      const a = join(dir, "a.ts");
      const b = join(dir, "b.ts");
      const ev = await captureGraph([a, b], [a]); // a was just written
      expect(ev.value.newEdges).toBeGreaterThanOrEqual(1);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});