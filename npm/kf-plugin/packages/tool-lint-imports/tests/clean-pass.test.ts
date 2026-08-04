// kirkforge-lint-disable no-console
import { describe, it, expect, afterAll } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { createImportLintEngine, BUILTIN_PYTHON_RENAMES, BUILTIN_TYPESCRIPT_RENAMES } from "../src/index.js";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";

const BASE_DIR = resolve(tmpdir(), `kirkforge-imports-clean-${Date.now()}`);
let testCounter = 0;

async function setup(files: Record<string, string>) {
  const dir = join(BASE_DIR, `test-${++testCounter}`);
  await mkdir(dir, { recursive: true });
  for (const [name, content] of Object.entries(files)) {
    await writeFile(join(dir, name), content);
  }
  return dir;
}

afterAll(async () => {
  await rm(BASE_DIR, { recursive: true, force: true });
});

describe("tool-lint-imports: clean pass", () => {
  it("reports pass for a directory with no Python or TS files", async () => {
    const dir = await setup({
      "README.md": "# nothing to scan",
      "data.txt": "no imports here",
    });
    const engine = createImportLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("pass");
    expect(result.value.filesScanned).toBe(0);
  });

  it("emits a verify.imports event when an eventBus is provided", async () => {
    const dir = await setup({
      "bad.py": "import PyPDF2\nx = PyPDF2.foo\n",
    });
    const eventBus = new EventBus({ bufferCapacity: 100 });
    const received: unknown[] = [];
    eventBus.on("verify.imports", (e) => {
      received.push(e);
      return Promise.resolve({ ok: true, value: undefined });
    });
    const engine = createImportLintEngine({ cwd: dir, eventBus, languages: ["python"] });
    const result = await engine.emit("task-1");
    expect(result.ok).toBe(true);
    // Give the bus a tick to deliver
    await new Promise((r) => setImmediate(r));
    expect(received.length).toBeGreaterThanOrEqual(1);
    const evt = received[0] as { kind: string; value: { status: string; details: Array<{ oldName: string }> } };
    expect(evt.kind).toBe("verify.imports");
    // Status is "pass" — the imports slot is advisory and never fail-closes
    expect(evt.value.status).toBe("pass");
    expect(evt.value.details.some((d) => d.oldName === "PyPDF2")).toBe(true);
    await eventBus.gracefulShutdown();
  });

  it("exposes curated rename tables", () => {
    expect(Object.keys(BUILTIN_PYTHON_RENAMES).length).toBeGreaterThanOrEqual(10);
    expect(Object.keys(BUILTIN_TYPESCRIPT_RENAMES).length).toBeGreaterThanOrEqual(5);
    // Sanity-check a known entry
    expect(BUILTIN_PYTHON_RENAMES["PyPDF2"]?.replacedBy).toBe("pypdf");
    expect(BUILTIN_TYPESCRIPT_RENAMES["request"]?.replacedBy).toContain("undici");
  });

  it("scans both languages by default", async () => {
    const dir = await setup({
      "py_file.py": "import PyPDF2\n",
      "ts_file.ts": "import request from 'request';\n",
      "md_file.md": "import request from 'request';  // not scanned\n",
    });
    const engine = createImportLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    // .py and .ts both scanned
    expect(result.value.findings).toBeGreaterThanOrEqual(2);
    expect(result.value.details.some((d) => d.oldName === "PyPDF2")).toBe(true);
    expect(result.value.details.some((d) => d.oldName === "request")).toBe(true);
  });

  it("respects a custom rename table override", async () => {
    const dir = await setup({
      "script.py": "import veryoldmod\n",
    });
    const custom = {
      veryoldmod: { replacedBy: "newmod", deprecatedSince: "2020", reason: "Test override" },
    };
    const engine = createImportLintEngine({
      cwd: dir,
      languages: ["python"],
      pythonRenames: custom,
    });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.oldName === "veryoldmod")).toBe(true);
    expect(result.value.details.some((d) => d.newName === "newmod")).toBe(true);
  });
});
