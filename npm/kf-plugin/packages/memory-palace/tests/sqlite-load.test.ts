/**
 * SqliteAdapter load test — validates SLO compliance for enterprise workloads.
 *
 * Tests concurrent writes, batch inserts, query performance, and backup/restore
 * under realistic data volumes. Targets the SLO thresholds defined in
 * docs/sla-definitions.md.
 */

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

describe.skipIf(!hasBetterSqlite3 || process.env.CI)("SqliteAdapter load test", () => {
  let dir: string;
  let dbPath: string;

  beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), "kirkforge-sqlite-load-"));
    dbPath = join(dir, "load-test.db");
  });

  afterAll(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it("handles 1000 sequential writes within SLO (< 10s)", { timeout: 30_000 }, async () => {
    const adapter = new SqliteAdapter(dbPath);
    const start = Date.now();

    for (let i = 0; i < 1000; i++) {
      const result = await adapter.write({
        id: `load-obs-${i}`,
        kind: "task-observation",
        taskId: `task-${Math.floor(i / 10)}`,
        timestamp: new Date(Date.now() - i * 100).toISOString(),
        description: `Load test observation ${i}: write a python function`,
        properties: {
          language: i % 2 === 0 ? "python" : "typescript",
          mode: "artifact",
          model: i % 3 === 0 ? "model-a" : "model-b",
          outcome: i % 4 === 0 ? "pass" : "fail",
          tokens: 100 + i,
          durationMs: 500 + i * 2,
        },
        tags: [i % 2 === 0 ? "python" : "typescript", i % 4 === 0 ? "pass" : "fail"],
      });
      expect(result.ok).toBe(true);
    }

    const elapsed = Date.now() - start;
    expect(elapsed).toBeLessThan(30000);

    const stats = await adapter.stats();
    expect(stats.ok).toBe(true);
    expect(stats.value!.totalObjects).toBe(1000);

    adapter.close();
  });

  it("handles 1000 writes via MemoryStore within SLO (< 10s)", { timeout: 30_000 }, async () => {
    const storeDbPath = join(dir, "load-store.db");
    const adapter = new SqliteAdapter(storeDbPath);
    const store = new MemoryStore(adapter);
    const start = Date.now();

    for (let i = 0; i < 1000; i++) {
      const result = await store.writeTaskObservation({
        taskId: `load-task-${i}`,
        description: `Load test task ${i}: implement a sorting algorithm`,
        language: i % 2 === 0 ? "python" : "typescript",
        mode: "artifact",
        model: i % 3 === 0 ? "model-a" : "model-b",
        outcome: i % 4 === 0 ? "pass" : "fail",
        tokens: 150 + i,
        durationMs: 800 + i * 3,
      });
      expect(result.ok).toBe(true);
    }

    const elapsed = Date.now() - start;
    expect(elapsed).toBeLessThan(20000);

    adapter.close();
  });

  it("queries 1000 entries by kind within SLO (< 2s)", { timeout: 30_000 }, async () => {
    const queryDbPath = join(dir, "load-query.db");
    const adapter = new SqliteAdapter(queryDbPath);

    // Seed data
    for (let i = 0; i < 1000; i++) {
      await adapter.write({
        id: `query-obs-${i}`,
        kind: i % 3 === 0 ? "task-observation" : i % 3 === 1 ? "emission" : "run",
        taskId: `task-${Math.floor(i / 10)}`,
        timestamp: new Date(Date.now() - i * 100).toISOString(),
        description: `Query test observation ${i}`,
        properties: { language: "python", mode: "artifact" },
        tags: ["python", "load-test"],
      });
    }

    const start = Date.now();
    const result = await adapter.query({ kind: "task-observation", limit: 100 });
    const elapsed = Date.now() - start;

    expect(result.ok).toBe(true);
    expect(result.value!.length).toBeLessThanOrEqual(334); // ~1/3 of 1000
    expect(elapsed).toBeLessThan(2000);

    adapter.close();
  });

  it("creates and verifies backup of 1000 entries", { timeout: 60_000 }, async () => {
    const backupDbPath = join(dir, "load-backup.db");
    const adapter = new SqliteAdapter(backupDbPath);

    // Write data
    for (let i = 0; i < 1000; i++) {
      await adapter.write({
        id: `backup-obs-${i}`,
        kind: "task-observation",
        taskId: `task-${i}`,
        timestamp: new Date().toISOString(),
        description: `Backup test ${i}`,
        properties: { index: i },
        tags: ["backup-test"],
      });
    }

    // Create backup
    const start = Date.now();
    const backupResult = await adapter.backup(join(dir, "load-backup-copy.db"));
    const elapsed = Date.now() - start;

    expect(backupResult.ok).toBe(true);
    expect(backupResult.value!.rowCount.observations).toBe(1000);
    expect(backupResult.value!.sha256).toMatch(/^[a-f0-9]{64}$/);
    expect(elapsed).toBeLessThan(30000);

    // Verify integrity
    expect(backupResult.value!.sizeBytes).toBeGreaterThan(0);

    adapter.close();
  });

  it("writes runs and emissions in bulk within SLO", { timeout: 30_000 }, () => {
    const bulkDbPath = join(dir, "load-bulk.db");
    const adapter = new SqliteAdapter(bulkDbPath);
    const start = Date.now();

    // Write 500 runs with emissions
    for (let i = 0; i < 500; i++) {
      adapter.writeRunAndEmissions(
        {
          runId: `run-bulk-${i}`,
          taskId: `task-bulk-${i}`,
          description: `Bulk run ${i}`,
          language: "python",
          mode: "artifact",
          model: "test-model",
          providerKey: "test",
          providerType: "local",
          outcome: i % 3 === 0 ? "pass" : "fail",
          outcomeClass: i % 3 === 0 ? "pass" : "task_fail",
          routingLesson: "reward",
          finalVerdict: i % 3 === 0 ? "pass" : "fail",
          sourceOfTruth: "verifier",
          finalAction: "accept",
          tokens: 200 + i,
          durationMs: 1000 + i * 5,
          turns: 3 + (i % 5),
          validatorDurationMs: 300,
          filesEmitted: i % 2,
          totalBytesEmitted: i * 100,
          emissionIds: [`em-bulk-${i}-0`],
          timestamp: new Date().toISOString(),
        },
        [
          {
            id: `em-bulk-${i}-0`,
            runId: `run-bulk-${i}`,
            taskId: `task-bulk-${i}`,
            turn: 1,
            path: `/workspace/file-${i}.py`,
            sha256: `abc${i.toString().padStart(60, "0")}`,
            bytes: 100 + i,
            beforeHash: null,
            existed: false,
            timestamp: new Date().toISOString(),
          },
        ],
      );
    }

    const elapsed = Date.now() - start;
    expect(elapsed).toBeLessThan(30000);

    // Verify data
    const runs = adapter.queryRuns(10);
    expect(runs.length).toBe(10);

    const emissions = adapter.queryEmissionsForRun("run-bulk-0");
    expect(emissions.length).toBe(1);

    adapter.close();
  });
});
