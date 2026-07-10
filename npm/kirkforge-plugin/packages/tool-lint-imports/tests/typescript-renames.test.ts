// kirkforge-lint-disable no-console
import { describe, it, expect, afterAll } from "vitest";
import { createImportLintEngine } from "../src/index.js";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";

const BASE_DIR = resolve(tmpdir(), `kirkforge-imports-ts-${Date.now()}`);
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

describe("tool-lint-imports: typescript", () => {
  it("flags default-import of `request`", async () => {
    const dir = await setup({
      "fetch.ts": "import request from 'request';\nrequest('https://example.com', cb);\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["typescript"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    // Advisory: status stays "pass"; findings count is what surfaces the issue
    expect(result.value.status).toBe("pass");
    expect(result.value.findings).toBeGreaterThanOrEqual(1);
    expect(result.value.details.some((d) => d.oldName === "request")).toBe(true);
  });

  it("flags named imports", async () => {
    const dir = await setup({
      "moment.ts": "import { format } from 'moment';\nconsole.log(format(new Date()));\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["typescript"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.oldName === "moment")).toBe(true);
  });

  it("flags require()", async () => {
    const dir = await setup({
      "cjs.js": "const mkdirp = require('mkdirp');\nmkdirp.sync('out');\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["typescript"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.oldName === "mkdirp")).toBe(true);
  });

  it("flags dynamic import()", async () => {
    const dir = await setup({
      "dyn.ts": "async function go() { const m = await import('glob'); return m.sync('**'); }\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["typescript"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.oldName === "glob")).toBe(true);
  });

  it("flags side-effect imports", async () => {
    const dir = await setup({
      "polyfill.ts": "import 'babel-polyfill';\nconsole.log('hi');\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["typescript"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.oldName === "babel-polyfill")).toBe(true);
  });

  it("ignores Node builtins (node:fs, node:path)", async () => {
    const dir = await setup({
      "node.ts": [
        "import { readFile } from 'node:fs';",
        "import path from 'node:path';",
        "const x: string = 'hi';\n",
      ].join("\n"),
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["typescript"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("pass");
    expect(result.value.findings).toBe(0);
  });

  it("ignores relative imports", async () => {
    const dir = await setup({
      "local.ts": "import { foo } from './foo';\nimport bar from '../bar';\nfoo(); bar();\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["typescript"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("pass");
  });

  it("preserves scoped package names (@scope/name)", async () => {
    const dir = await setup({
      "scoped.ts": "import { x } from '@some-scope/lib';\nx();\n",
    });
    const engine = createImportLintEngine({ cwd: dir, languages: ["typescript"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    // No entry in the renames table for @some-scope/lib — should pass cleanly
    expect(result.value.status).toBe("pass");
  });
});
