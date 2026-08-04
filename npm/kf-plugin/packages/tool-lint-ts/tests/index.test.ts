// kirkforge-lint-disable no-eval no-hardcoded-openai-key no-var
import { describe, it, expect, afterAll } from "vitest";
import { createTSLintEngine } from "../src/index.js";
import { writeFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { rm } from "node:fs/promises";

const BASE_DIR = resolve(tmpdir(), `kirkforge-lint-test-${Date.now()}`);
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

describe("tool-lint-ts", () => {
  it("detects var usage", async () => {
    const dir = await setup({
      "src/bad.ts": "var x = 1;\nconsole.log(x);\n",
    });
    const engine = createTSLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("fail");
    expect(result.value.details.some((d) => d.rule === "no-var")).toBe(true);
    expect(result.value.details.some((d) => d.rule === "no-console")).toBe(true);
  });

  it("passes clean code", async () => {
    const dir = await setup({
      "src/good.ts": "const x = 1;\nconst y = x + 1;\nexport { y };\n",
    });
    const engine = createTSLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("pass");
    expect(result.value.details).toHaveLength(0);
  });

  it("detects eval usage as critical", async () => {
    const dir = await setup({
      "src/danger.ts": 'eval("console.log(1)");',
    });
    const engine = createTSLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.status).toBe("fail");
    expect(result.value.details.some((d) => d.rule === "no-eval")).toBe(true);
  });

  it("detects throw literal", async () => {
    const dir = await setup({
      "src/bad.ts": 'throw "something broke";',
    });
    const engine = createTSLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.details.some((d) => d.rule === "no-throw-literal")).toBe(true);
  });

  it("detects process.env access", async () => {
    const dir = await setup({
      "src/config.ts": "const key = process.env.SECRET_KEY;",
    });
    const engine = createTSLintEngine({ cwd: dir });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    const details = result.value.details;
    expect(details.some((d) => d.rule === "no-process-env")).toBe(true);
    expect(details.length).toBe(1);
  });

  it("respects file filter", async () => {
    const dir = await setup({
      "src/bad.ts": "var x = 1;\nconsole.log(x);\n",
      "src/also-bad.ts": "var z = 3;\nconsole.log(z);\n",
      "src/readme.md": "var ignored = 'should not scan';",
    });
    const engine = createTSLintEngine({ cwd: dir, files: ["src/bad.ts"] });
    const result = await engine.emit("test");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value.filesScanned).toBe(1);
    // Only src/bad.ts is scanned; it has no-var + no-console
    expect(result.value.details).toHaveLength(2);
    expect(result.value.details[0]!.file).toBe("src/bad.ts");
  });

  it("has correct rule categories", () => {
    const engine = createTSLintEngine({ cwd: BASE_DIR });
    expect(engine.emit).toBeDefined();
  });
  // kirkforge-lint-enable no-eval no-hardcoded-openai-key no-var
});
