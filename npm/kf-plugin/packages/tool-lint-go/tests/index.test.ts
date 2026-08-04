import { describe, it, expect, afterAll } from "vitest";
import { createGoLintEngine } from "../src/index.js";
import { writeFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { rm } from "node:fs/promises";

const BASE = resolve(tmpdir(), "kirkforge-lint-go-" + Date.now());
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

describe("tool-lint-go", () => {
  it("detects panic() as high severity", async () => {
    const d = await s({ "bad.go": 'package main\n\nfunc main() {\n  panic("oh no")\n}\n' });
    const engine = createGoLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.status).toBe("fail");
    expect(r.value.details.some((x) => x.rule === "no-panic")).toBe(true);
  });

  it("detects unhandled error with blank identifier", async () => {
    const d = await s({
      "bad.go": "package main\n\nfunc main() {\n  result, _ := doThing()\n  _ = result\n}\n",
    });
    const engine = createGoLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-unhandled-error")).toBe(true);
  });

  it("detects strings.Title as deprecated", async () => {
    const d = await s({
      "bad.go":
        'package main\nimport "strings"\n\nfunc main() {\n  s := strings.Title("hello")\n}\n',
    });
    const engine = createGoLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-string-title")).toBe(true);
  });

  it("detects global var at package level", async () => {
    const d = await s({ "bad.go": "package main\n\nvar globalCounter = 0\n\nfunc main() {}\n" });
    const engine = createGoLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-global-var")).toBe(true);
  });

  it("detects defer in a for loop line", async () => {
    // The engine tests per-line; inline defer+for on same line for regex match
    const d = await s({
      "bad.go": "package main\n\nfunc main() {\n  for _, f := range files { defer f.Close() }\n}\n",
    });
    const engine = createGoLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-defer-in-loop")).toBe(true);
  });

  it("passes clean Go code", async () => {
    const d = await s({
      "good.go": 'package main\n\nimport "fmt"\n\nfunc main() {\n  fmt.Println("hello")\n}\n',
    });
    const engine = createGoLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.status).toBe("pass");
  });

  it("respects file filter", async () => {
    const d = await s({
      "bad.go": 'package main\nfunc main() { panic("x") }\n',
      "good.go": "package main\nfunc main() {}\n",
    });
    const engine = createGoLintEngine({ cwd: d, files: ["bad.go"] });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.filesScanned).toBe(1);
    expect(r.value.details[0]!.file).toBe("bad.go");
  });
});
