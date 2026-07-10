import { describe, it, expect, afterAll } from "vitest";
import { createRsLintEngine } from "../src/index.js";
import { writeFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { rm } from "node:fs/promises";

const BASE = resolve(tmpdir(), "kirkforge-lint-rs-" + Date.now());
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

describe("tool-lint-rs", () => {
  it("detects .unwrap() as high severity", async () => {
    const d = await s({ "bad.rs": "fn main() {\n  let x = Some(1);\n  let v = x.unwrap();\n}\n" });
    const engine = createRsLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.status).toBe("fail");
    expect(r.value.details.some((x) => x.rule === "no-unwrap")).toBe(true);
  });

  it("detects unsafe block", async () => {
    const d = await s({
      "bad.rs": "fn main() {\n  unsafe {\n    let p: *const i32 = &42;\n  }\n}\n",
    });
    const engine = createRsLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-unsafe")).toBe(true);
  });

  it("detects println! in lib code", async () => {
    const d = await s({ "lib.rs": 'println!("hello");\n' });
    const engine = createRsLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-println-in-lib")).toBe(true);
  });

  it("detects todo!() and dbg!()", async () => {
    const d = await s({ "bad.rs": 'todo!("finish this");\ndbg!(some_value);\n' });
    const engine = createRsLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-todo")).toBe(true);
    expect(r.value.details.some((x) => x.rule === "no-dbg")).toBe(true);
  });

  it("detects .expect() usage", async () => {
    const d = await s({ "bad.rs": 'let v = x.expect("should exist");\n' });
    const engine = createRsLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-expect-in-prod")).toBe(true);
  });

  it("passes clean Rust code", async () => {
    const d = await s({
      "good.rs": "fn main() -> Result<(), Box<dyn std::error::Error>> {\n  Ok(())\n}\n",
    });
    const engine = createRsLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.status).toBe("pass");
  });

  it("respects file filter", async () => {
    const d = await s({
      "src/bad.rs": "let v = x.unwrap();\n",
      "src/lib.rs": 'println!("no lint here");\n',
    });
    const engine = createRsLintEngine({ cwd: d, files: ["src/bad.rs"] });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.filesScanned).toBe(1);
    expect(r.value.details[0]!.file).toBe("src/bad.rs");
  });
});
