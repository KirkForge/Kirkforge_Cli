import { describe, it, expect } from "vitest";
import { GitnexusEmitter } from "../src/index.js";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

async function isGitAvailable(): Promise<boolean> {
  try {
    await execFileAsync("git", ["--version"], { timeout: 5000 });
    return true;
  } catch {
    return false;
  }
}

describe("GitnexusEmitter", () => {
  it("constructs with cwd option", () => {
    const emitter = new GitnexusEmitter({ cwd: process.cwd() });
    expect(emitter).toBeInstanceOf(GitnexusEmitter);
  });

  it("returns skipped report when not a git repo", async () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "gitnexus-test-"));
    try {
      const emitter = new GitnexusEmitter({ cwd: tmpDir });
      const result = await emitter.emit("test-task-1");
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.filesChanged).toBe(0);
        expect(result.value.insertions).toBe(0);
        expect(result.value.deletions).toBe(0);
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("reports written files when not a git repo", async () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "gitnexus-test-"));
    try {
      writeFileSync(join(tmpDir, "hello.ts"), "console.log('hello');\n");
      const emitter = new GitnexusEmitter({ cwd: tmpDir, writtenFiles: ["hello.ts"] });
      const result = await emitter.emit("test-task-2");
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.filesChanged).toBe(1);
        expect(result.value.paths).toContain("hello.ts");
        expect(result.value.insertions).toBe(1);
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("reports changes in a git repo", async () => {
    if (!(await isGitAvailable())) return;
    const tmpDir = mkdtempSync(join(tmpdir(), "gitnexus-git-test-"));
    try {
      await execFileAsync("git", ["init"], { cwd: tmpDir });
      await execFileAsync("git", ["config", "user.email", "test@test.com"], { cwd: tmpDir });
      await execFileAsync("git", ["config", "user.name", "Test"], { cwd: tmpDir });
      writeFileSync(join(tmpDir, "initial.txt"), "hello\n");
      await execFileAsync("git", ["add", "."], { cwd: tmpDir });
      await execFileAsync("git", ["commit", "-m", "initial"], { cwd: tmpDir });
      writeFileSync(join(tmpDir, "new-file.txt"), "world\n");
      const emitter = new GitnexusEmitter({ cwd: tmpDir });
      const result = await emitter.emit("test-task-git");
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.filesChanged).toBeGreaterThanOrEqual(1);
        expect(result.value.paths).toContain("new-file.txt");
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("includes durationMs in report", async () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "gitnexus-test-"));
    try {
      const emitter = new GitnexusEmitter({ cwd: tmpDir });
      const result = await emitter.emit("test-task-duration");
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.durationMs).toBeGreaterThanOrEqual(0);
      }
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });
});
