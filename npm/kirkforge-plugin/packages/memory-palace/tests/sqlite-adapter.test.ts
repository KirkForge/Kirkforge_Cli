import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { SqliteAdapter } from "../src/sqlite-adapter.js";
import { MemoryStore } from "../src/index.js";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

let hasBetterSqlite3 = false;
try {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const Database = require("better-sqlite3");
  const db = new Database(":memory:");
  db.close();
  hasBetterSqlite3 = true;
} catch {
  /* native binding unavailable */
}

describe.skipIf(!hasBetterSqlite3)("SqliteAdapter", () => {
  let dir: string;
  let dbPath: string;

  beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), "kirkforge-sqlite-"));
    dbPath = join(dir, "memory.db");
  });

  afterAll(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it("writes and reads an object", async () => {
    const adapter = new SqliteAdapter(dbPath);
    const result = await adapter.write({
      id: "obj-1",
      kind: "test",
      taskId: "task-1",
      timestamp: new Date().toISOString(),
      description: "test write",
      properties: { language: "typescript", mode: "artifact" },
      tags: ["coding", "ts"],
    });
    expect(result.ok).toBe(true);

    const readResult = await adapter.read("obj-1");
    expect(readResult.ok).toBe(true);
    expect(readResult.value).not.toBeNull();
    expect(readResult.value!.id).toBe("obj-1");
    expect(readResult.value!.properties.language).toBe("typescript");
    adapter.close();
  });

  it("returns null for missing object", async () => {
    const adapter = new SqliteAdapter(dbPath);
    const result = await adapter.read("nonexistent");
    expect(result.ok).toBe(true);
    expect(result.value).toBeNull();
    adapter.close();
  });

  it("queries by kind", async () => {
    const adapter = new SqliteAdapter(dbPath);
    await adapter.write({
      id: "q-1",
      kind: "alpha",
      taskId: "t1",
      timestamp: "2026-01-01T00:00:00Z",
      description: "alpha one",
      properties: {},
      tags: [],
    });
    await adapter.write({
      id: "q-2",
      kind: "beta",
      taskId: "t2",
      timestamp: "2026-01-02T00:00:00Z",
      description: "beta one",
      properties: {},
      tags: [],
    });

    const result = await adapter.query({ kind: "alpha" });
    expect(result.ok).toBe(true);
    expect(result.value!.length).toBe(1);
    expect(result.value![0]!.id).toBe("q-1");

    adapter.close();
  });

  it("queries with limit", async () => {
    const adapter = new SqliteAdapter(dbPath);
    const result = await adapter.query({ limit: 1 });
    expect(result.ok).toBe(true);
    expect(result.value!.length).toBeLessThanOrEqual(1);
    adapter.close();
  });

  it("reports stats", async () => {
    const adapter = new SqliteAdapter(dbPath);
    const result = await adapter.stats();
    expect(result.ok).toBe(true);
    expect(result.value!.totalObjects).toBeGreaterThan(0);
    adapter.close();
  });

  it("works with MemoryStore", async () => {
    const adapter = new SqliteAdapter(dbPath);
    const store = new MemoryStore(adapter);

    const writeResult = await store.writeTaskObservation({
      taskId: "store-1",
      description: "write a python script",
      language: "python",
      mode: "artifact",
      model: "test-model",
      outcome: "pass",
      tokens: 100,
      durationMs: 500,
    });
    expect(writeResult.ok).toBe(true);

    const recallResult = await store.recall("write a python script", "test-model");
    expect(recallResult.ok).toBe(true);
    expect(recallResult.value).not.toBeNull();

    adapter.close();
  });
});
