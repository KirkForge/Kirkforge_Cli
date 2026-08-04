import { describe, it, expect, afterAll } from "vitest";
import { createSqlLintEngine } from "../src/index.js";
import { writeFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { rm } from "node:fs/promises";

const BASE = resolve(tmpdir(), "kirkforge-lint-sql-" + Date.now());
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

describe("tool-lint-sql", () => {
  it("detects SELECT * as inefficient", async () => {
    const d = await s({ "bad.sql": "SELECT * FROM users;\n" });
    const engine = createSqlLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-select-star")).toBe(true);
  });

  it("detects DROP TABLE as critical", async () => {
    const d = await s({ "bad.sql": "DROP TABLE users;\n" });
    const engine = createSqlLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.status).toBe("fail");
    expect(r.value.details.some((x) => x.rule === "no-drop-table")).toBe(true);
  });

  it("detects DELETE without WHERE", async () => {
    const d = await s({ "bad.sql": "DELETE FROM logs;\n" });
    const engine = createSqlLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-unsafe-delete")).toBe(true);
  });

  it("detects TRUNCATE as critical", async () => {
    const d = await s({ "bad.sql": "TRUNCATE TABLE cache;\n" });
    const engine = createSqlLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-truncate")).toBe(true);
  });

  it("detects implicit join syntax", async () => {
    // Pattern: \bFROM\s+\w+\s*,\s*\w+\s+WHERE\b — needs FROM word, comma, word, WHERE
    const d = await s({ "bad.sql": "SELECT u.name FROM users, orders WHERE u.id = o.user_id;\n" });
    const engine = createSqlLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-implicit-join")).toBe(true);
  });

  it("detects SQL injection via string concatenation", async () => {
    // Pattern: (['"])\s*(?:\|\||\+)\s*\w+(?:\|\||\+)\s*['"] — e.g. ' + name + '
    const d = await s({ "bad.sql": "SELECT * FROM users WHERE name = ' + name + ';\n" });
    const engine = createSqlLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-dynamic-injection")).toBe(true);
  });

  it("passes clean SQL", async () => {
    const d = await s({
      "good.sql": "SELECT id, name, email FROM users WHERE active = 1 ORDER BY name;\n",
    });
    const engine = createSqlLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.status).toBe("pass");
  });

  it("respects file filter", async () => {
    const d = await s({
      "bad.sql": "DROP TABLE users;\n",
      "migrations/bad.sql": "DROP TABLE logs;\n",
    });
    const engine = createSqlLintEngine({ cwd: d, files: ["bad.sql"] });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.filesScanned).toBe(1);
    expect(r.value.details[0]!.file).toBe("bad.sql");
  });
});
