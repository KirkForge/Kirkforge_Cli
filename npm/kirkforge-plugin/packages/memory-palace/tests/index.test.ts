import { describe, it, expect } from "vitest";
import { InMemoryAdapter, FileAdapter, MemoryStore } from "../src/index.js";
import { mkdtempSync, rmSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

describe("MemoryStore", () => {
  it("writes and reads objects", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    await store.adapter.write({
      id: "test-1",
      kind: "task-observation",
      taskId: "t1",
      timestamp: "now",
      description: "build a thing",
      properties: { mode: "hard-prompt", score: 25 },
      tags: ["task"],
    });
    const read = await store.adapter.read("test-1");
    expect(read.ok && read.value?.id).toBe("test-1");
  });

  it("queries by kind", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    await store.adapter.write({
      id: "a",
      kind: "task-observation",
      taskId: "t1",
      timestamp: "now",
      description: "x",
      properties: {},
      tags: [],
    });
    await store.adapter.write({
      id: "b",
      kind: "benchmark.run",
      taskId: "t2",
      timestamp: "now",
      description: "y",
      properties: {},
      tags: [],
    });
    const result = await store.adapter.query({ kind: "task-observation" });
    expect(result.ok && result.value).toHaveLength(1);
  });

  it("recall returns recommendations with confidence", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    await store.writeTaskObservation({
      taskId: "t1",
      description: "build a component",
      language: "typescript",
      mode: "schema-contract",
      model: "gpt-4",
      tokens: 500,
      durationMs: 100,
    });
    await store.writeTaskObservation({
      taskId: "t2",
      description: "build a component",
      language: "typescript",
      mode: "schema-contract",
      model: "gpt-4",
      tokens: 480,
      durationMs: 90,
    });
    await store.writeTaskObservation({
      taskId: "t3",
      description: "build a component",
      language: "typescript",
      mode: "hard-prompt",
      model: "gpt-4",
      tokens: 800,
      durationMs: 200,
    });
    const rec = await store.recall("build a component");
    expect(rec.ok).toBe(true);
  });

  it("recall uses empirical similar task outcomes for routing bias", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    await store.writeTaskObservation({
      taskId: "fail-rnj",
      description: "fix broken-python pip dependency repair",
      language: "python",
      mode: "artifact",
      model: "rnj-1:8b-cloud",
      verifierOverall: "fail",
      finalAction: "escalate",
      outcome: "fail",
      reason: "misread dependency repair task",
      tokens: 1046,
      durationMs: 40000,
    });
    await store.writeTaskObservation({
      taskId: "pass-glm",
      description: "repair broken-python requirements and make pytest pass",
      language: "python",
      mode: "artifact",
      model: "glm-4.7:cloud",
      verifierOverall: "pass",
      finalAction: "accept",
      outcome: "pass",
      reason: "task tests passed",
      tokens: 3812,
      durationMs: 120000,
    });

    const rec = await store.recall("broken-python pip dependency repair");
    expect(rec.ok && rec.value?.routingBias?.prefer).toContain("glm-4.7:cloud");
    expect(rec.ok && rec.value?.routingBias?.avoid).toContain("rnj-1:8b-cloud");
    expect(rec.ok && rec.value?.routingBias?.influence).toBe(0.25);
  });

  it("stats returns count", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    await store.adapter.write({
      id: "s1",
      kind: "task-observation",
      taskId: "t1",
      timestamp: "now",
      description: "x",
      properties: {},
      tags: [],
    });
    const stats = await store.adapter.stats();
    expect(stats.ok && stats.value.totalObjects).toBe(1);
  });
});

describe("inferOutcome (writeTaskObservation)", () => {
  it("taskPass true produces outcome pass", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    await store.writeTaskObservation({
      taskId: "t1",
      description: "test",
      language: "typescript",
      mode: "artifact",
      model: "gpt-4",
      taskPass: true,
      tokens: 100,
      durationMs: 1000,
    });
    const obs = await adapter.query({ kind: "task-observation", limit: 1 });
    expect(obs.ok && obs.value[0]!.properties.outcome).toBe("pass");
  });

  it("taskPass false produces outcome fail", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    await store.writeTaskObservation({
      taskId: "t2",
      description: "test",
      language: "typescript",
      mode: "artifact",
      model: "gpt-4",
      taskPass: false,
      tokens: 100,
      durationMs: 1000,
    });
    const obs = await adapter.query({ kind: "task-observation", limit: 1 });
    expect(obs.ok && obs.value[0]!.properties.outcome).toBe("fail");
  });

  it("finalAction accept + verifierOverall pass + no taskPass/outcome produces error, not pass", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    await store.writeTaskObservation({
      taskId: "t3",
      description: "test",
      language: "typescript",
      mode: "artifact",
      model: "gpt-4",
      finalAction: "accept",
      verifierOverall: "pass",
      tokens: 100,
      durationMs: 1000,
    });
    const obs = await adapter.query({ kind: "task-observation", limit: 1 });
    expect(obs.ok && obs.value[0]!.properties.outcome).toBe("error");
  });

  it("finalAction escalate + verifierOverall fail + no taskPass/outcome produces error, not fail", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    await store.writeTaskObservation({
      taskId: "t4",
      description: "test",
      language: "typescript",
      mode: "artifact",
      model: "gpt-4",
      finalAction: "escalate",
      verifierOverall: "fail",
      tokens: 100,
      durationMs: 1000,
    });
    const obs = await adapter.query({ kind: "task-observation", limit: 1 });
    expect(obs.ok && obs.value[0]!.properties.outcome).toBe("error");
  });

  it("explicit outcome overrides all inference", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    await store.writeTaskObservation({
      taskId: "t5",
      description: "test",
      language: "typescript",
      mode: "artifact",
      model: "gpt-4",
      taskPass: false,
      outcome: "pass",
      tokens: 100,
      durationMs: 1000,
    });
    const obs = await adapter.query({ kind: "task-observation", limit: 1 });
    expect(obs.ok && obs.value[0]!.properties.outcome).toBe("pass");
  });

  it("explicit outcome fail overrides taskPass true", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    await store.writeTaskObservation({
      taskId: "t6",
      description: "test",
      language: "typescript",
      mode: "artifact",
      model: "gpt-4",
      taskPass: true,
      outcome: "fail",
      tokens: 100,
      durationMs: 1000,
    });
    const obs = await adapter.query({ kind: "task-observation", limit: 1 });
    expect(obs.ok && obs.value[0]!.properties.outcome).toBe("fail");
  });

  it("reason defaults to task outcome unknown when no task outcome", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    await store.writeTaskObservation({
      taskId: "t7",
      description: "test",
      language: "typescript",
      mode: "artifact",
      model: "gpt-4",
      finalAction: "accept",
      verifierOverall: "pass",
      tokens: 100,
      durationMs: 1000,
    });
    const obs = await adapter.query({ kind: "task-observation", limit: 1 });
    expect(obs.ok && obs.value[0]!.properties.reason).toBe("task outcome unknown");
  });
});

describe("FileAdapter", () => {
  it("persists and reloads across instances", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-mem-"));
    const filePath = join(dir, "memory.json");
    try {
      const a1 = new FileAdapter(filePath);
      await a1.write({
        id: "p1",
        kind: "task-observation",
        taskId: "t1",
        timestamp: "now",
        description: "build a thing",
        properties: { score: 25 },
        tags: [],
      });
      await (a1 as any).flush();

      const a2 = new FileAdapter(filePath);
      const r = await a2.read("p1");
      expect(r.ok && r.value?.id).toBe("p1");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("returns empty for missing file", async () => {
    const a = new FileAdapter("/tmp/nonexistent-kirkforge/memory.json");
    const r = await a.read("anything");
    expect(r.ok && r.value).toBeNull();
  });
});

describe("FileAdapter corruption safety", () => {
  it("corrupted memory file does not become empty memory silently", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-mem-corrupt-"));
    const filePath = join(dir, "memory.json");
    try {
      writeFileSync(filePath, "NOT VALID JSON {{{{");
      const adapter = new FileAdapter(filePath);
      const result = await adapter.write({
        id: "x",
        kind: "test",
        taskId: "t1",
        timestamp: "now",
        description: "test",
        properties: {},
        tags: [],
      });
      expect(result.ok).toBe(false);
      if (!result.ok) {
        expect(result.error.message).toContain("corrupted");
      }
      expect(existsSync(filePath + ".corrupt")).toBe(true);
      const backup = readFileSync(filePath + ".corrupt", "utf-8");
      expect(backup).toBe("NOT VALID JSON {{{{");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("non-array JSON is rejected as corrupted", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-mem-nonarray-"));
    const filePath = join(dir, "memory.json");
    try {
      writeFileSync(filePath, '{"key": "value"}');
      const adapter = new FileAdapter(filePath);
      const result = await adapter.write({
        id: "x",
        kind: "test",
        taskId: "t1",
        timestamp: "now",
        description: "test",
        properties: {},
        tags: [],
      });
      expect(result.ok).toBe(false);
      if (!result.ok) {
        expect(result.error.message).toContain("corrupted");
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("write creates valid JSON", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-mem-write-"));
    const filePath = join(dir, "memory.json");
    try {
      const adapter = new FileAdapter(filePath);
      const obj = {
        id: "w1",
        kind: "task-observation",
        taskId: "t1",
        timestamp: "2025-01-01T00:00:00.000Z",
        description: "build a thing",
        properties: { mode: "artifact", score: 42 },
        tags: ["task"],
      };
      await adapter.write(obj);
      await adapter.persist();
      const raw = readFileSync(filePath, "utf-8");
      const parsed = JSON.parse(raw);
      expect(Array.isArray(parsed)).toBe(true);
      expect(parsed).toHaveLength(1);
      expect(parsed[0].id).toBe("w1");
      expect(parsed[0].properties.score).toBe(42);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("recall limit prefers newest records", async () => {
    const adapter = new InMemoryAdapter();
    const _store = new MemoryStore(adapter);
    await adapter.write({
      id: "old",
      kind: "task-observation",
      taskId: "t1",
      timestamp: "2024-01-01T00:00:00.000Z",
      description: "old task",
      properties: { mode: "hard-prompt" },
      tags: ["task-observation"],
    });
    await adapter.write({
      id: "mid",
      kind: "task-observation",
      taskId: "t2",
      timestamp: "2024-06-15T00:00:00.000Z",
      description: "mid task",
      properties: { mode: "artifact" },
      tags: ["task-observation"],
    });
    await adapter.write({
      id: "new",
      kind: "task-observation",
      taskId: "t3",
      timestamp: "2025-01-01T00:00:00.000Z",
      description: "new task",
      properties: { mode: "schema-contract" },
      tags: ["task-observation"],
    });
    const result = await adapter.query({ kind: "task-observation", limit: 2 });
    expect(result.ok).toBe(true);
    expect(result.value).toHaveLength(2);
    expect(result.value[0].id).toBe("new");
    expect(result.value[1].id).toBe("mid");
  });

  it("concurrent writes are serialized per instance (documented limitation)", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-mem-concurrent-"));
    const filePath = join(dir, "memory.json");
    try {
      const adapter = new FileAdapter(filePath);
      const writes = [];
      for (let i = 0; i < 5; i++) {
        writes.push(
          adapter.write({
            id: `c${i}`,
            kind: "task-observation",
            taskId: `t${i}`,
            timestamp: `2025-01-0${i + 1}T00:00:00.000Z`,
            description: `concurrent ${i}`,
            properties: { index: i },
            tags: [],
          }),
        );
      }
      await Promise.all(writes);
      await adapter.persist();
      const raw = readFileSync(filePath, "utf-8");
      const parsed = JSON.parse(raw);
      expect(parsed).toHaveLength(5);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("MemoryStore decomposition", () => {
  it("writes and recalls a decomposition by taskId", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    const tasks = [
      {
        id: "setup",
        description: "Init project",
        language: "typescript",
        dependsOn: [] as string[],
        estimatedComplexity: "simple" as const,
        outputFiles: ["package.json"],
        verificationHint: "npm test",
      },
      {
        id: "build",
        description: "Build app",
        language: "typescript",
        dependsOn: ["setup"],
        estimatedComplexity: "moderate" as const,
        outputFiles: ["src/app.ts"],
        verificationHint: "tsc passes",
      },
    ];
    await store.writeDecomposition("task-123", "Build a full-stack app", tasks, "typescript");

    const recalled = await store.recallDecomposition("task-123");
    expect(recalled.ok).toBe(true);
    if (!recalled.ok) throw new Error("unreachable");
    expect(recalled.value).not.toBeNull();
    expect(recalled.value!.taskId).toBe("task-123");
    expect(recalled.value!.description).toBe("Build a full-stack app");
    expect(recalled.value!.tasks).toHaveLength(2);
    expect(recalled.value!.tasks[0]!.id).toBe("setup");
  });

  it("recalls by description substring", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    await store.writeDecomposition("t-1", "Build a REST API with auth", [], "typescript");
    await store.writeDecomposition("t-2", "Create a CLI tool", [], "rust");

    const recalled = await store.recallDecomposition("REST API");
    expect(recalled.ok).toBe(true);
    if (!recalled.ok) throw new Error("unreachable");
    expect(recalled.value).not.toBeNull();
    expect(recalled.value!.taskId).toBe("t-1");
  });

  it("returns null for no match", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    const recalled = await store.recallDecomposition("nonexistent");
    expect(recalled.ok).toBe(true);
    if (!recalled.ok) throw new Error("unreachable");
    expect(recalled.value).toBeNull();
  });

  it("returns null for empty store", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    const recalled = await store.recallDecomposition("anything");
    expect(recalled.ok).toBe(true);
    if (!recalled.ok) throw new Error("unreachable");
    expect(recalled.value).toBeNull();
  });
});
