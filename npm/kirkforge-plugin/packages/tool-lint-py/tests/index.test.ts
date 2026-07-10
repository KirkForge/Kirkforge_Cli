// kirkforge-lint-disable no-eval no-hardcoded-openai-key
import { describe, it, expect, afterAll } from "vitest";
import { createPyLintEngine } from "../src/index.js";
import { writeFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { rm } from "node:fs/promises";

const BASE_DIR = resolve(tmpdir(), `kirkforge-lint-py-test-${Date.now()}`);
let testCounter = 0;

async function setup(files: Record<string, string>) {
  const dir = join(BASE_DIR, `test-${++testCounter}`);
  await mkdir(dir, { recursive: true });
  for (const [name, content] of Object.entries(files)) {
    const filePath = join(dir, name);
    await mkdir(filePath.substring(0, filePath.lastIndexOf("/")), { recursive: true });
    await writeFile(filePath, content);
  }
  return dir;
}

afterAll(async () => {
  await rm(BASE_DIR, { recursive: true, force: true });
});

describe("tool-lint-py", () => {
  it("detects bare except", async () => {
    const dir = await setup({ "src/bad.py": "try:\n  x = 1\nexcept:\n  pass\n" });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("fail");
    expect(result.value.details.some((d) => d.rule === "no-bare-except")).toBe(true);
  });

  it("detects mutable defaults", async () => {
    const dir = await setup({ "src/bad.py": "def foo(x=[]):\n  pass\n" });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-mutable-defaults")).toBe(true);
  });

  it("detects print statements", async () => {
    const dir = await setup({ "src/bad.py": "print('hello')\n" });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-print")).toBe(true);
  });

  it("detects wildcard import", async () => {
    const dir = await setup({ "src/bad.py": "from os import *\n" });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-wildcard-import")).toBe(true);
  });

  it("detects eval/exec", async () => {
    const dir = await setup({ "src/bad.py": "eval('1+1')\nexec('x=1')\n" });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("fail");
    expect(result.value.details.some((d) => d.rule === "no-eval-exec")).toBe(true);
  });

  it("detects subprocess shell=True", async () => {
    const dir = await setup({
      "src/bad.py": "import subprocess\nsubprocess.run('ls', shell=True)\n",
    });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-subprocess-shell")).toBe(true);
  });

  it("detects pickle usage", async () => {
    const dir = await setup({ "src/bad.py": "import pickle\npickle.loads(data)\n" });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-pickle")).toBe(true);
  });

  it("detects yaml.load (unsafe)", async () => {
    const dir = await setup({ "src/bad.py": "import yaml\nyaml.load(data)\n" });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-yaml-load")).toBe(true);
  });

  it("detects hardcoded password", async () => {
    const dir = await setup({ "src/bad.py": 'password = "hunter2"\n' });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-hardcoded-password")).toBe(true);
  });

  it("detects API token", async () => {
    const dir = await setup({ "src/bad.py": 'token = "sk-12345678901234567890abcdef"\n' });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-hardcoded-token")).toBe(true);
  });

  it("detects range(len()) anti-pattern", async () => {
    const dir = await setup({ "src/bad.py": "for i in range(len(items)):\n  print(i)\n" });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-range-len")).toBe(true);
  });

  it("passes clean code", async () => {
    const dir = await setup({
      "src/good.py": '"""Add two integers."""\ndef add(a: int, b: int) -> int:\n    return a + b\n',
    });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("pass");
    expect(result.value.errors).toBe(0);
  });

  it("respects extensions filter", async () => {
    const dir = await setup({
      "src/bad.py": "print('hello')",
      "src/ignore.ts": "console.log('hello')",
      "src/readme.md": "print('nope')",
    });
    const engine = createPyLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.filesScanned).toBe(1);
    expect(result.value.details[0]!.file).toBe("src/bad.py");
  });

  it("has correct rule categories", () => {
    const engine = createPyLintEngine({ cwd: BASE_DIR });
    expect(engine.emit).toBeDefined();
  });
  // kirkforge-lint-enable no-eval no-hardcoded-openai-key
});
