import { describe, it, expect } from "vitest";
import { recordObservation } from "@kirkforge/plugin";
import { FileAdapter, MemoryStore } from "@kirkforge/memory-palace";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

function makeStore(dir: string) {
  const adapter = new FileAdapter(join(dir, "mem.json"));
  const store = new MemoryStore(adapter);
  return { adapter, store };
}

const validObservation = {
  taskId: "test-task-1",
  description: "implement foo",
  language: "typescript",
  mode: "hard-prompt",
  model: "gpt-4",
  outcome: "pass" as const,
  durationMs: 5000,
};

describe("observe: argument validation", () => {
  it("rejects missing memoryStore", async () => {
    const result = await recordObservation(validObservation);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("MemoryStore");
    }
  });
});

describe("observe: output shape", () => {
  it("writes observation and returns ok", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-observe-"));
    try {
      const { adapter, store } = makeStore(dir);
      const result = await recordObservation(validObservation, store);
      expect(result.ok).toBe(true);
      await adapter.persist();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("writes observation with optional tokens field", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-observe-"));
    try {
      const { adapter, store } = makeStore(dir);
      const result = await recordObservation({ ...validObservation, tokens: 1200 }, store);
      expect(result.ok).toBe(true);
      await adapter.persist();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("writes with outcome escalate mapping to error", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-observe-"));
    try {
      const { adapter, store } = makeStore(dir);
      const result = await recordObservation({ ...validObservation, outcome: "escalate" }, store);
      expect(result.ok).toBe(true);
      await adapter.persist();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("persists data that can be queried back", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-observe-"));
    try {
      const { adapter, store } = makeStore(dir);
      await recordObservation(validObservation, store);
      await adapter.persist();

      const statsResult = await store.adapter.stats();
      expect(statsResult.ok).toBe(true);
      if (statsResult.ok) {
        expect(statsResult.value.totalObjects).toBeGreaterThanOrEqual(1);
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("round-trips escalate outcome as error in stored object (distinct from fail)", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-observe-"));
    try {
      const { adapter, store } = makeStore(dir);
      await recordObservation({ ...validObservation, outcome: "escalate" }, store);
      await adapter.persist();

      const queryResult = await adapter.query({ kind: "task-observation" });
      expect(queryResult.ok).toBe(true);
      if (queryResult.ok) {
        expect(queryResult.value.length).toBeGreaterThanOrEqual(1);
        const obs = queryResult.value[0];
        expect(obs).toBeDefined();
        expect(obs?.properties?.outcome).toBe("error");
      }
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
