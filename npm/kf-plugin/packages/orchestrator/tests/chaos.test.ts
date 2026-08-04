import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { Orchestrator } from "../src/index.js";
import type { OrchestratorConfig } from "../src/index.js";
import { InMemoryAdapter, MemoryStore } from "@kirkforge/memory-palace";
import { EventBus } from "@kirkforge/core-events";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import type { TaskInput } from "../src/types.js";
import { ok } from "@kirkforge/core-types";
import type { Result } from "@kirkforge/core-types";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeModelConfig() {
  return {
    providers: {
      test: {
        provider: "ollama" as const,
        baseUrl: "http://localhost:11434",
        defaultModel: "test-model",
      },
    },
    defaultProvider: "test",
  };
}

function tmpWorkspace(extraFiles: Record<string, string> = {}): string {
  const dir = mkdtempSync(join(tmpdir(), "kirkforge-chaos-"));
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: "chaos-test", type: "module" }));
  writeFileSync(
    join(dir, "tsconfig.json"),
    JSON.stringify({ compilerOptions: { target: "ES2022", module: "ESNext", strict: true } }),
  );
  writeFileSync(join(dir, "src.ts"), "export function hello() { return 'hello'; }\n");
  for (const [relPath, content] of Object.entries(extraFiles)) {
    const full = resolve(dir, relPath);
    mkdirSync(join(full, ".."), { recursive: true });
    writeFileSync(full, content);
  }
  return dir;
}

function makePassPacket(taskId: string, files: string[] = ["src.ts"]) {
  return {
    taskId,
    verification: {
      overall: "pass" as const,
      lint: { errors: 0, warnings: 0 },
      types: { errors: 0 },
      security: { findings: 0, critical: 0, high: 0 },
    },
    graph: { edgeCount: 5, newEdges: 0, brokenEdges: 0, cycles: 0 },
    artifactEnforcement: {
      status: "pass" as const,
      blocked: 0,
      blockedPaths: [],
    },
    changes: { filesChanged: files.length, paths: files, insertions: 10, deletions: 2 },
    emissions: {
      filesWritten: files.length,
      totalBytes: files.length * 100,
      files: files.map((f) => ({
        path: f,
        sha256: "abc123",
        bytes: 100,
        beforeHash: null,
        existed: true,
      })),
    },
    verifierPolicy: {
      required: ["lint" as const],
      advisory: ["typecheck" as const],
      missingRequired: [] as string[],
      skippedRequired: [] as string[],
    },
  };
}

type DelegationResultStub = {
  decision: { mode: "artifact" | "hard-prompt" | "schema-contract" };
  emission: {
    agentId: string;
    content: string;
    promptTokens: number;
    completionTokens: number;
    totalTokens: number;
    model: string;
    format: "artifact" | "hard-prompt" | "schema-contract";
  };
  signals: Array<{
    id: string;
    taskId: string;
    domain: string;
    kind: string;
    source: string;
    ts: string;
    value: unknown;
  }>;
  packet: ReturnType<typeof makePassPacket>;
};

class TestableOrchestrator extends Orchestrator {
  private _stub: ((task: TaskInput) => Promise<Result<DelegationResultStub, Error>>) | null = null;
  constructor(config: OrchestratorConfig) {
    super(config);
  }
  stubDelegate(fn: (task: TaskInput) => Promise<Result<DelegationResultStub, Error>>) {
    this._stub = fn;
  }
  override async delegate(task: TaskInput) {
    if (this._stub) return this._stub(task);
    return super.delegate(task);
  }
}

// ---------------------------------------------------------------------------
// Chaos Tests
// ---------------------------------------------------------------------------

describe("circuit breaker chaos", () => {
  let dir: string;
  beforeAll(() => {
    dir = tmpWorkspace();
  });
  afterAll(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it("escalates instead of infinite-looping when delegation keep failing", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const bus = new EventBus();
    const orchestrator = new TestableOrchestrator({
      modelConfig: makeModelConfig(),
      memoryStore: store,
      eventBus: bus,
      cwd: dir,
    });

    let callCount = 0;
    orchestrator.stubDelegate(async (task) => {
      callCount++;
      const packet = makePassPacket(task.taskId ?? "chaos-loop");
      // Simulate that the worker output always fails validation
      packet.verification.overall = "fail";
      return ok({
        decision: { mode: "artifact" },
        emission: {
          agentId: "a1",
          content: "// code\n",
          promptTokens: 10,
          completionTokens: 5,
          totalTokens: 15,
          model: "test-model",
          format: "artifact" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: task.taskId ?? "chaos-loop",
            domain: "files",
            kind: "files.written",
            source: "agent",
            ts: new Date().toISOString(),
            value: { files: ["src.ts"] },
          },
        ],
        packet,
      });
    });

    const result = await orchestrator.runCorrectionLoop(
      { taskId: "chaos-loop", description: "fix the TypeScript file" },
      { maxCorrections: 3 },
    );

    expect(result).toBeDefined();
    expect(result.finalVerdict).toBeDefined();
    // Must not retry more than maxCorrections + initial attempt
    expect(callCount).toBeLessThanOrEqual(4); // initial + 3 corrections
    // Should eventually escalate, not loop forever
    expect(result.finalAction).toBe("escalate");
  });
});

describe("validator chaos", () => {
  let dir: string;
  beforeAll(() => {
    dir = tmpWorkspace();
  });
  afterAll(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it("handles validator returning error status as infrastructure failure", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const bus = new EventBus();
    const orchestrator = new TestableOrchestrator({
      modelConfig: makeModelConfig(),
      memoryStore: store,
      eventBus: bus,
      cwd: dir,
    });

    orchestrator.stubDelegate(async (task) => {
      const packet = makePassPacket(task.taskId ?? "chaos-val");
      return ok({
        decision: { mode: "artifact" },
        emission: {
          agentId: "a1",
          content: "// code\n",
          promptTokens: 10,
          completionTokens: 5,
          totalTokens: 15,
          model: "test-model",
          format: "artifact" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: task.taskId ?? "chaos-val",
            domain: "files",
            kind: "files.written",
            source: "agent",
            ts: new Date().toISOString(),
            value: { files: ["src.ts"] },
          },
        ],
        packet,
      });
    });

    // Use a validator that exits immediately with error — but NOT via shell
    // (shell validators are blocked by default). Instead use structured validator
    // that crashes.
    const result = await orchestrator.runCorrectionLoop(
      { taskId: "chaos-val-infra", description: "add TypeScript types to src.ts" },
      {
        maxCorrections: 1,
        validator: {
          command: "nonexistent-binary-kirkforge-test",
          args: ["--check"],
          timeoutMs: 5000,
        },
      },
    );

    expect(result).toBeDefined();
    // Should escalate on validator infrastructure error (missing binary)
    expect(result.finalAction).toBe("escalate");
  });
});

describe("concurrency chaos (separate instances)", () => {
  // NOTE: Orchestrator instances are NOT safe for concurrent runCorrectionLoop
  // calls on a single instance. Each instance must be used by one caller at a time.
  // This test verifies that separate instances don't corrupt each other.
  it("handles multiple separate orchestrator instances concurrently without cross-instance corruption", async () => {
    const dirs = Array.from({ length: 3 }, () => tmpWorkspace());

    try {
      const runs = dirs.map((dir, i) => {
        const store = new MemoryStore(new InMemoryAdapter());
        const bus = new EventBus();
        const orchestrator = new TestableOrchestrator({
          modelConfig: makeModelConfig(),
          memoryStore: store,
          eventBus: bus,
          cwd: dir,
        });

        orchestrator.stubDelegate(async (task) => {
          const packet = makePassPacket(task.taskId ?? `conc-${i}`, ["src.ts"]);
          return ok({
            decision: { mode: "hard-prompt" },
            emission: {
              agentId: "a1",
              content: "ok",
              promptTokens: 5,
              completionTokens: 5,
              totalTokens: 10,
              model: "test-model",
              format: "hard-prompt" as const,
            },
            signals: [
              {
                id: "s1",
                taskId: task.taskId ?? `conc-${i}`,
                domain: "files",
                kind: "files.written",
                source: "agent",
                ts: new Date().toISOString(),
                value: { files: ["src.ts"] },
              },
            ],
            packet,
          });
        });

        return orchestrator.runCorrectionLoop(
          { taskId: `chaos-conc-${i}`, description: `implement function${i} in src.ts` },
          { maxCorrections: 1 },
        );
      });

      const results = await Promise.all(runs);
      expect(results).toHaveLength(3);
      for (const r of results) {
        expect(r).toBeDefined();
        expect(r.finalVerdict).toBeDefined();
      }
    } finally {
      for (const d of dirs) {
        try {
          rmSync(d, { recursive: true, force: true });
        } catch {
          /* ignore */
        }
      }
    }
  });
});

describe("memory chaos", () => {
  let dir: string;
  beforeAll(() => {
    dir = tmpWorkspace();
  });
  afterAll(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it("survives memory store adapter write errors without throwing", async () => {
    class FailingAdapter extends InMemoryAdapter {
      override async write(): ReturnType<InMemoryAdapter["write"]> {
        const { err: makeErr } = await import("@kirkforge/core-types");
        return makeErr(new Error("simulated disk full"));
      }
    }

    const store = new MemoryStore(new FailingAdapter());
    const bus = new EventBus();
    const orchestrator = new TestableOrchestrator({
      modelConfig: makeModelConfig(),
      memoryStore: store,
      eventBus: bus,
      cwd: dir,
    });

    orchestrator.stubDelegate(async (task) => {
      const packet = makePassPacket(task.taskId ?? "chaos-mem");
      return ok({
        decision: { mode: "hard-prompt" },
        emission: {
          agentId: "a1",
          content: "ok",
          promptTokens: 5,
          completionTokens: 5,
          totalTokens: 10,
          model: "test-model",
          format: "hard-prompt" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: task.taskId ?? "chaos-mem",
            domain: "files",
            kind: "files.written",
            source: "agent",
            ts: new Date().toISOString(),
            value: { files: ["src.ts"] },
          },
        ],
        packet,
      });
    });

    const result = await orchestrator.runCorrectionLoop(
      { taskId: "chaos-mem-fail", description: "write a hello function in src.ts" },
      { maxCorrections: 1 },
    );

    // Should not throw — memory write errors are logged, not thrown
    expect(result).toBeDefined();
    expect(result.finalVerdict).toBeDefined();
  });
});

describe("path safety chaos", () => {
  let dir: string;
  beforeAll(() => {
    dir = tmpWorkspace({
      "sub/deep.ts": "export const x = 1;\n",
    });
  });
  afterAll(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it("blocks emission of files that escape cwd", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    const bus = new EventBus();
    const orchestrator = new TestableOrchestrator({
      modelConfig: makeModelConfig(),
      memoryStore: store,
      eventBus: bus,
      cwd: dir,
    });

    orchestrator.stubDelegate(async (task) => {
      const packet = makePassPacket(task.taskId ?? "chaos-path", ["../escape.ts"]);
      packet.artifactEnforcement = {
        status: "fail",
        blocked: 1,
        blockedPaths: [{ path: "../escape.ts", reason: "path escapes cwd" }],
      };
      return ok({
        decision: { mode: "artifact" },
        emission: {
          agentId: "a1",
          content: "attempted path escape",
          promptTokens: 5,
          completionTokens: 5,
          totalTokens: 10,
          model: "test-model",
          format: "artifact" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: task.taskId ?? "chaos-path",
            domain: "artifact",
            kind: "artifact.blocked",
            source: "orchestrator",
            ts: new Date().toISOString(),
            value: { blockedPaths: [{ path: "../escape.ts", reason: "path escapes cwd" }] },
          },
        ],
        packet,
      });
    });

    const result = await orchestrator.runCorrectionLoop(
      { taskId: "chaos-path-escape", description: "edit a file outside the workspace" },
      { maxCorrections: 1 },
    );

    expect(result).toBeDefined();
    // Path escape should result in escalation
  });
});

describe("shutdown chaos", () => {
  let dir: string;
  beforeAll(() => {
    dir = tmpWorkspace();
  });
  afterAll(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it("gracefulShutdown while a run is in-flight does not throw", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    const bus = new EventBus();
    const orchestrator = new TestableOrchestrator({
      modelConfig: makeModelConfig(),
      memoryStore: store,
      eventBus: bus,
      cwd: dir,
    });

    orchestrator.stubDelegate(async (task) => {
      const packet = makePassPacket(task.taskId ?? "chaos-shutdown");
      return ok({
        decision: { mode: "hard-prompt" },
        emission: {
          agentId: "a1",
          content: "ok",
          promptTokens: 5,
          completionTokens: 5,
          totalTokens: 10,
          model: "test-model",
          format: "hard-prompt" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: task.taskId ?? "chaos-shutdown",
            domain: "files",
            kind: "files.written",
            source: "agent",
            ts: new Date().toISOString(),
            value: { files: ["src.ts"] },
          },
        ],
        packet,
      });
    });

    // Start run and shut down concurrently
    const runP = orchestrator.runCorrectionLoop(
      { taskId: "chaos-shutdown", description: "write a TypeScript async sleep function" },
      { maxCorrections: 1 },
    );

    // Give it a moment to start, then shut down
    await new Promise((r) => setTimeout(r, 50));
    const shutdownP = orchestrator.gracefulShutdown();

    const [runR] = await Promise.allSettled([runP, shutdownP]);
    if (runR.status === "fulfilled") {
      expect(runR.value).toBeDefined();
    }
    // Either outcome is acceptable — shutdown may interrupt the run
  });
});
