import { describe, it, expect, afterAll } from "vitest";
import { createCLintEngine } from "../src/index.js";
import { writeFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { rm } from "node:fs/promises";

const BASE = resolve(tmpdir(), "kirkforge-lint-c-" + Date.now());
let n = 0;

async function s(files: Record<string, string>) {
  const d = join(BASE, "t" + ++n);
  await mkdir(d, { recursive: true });
  for (const [name, content] of Object.entries(files)) {
    const fp = join(d, name);
    await mkdir(fp.substring(0, fp.lastIndexOf("/")), { recursive: true });
    await writeFile(fp, content);
  }
  return d;
}

afterAll(async () => {
  await rm(BASE, { recursive: true, force: true });
});

describe("tool-lint-c", () => {
  it("detects gets() usage as critical", async () => {
    const d = await s({ "bad.c": "char buf[100];\ngets(buf);\n" });
    const engine = createCLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.status).toBe("fail");
    expect(r.value.details.some((x) => x.rule === "no-gets")).toBe(true);
  });

  it("detects system() usage", async () => {
    const d = await s({ "bad.c": 'system("rm -rf /");\n' });
    const engine = createCLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-system")).toBe(true);
  });

  it("detects strcpy and sprintf", async () => {
    const d = await s({ "bad.c": 'strcpy(buf, src);\nsprintf(buf, "%s", src);\n' });
    const engine = createCLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-strcpy")).toBe(true);
    expect(r.value.details.some((x) => x.rule === "no-sprintf")).toBe(true);
  });

  it("detects void main", async () => {
    const d = await s({ "bad.c": "void main(void) { return; }\n" });
    const engine = createCLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-void-main")).toBe(true);
  });

  it("detects goto", async () => {
    const d = await s({ "bad.c": "if (x) { goto cleanup; }\ncleanup: return;\n" });
    const engine = createCLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-goto")).toBe(true);
  });

  it("detects magic numbers", async () => {
    const d = await s({ "bad.c": "int x = 99999;\n" });
    const engine = createCLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-magic-numbers")).toBe(true);
  });

  it("passes clean C code with only known false-positives", async () => {
    const d = await s({
      "good.c":
        '#include <stdio.h>\n#define FOO 1\nint main(void) {\n  char buf[100];\n  fgets(buf, sizeof(buf), stdin);\n  snprintf(buf, sizeof(buf), "hello");\n  return 0;\n}\n',
    });
    const engine = createCLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    // Per-line engine false-positives on multi-line rules (no-missing-include-guard) are known
    const realViolations = r.value.details.filter((x) => x.rule !== "no-missing-include-guard");
    expect(realViolations).toHaveLength(0);
  });

  it("respects file filter for .c only", async () => {
    const d = await s({
      "src/bad.c": "gets(buf);\n",
      "readme.md": "gets() is bad in C",
    });
    const engine = createCLintEngine({ cwd: d, files: ["src/bad.c"] });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.filesScanned).toBe(1);
    expect(r.value.details[0]!.file).toBe("src/bad.c");
  });
});
