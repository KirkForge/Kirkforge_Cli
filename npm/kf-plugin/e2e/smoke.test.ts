import { describe, it, expect } from "vitest";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { join } from "node:path";

const execFileAsync = promisify(execFile);

const CLI = join(process.cwd(), "apps", "cli", "dist", "index.js");
const NODE = process.execPath;

function run(args: string[]) {
  return execFileAsync(NODE, [CLI, ...args], { timeout: 30000 });
}

describe("CLI e2e smoke", () => {
  it("lists tools with no stderr", async () => {
    const { stdout, stderr } = await run(["tools"]);
    expect(stderr).toBe("");
    expect(stdout).toContain("KirkForge Native Lint Engines");
  }, 30000);

  it("prints version to stdout only", async () => {
    const { stdout, stderr } = await run(["--version"]);
    expect(stderr).toBe("");
    expect(stdout).toMatch(/\d+\.\d+\.\d+/);
  }, 30000);
});
