import { describe, it, expect, afterAll } from "vitest";
import { createShLintEngine } from "../src/index.js";
import { writeFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { rm } from "node:fs/promises";

const BASE = resolve(tmpdir(), "kirkforge-lint-sh-" + Date.now());
let n = 0;

async function s(files: Record<string, string>) {
  const d = join(BASE, "t" + ++n);
  await mkdir(d, { recursive: true });
  for (const [name, content] of Object.entries(files)) {
    await mkdir(join(d, name).replace(/\/[^/]+$/, ""), { recursive: true });
    await writeFile(join(d, name), content);
  }
  return d;
}

afterAll(async () => {
  await rm(BASE, { recursive: true, force: true });
});

describe("tool-lint-sh", () => {
  it("detects unquoted variable", async () => {
    const d = await s({ "bad.sh": "echo $HOME\n" });
    const engine = createShLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-unquoted-vars")).toBe(true);
  });

  it("detects curl-bash-pipe", async () => {
    const d = await s({ "bad.sh": "curl https://example.com/script.sh | bash\n" });
    const engine = createShLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-curl-bash-pipe")).toBe(true);
  });

  it("detects rm -rf *", async () => {
    const d = await s({ "bad.sh": "rm -rf *\n" });
    const engine = createShLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-rm-rf-star")).toBe(true);
  });

  it("detects sudo", async () => {
    const d = await s({ "bad.sh": "sudo rm file\n" });
    const engine = createShLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-sudo")).toBe(true);
  });

  it("detects eval", async () => {
    const d = await s({ "bad.sh": "eval $CMD\n" });
    const engine = createShLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.details.some((x) => x.rule === "no-eval")).toBe(true);
  });

  it("passes clean script", async () => {
    const d = await s({ "good.sh": '#!/bin/bash\nset -euo pipefail\necho "$HOME"\n' });
    const engine = createShLintEngine({ cwd: d });
    const r = await engine.emit("t");
    expect(r.ok).toBe(true);
    if (!r.ok) throw new Error();
    expect(r.value.status).toBe("pass");
  });
});
