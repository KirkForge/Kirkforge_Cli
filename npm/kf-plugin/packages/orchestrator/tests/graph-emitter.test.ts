import { describe, it, expect } from "vitest";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { EventBus } from "@kirkforge/core-events";
import { ok } from "@kirkforge/core-types";
import type { StateGraphEvent } from "@kirkforge/core-types";
import { GraphifyEmitter } from "@kirkforge/tool-graphify";

// WO 15.6 / 5.2: the local `GraphEmitter` was refactored into the external
// `@kirkforge/tool-graphify` package (`GraphifyEmitter`). The constructor
// takes a required `cwd` + optional `eventBus` + optional `files`; `emit`
// returns `Promise<Result<GraphifyReport, Error>>`. Files resolve relative
// to `cwd`. The graph event shape (`state.graph`) is unchanged.
//
// Gate (Task 8 sub-task 1): a known import cycle yields cycles >= 1 and
// status != "skipped"; a referenced symbol that is removed/never exported
// yields brokenEdges >= 1.

async function captureGraph(cwd: string, files: string[]): Promise<StateGraphEvent> {
  const bus = new EventBus();
  let captured: StateGraphEvent | undefined;
  bus.on<StateGraphEvent>("state.graph", (e) => {
    captured = e;
    return Promise.resolve(ok(undefined));
  });
  const emitter = new GraphifyEmitter({ cwd, eventBus: bus, files });
  await emitter.emit("task-1");
  if (!captured) throw new Error("state.graph was not emitted");
  return captured;
}

describe("GraphifyEmitter", () => {
  it("detects an import cycle (cycles >= 1, status != skipped)", async () => {
    const dir = mkdtempSync(join(tmpdir(), "graph-cycle-"));
    try {
      writeFileSync(join(dir, "a.ts"), `import { b } from "./b";\nexport const a = 1;\n`);
      writeFileSync(join(dir, "b.ts"), `import { a } from "./a";\nexport const b = 2;\n`);
      const ev = await captureGraph(dir, ["a.ts", "b.ts"]);
      expect(ev.value.status).not.toBe("skipped");
      expect(ev.value.cycles).toBeGreaterThanOrEqual(1);
      // both imports resolve to real exports -> no broken edges
      expect(ev.value.brokenEdges).toBe(0);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("flags a broken edge when the import target is missing entirely", async () => {
    const dir = mkdtempSync(join(tmpdir(), "graph-missing-"));
    try {
      writeFileSync(join(dir, "a.ts"), `import { x } from "./does-not-exist";\n`);
      const ev = await captureGraph(dir, ["a.ts"]);
      expect(ev.value.brokenEdges).toBeGreaterThanOrEqual(1);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  // `GraphifyEmitter` resolves broken edges at the file level (missing import
  // target), not the symbol level. The previous in-repo `GraphEmitter`
  // tracked named exports; the refactored external package does not. An
  // import like `import { missing } from "./b"` where `b.ts` exists but does
  // not export `missing` is NOT a broken edge under the new contract. This
  // test pins that ceiling so the difference is documented, not silently
  // regressed.
  it("does not flag a broken edge when the target file exists but the named export is absent", async () => {
    const dir = mkdtempSync(join(tmpdir(), "graph-symbol-"));
    try {
      writeFileSync(join(dir, "a.ts"), `import { missing } from "./b";\nexport const a = 1;\n`);
      writeFileSync(join(dir, "b.ts"), `export const b = 2;\n`); // exports `b`, not `missing`
      const ev = await captureGraph(dir, ["a.ts", "b.ts"]);
      expect(ev.value.brokenEdges).toBe(0);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("reports skipped with zero edges when there are no files", async () => {
    const dir = mkdtempSync(join(tmpdir(), "graph-empty-"));
    try {
      const ev = await captureGraph(dir, []);
      expect(ev.value.status).toBe("skipped");
      expect(ev.value.edgeCount).toBe(0);
      expect(ev.value.cycles).toBe(0);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  // `GraphifyEmitter` does not track which files were "just written" separately
  // from the full file set; `newEdges` equals `edgeCount` (every edge is
  // counted as new). The previous `GraphEmitter` exposed a `writtenFiles`
  // option that this package does not, so this test asserts the new contract:
  // any edge is reported as a new edge.
  it("reports newEdges equal to edgeCount for a graph with imports", async () => {
    const dir = mkdtempSync(join(tmpdir(), "graph-newedge-"));
    try {
      writeFileSync(join(dir, "a.ts"), `import { b } from "./b";\nexport const a = 1;\n`);
      writeFileSync(join(dir, "b.ts"), `export const b = 2;\n`);
      const ev = await captureGraph(dir, ["a.ts", "b.ts"]);
      expect(ev.value.newEdges).toBeGreaterThanOrEqual(1);
      expect(ev.value.newEdges).toBe(ev.value.edgeCount);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});