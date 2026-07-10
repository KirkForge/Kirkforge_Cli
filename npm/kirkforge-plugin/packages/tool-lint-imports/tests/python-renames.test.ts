// kirkforge-lint-disable no-console
import { describe, it, expect, afterAll } from "vitest";
import { createImportLintEngine } from "../src/index.js";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";

const BASE_DIR = resolve(tmpdir(), `kirkforge-imports-py-${Date.now()}`);
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

describe("tool-lint-imports: python", () => {
  it("flags PyPDF2 import", async () => {
    const dir = await setup({
      "bad.py": "import PyPDF2\ndef extract(p): return PyPDF2.PdfReader(p)\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["python"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    // Advisory: status stays "pass"; findings count is what surfaces the issue
    expect(result.value.status).toBe("pass");
    expect(result.value.findings).toBeGreaterThanOrEqual(1);
    expect(result.value.details.some((d) => d.oldName === "PyPDF2")).toBe(true);
    expect(result.value.details.some((d) => d.newName === "pypdf")).toBe(true);
  });

  it("flags urllib2 (Python 2 leftover)", async () => {
    const dir = await setup({
      "legacy.py": "import urllib2\ndef fetch(u): return urllib2.urlopen(u)\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["python"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.findings).toBeGreaterThanOrEqual(1);
    expect(result.value.details.some((d) => d.oldName === "urllib2")).toBe(true);
  });

  it("flags distutils", async () => {
    const dir = await setup({
      "setup.py": "from distutils.core import setup\nsetup(name='x')\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["python"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.oldName === "distutils")).toBe(true);
    expect(result.value.details.some((d) => d.newName === "setuptools")).toBe(true);
  });

  it("flags from X import Y using the top-level package", async () => {
    const dir = await setup({
      "script.py": "from PyPDF2 import PdfReader\ndef go(): return PdfReader('x')\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["python"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.oldName === "PyPDF2")).toBe(true);
  });

  it("flags multiple deprecated imports in one file", async () => {
    const dir = await setup({
      "many.py": [
        "import imp",
        "import distutils",
        "import urllib2",
        "import hashlib  # fine",
        "",
      ].join("\n"),
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["python"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    const oldNames = result.value.details.map((d) => d.oldName).sort();
    expect(oldNames).toEqual(["distutils", "imp", "urllib2"]);
  });

  it("does not flag a clean import", async () => {
    const dir = await setup({
      "good.py": "import pypdf\nfrom pypdf import PdfReader\nimport hashlib\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["python"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("pass");
    expect(result.value.findings).toBe(0);
  });
});
