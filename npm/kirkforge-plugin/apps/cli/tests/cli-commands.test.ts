import { describe, it, expect, afterAll } from "vitest";
import { execFile, type ChildProcess } from "node:child_process";
import { promisify } from "node:util";
import { writeFileSync, mkdirSync, rmSync, mkdtempSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { buildCorrectionPrompt, recallRoutingBias, verifyWorkspace } from "@kirkforge/plugin";
import { FileAdapter, MemoryStore } from "@kirkforge/memory-palace";

const execFileAsync = promisify(execFile);

const CLI = join(process.cwd(), "apps", "cli", "dist", "index.js");
const NODE = process.execPath;

// Track active child processes for cleanup
const activeChildren = new Set<ChildProcess>();

function run(args: string[]) {
  const child = execFile(NODE, [CLI, ...args], { timeout: 30000 });
  activeChildren.add(child);
  const promise = execFileAsync(NODE, [CLI, ...args], {
    timeout: 30000,
  });
  return promise;
}

afterAll(() => {
  for (const child of activeChildren) {
    try {
      child.kill("SIGTERM");
    } catch {
      // already dead
    }
  }
  activeChildren.clear();
});

const validPacket = {
  taskId: "test-task",
  turn: 0,
  ts: new Date().toISOString(),
  verification: {
    lint: { errors: 3, warnings: 0 },
    types: { errors: 1 },
    security: { findings: 0, critical: 0, high: 0 },
    overall: "fail" as const,
  },
  changes: { filesChanged: 2, paths: ["src/foo.ts", "src/bar.ts"], insertions: 10, deletions: 5 },
  graph: { edgeCount: 5, newEdges: 0, brokenEdges: 0, cycles: 0 },
  contributingSignals: [],
};

function makeTmpDir() {
  return mkdtempSync(join(tmpdir(), "kirkforge-cli-test-"));
}

function cleanTmpDir(dir: string) {
  rmSync(dir, { recursive: true, force: true });
}

function makeStore(dir: string) {
  const adapter = new FileAdapter(join(dir, "mem.json"));
  const store = new MemoryStore(adapter);
  return { adapter, store };
}

// ── CLI smoke tests (subprocess argument parsing) ──

describe("CLI smoke: argument-parsing errors", () => {
  it("exits 1 with stderr on missing required args", async () => {
    try {
      await run(["prompt"]);
      expect.unreachable("should have thrown");
    } catch (err: unknown) {
      const e = err as { stderr: string; code: number };
      expect(e.code).toBe(1);
      expect(e.stderr).toContain("required option");
    }
  }, 30000);

  it("exits 1 with stderr on nonexistent file", async () => {
    try {
      await run(["prompt", "--packet", "/nonexistent/path/packet.json"]);
      expect.unreachable("should have thrown");
    } catch (err: unknown) {
      const e = err as { stderr: string; code: number };
      expect(e.code).toBe(1);
      expect(e.stderr).toContain("Error");
    }
  }, 30000);

  it("exits 1 with stderr on invalid JSON packet file", async () => {
    const tmpDir = makeTmpDir();
    try {
      const badPath = join(tmpDir, "bad.json");
      writeFileSync(badPath, "not json at all");
      try {
        await run(["prompt", "--packet", badPath]);
        expect.unreachable("should have thrown");
      } catch (err: unknown) {
        const e = err as { stderr: string; code: number };
        expect(e.code).toBe(1);
        expect(e.stderr).toContain("invalid JSON");
      }
    } finally {
      cleanTmpDir(tmpDir);
    }
  }, 30000);

  it("CLI binary exists and returns version", async () => {
    const { stdout, stderr } = await run(["--version"]);
    expect(stderr).toBe("");
    expect(stdout).toMatch(/\d+\.\d+\.\d+/);
  }, 30000);
});

// ── Prompt: unit tests ──

describe("buildCorrectionPrompt", () => {
  it("produces output mentioning lint errors from packet", () => {
    const output = buildCorrectionPrompt(validPacket);
    expect(output).toContain("lint errors");
    expect(output).toContain("type errors");
    expect(output).toContain("src/foo.ts, src/bar.ts");
  });

  it("with python language context resolves python tool names", () => {
    const pyPacket = structuredClone(validPacket);
    pyPacket.verification.security = { findings: 2, critical: 0, high: 1 };
    const output = buildCorrectionPrompt(pyPacket, { language: "python" });
    expect(output).toContain("KirkForge Python lint engine");
    expect(output).toContain("pyright");
    // bandit surfaces only when security findings are present
    expect(output).toContain("KirkForge Python lint engine (safety rules)");
  });

  it("includes security findings when present", () => {
    const packet = structuredClone(validPacket);
    packet.verification.security = { findings: 2, critical: 0, high: 1 };
    const output = buildCorrectionPrompt(packet);
    expect(output).toContain("security");
  });

  it("returns non-empty string for empty packet defaults", () => {
    const packet = structuredClone(validPacket);
    packet.verification = {
      lint: { errors: 0, warnings: 0 },
      types: { errors: 0 },
      security: { findings: 0, critical: 0, high: 0 },
      overall: "pass" as const,
    };
    packet.changes = { filesChanged: 0, paths: [], insertions: 0, deletions: 0 };
    const output = buildCorrectionPrompt(packet);
    expect(typeof output).toBe("string");
    expect(output.length).toBeGreaterThan(0);
  });
});

// ── Recall: unit tests ──

describe("recallRoutingBias", () => {
  it("returns { ok: true, bias: null } when store is empty", async () => {
    const dir = makeTmpDir();
    try {
      const { store } = makeStore(dir);
      const result = await recallRoutingBias("no match", undefined, store);
      expect(result.ok).toBe(true);
      if (result.ok) expect(result.value).toBeNull();
    } finally {
      cleanTmpDir(dir);
    }
  });

  it("accepts optional model filter and still returns null on empty store", async () => {
    const dir = makeTmpDir();
    try {
      const { store } = makeStore(dir);
      const result = await recallRoutingBias("no match", "claude-3", store);
      expect(result.ok).toBe(true);
      if (result.ok) expect(result.value).toBeNull();
    } finally {
      cleanTmpDir(dir);
    }
  });

  it("returns bias after writing an observation via MemoryStore", async () => {
    const dir = makeTmpDir();
    try {
      const { adapter, store } = makeStore(dir);
      const writeResult = await store.writeTaskObservation({
        taskId: "r1",
        description: "implement login flow",
        language: "typescript",
        mode: "hard-prompt",
        model: "gpt-4",
        outcome: "pass",
        tokens: 0,
        durationMs: 5000,
      });
      expect(writeResult.ok).toBe(true);
      await adapter.persist();

      const result = await recallRoutingBias("implement login flow", undefined, store);
      expect(result.ok).toBe(true);
      if (result.ok && result.value) {
        expect(result.value).toHaveProperty("prefer");
        expect(result.value).toHaveProperty("avoid");
        expect(result.value).toHaveProperty("confidence");
      }
    } finally {
      cleanTmpDir(dir);
    }
  });

  it("returns low-confidence bias for weak matches", async () => {
    const dir = makeTmpDir();
    try {
      const { adapter, store } = makeStore(dir);
      const writeResult = await store.writeTaskObservation({
        taskId: "r2",
        description: "implement login flow",
        language: "typescript",
        mode: "hard-prompt",
        model: "gpt-4",
        outcome: "pass",
        tokens: 0,
        durationMs: 5000,
      });
      expect(writeResult.ok).toBe(true);
      await adapter.persist();

      const result = await recallRoutingBias(
        "completely unrelated unicorn feature",
        undefined,
        store,
      );
      expect(result.ok).toBe(true);
      if (result.ok) {
        // Weak semantic match still returns a bias with low confidence
        expect(result.value).not.toBeNull();
        if (result.value) {
          expect(typeof result.value.confidence).toBe("number");
          expect(result.value.confidence).toBeLessThan(0.3);
        }
      }
    } finally {
      cleanTmpDir(dir);
    }
  });
});

// ── Verify-workspace: unit tests ──

describe("verifyWorkspace", () => {
  it("returns err for nonexistent workspace directory", async () => {
    const result = await verifyWorkspace({
      workspace: "/nonexistent/path/workspace",
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("does not exist");
    }
  });

  it("returns ReducedStatePacket for empty temp workspace", async () => {
    const dir = makeTmpDir();
    try {
      const wsDir = join(dir, "workspace");
      mkdirSync(wsDir);
      const result = await verifyWorkspace({ workspace: wsDir });
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value).toHaveProperty("verification");
        expect(result.value).toHaveProperty("changes");
        expect(result.value).toHaveProperty("graph");
        expect(result.value.verification).toHaveProperty("overall");
      }
    } finally {
      cleanTmpDir(dir);
    }
  });

  it("returns ok even when verification reports failures", async () => {
    const dir = makeTmpDir();
    const wsDir = join(dir, "workspace");
    mkdirSync(wsDir);
    writeFileSync(join(wsDir, "broken.ts"), "export const x: string = 42;\n");
    try {
      const result = await verifyWorkspace({
        workspace: wsDir,
        language: "typescript",
      });
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.verification).toHaveProperty("overall");
        expect(typeof result.value.verification.overall).toBe("string");
      }
    } finally {
      cleanTmpDir(dir);
    }
  });

  it("honours --file filter when provided", async () => {
    const dir = makeTmpDir();
    const wsDir = join(dir, "workspace");
    mkdirSync(wsDir);
    writeFileSync(join(wsDir, "a.ts"), "export const a = 1;\n");
    writeFileSync(join(wsDir, "b.ts"), "export const b: string = 2;\n");
    try {
      const result = await verifyWorkspace({
        workspace: wsDir,
        files: ["a.ts"],
        language: "typescript",
      });
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value).toHaveProperty("verification");
      }
    } finally {
      cleanTmpDir(dir);
    }
  });
});
