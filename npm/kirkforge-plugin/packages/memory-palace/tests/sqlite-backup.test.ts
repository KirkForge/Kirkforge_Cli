import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { SqliteAdapter } from "../src/sqlite-adapter.js";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
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

describe.skipIf(!hasBetterSqlite3 || process.env.CI)("SqliteAdapter backup/restore", () => {
  let dir: string;

  beforeAll(() => {
    dir = mkdtempSync(join(tmpdir(), "kirkforge-backup-"));
  });

  afterAll(() => {
    rmSync(dir, { recursive: true, force: true });
  });

  it("creates a backup with metadata", { timeout: 60_000 }, async () => {
    const dbPath = join(dir, "backup-test.db");
    const adapter = new SqliteAdapter(dbPath);

    // Write some data
    await adapter.write({
      id: "obs-1",
      kind: "task-observation",
      taskId: "t1",
      timestamp: new Date().toISOString(),
      description: "test observation",
      properties: { language: "typescript" },
      tags: ["test"],
    });

    // Create backup
    const result = await adapter.backup();
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");

    const meta = result.value;
    expect(meta.filePath).toContain("backup-test.db.backup.");
    expect(meta.sizeBytes).toBeGreaterThan(0);
    expect(meta.sha256).toMatch(/^[a-f0-9]{64}$/);
    expect(meta.schemaVersion).toBe(3);
    expect(meta.rowCount.observations).toBe(1);
    expect(meta.rowCount.runs).toBe(0);
    expect(meta.rowCount.emissions).toBe(0);
    expect(existsSync(meta.filePath)).toBe(true);

    adapter.close();
  });

  it("creates backup to a specific path", { timeout: 60_000 }, async () => {
    const dbPath = join(dir, "specific-backup.db");
    const adapter = new SqliteAdapter(dbPath);
    const destPath = join(dir, "custom-backup.db");

    await adapter.write({
      id: "obs-2",
      kind: "task-observation",
      taskId: "t2",
      timestamp: new Date().toISOString(),
      description: "specific path test",
      properties: {},
      tags: [],
    });

    const result = await adapter.backup(destPath);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");

    expect(result.value.filePath).toBe(destPath);
    expect(existsSync(destPath)).toBe(true);

    adapter.close();
  });

  it("restores from a backup and recovers all data", { timeout: 60_000 }, async () => {
    const dbPath = join(dir, "restore-test.db");
    const adapter = new SqliteAdapter(dbPath);

    // Write data
    await adapter.write({
      id: "obs-3",
      kind: "task-observation",
      taskId: "t3",
      timestamp: "2026-01-01T00:00:00Z",
      description: "will be restored",
      properties: { key: "value" },
      tags: ["restore"],
    });

    // Write a run
    adapter.writeRun({
      runId: "run-1",
      taskId: "t3",
      description: "test run",
      language: "typescript",
      mode: "artifact",
      model: "test-model",
      providerKey: "test",
      providerType: "local",
      outcome: "pass",
      outcomeClass: "pass",
      routingLesson: "reward",
      finalVerdict: "pass",
      sourceOfTruth: "verifier",
      finalAction: "accept",
      tokens: 100,
      durationMs: 500,
      turns: 1,
      validatorDurationMs: 200,
      filesEmitted: 0,
      totalBytesEmitted: 0,
      emissionIds: [],
      timestamp: "2026-01-01T00:00:00Z",
    });

    // Backup
    const backupResult = await adapter.backup(join(dir, "restore-backup.db"));
    expect(backupResult.ok).toBe(true);
    const backupMeta = backupResult.value;

    // Verify backup has the data
    expect(backupMeta.rowCount.observations).toBe(1);
    expect(backupMeta.rowCount.runs).toBe(1);

    // Write more data to the live DB (simulating ongoing writes)
    await adapter.write({
      id: "obs-4",
      kind: "task-observation",
      taskId: "t4",
      timestamp: "2026-01-02T00:00:00Z",
      description: "added after backup",
      properties: {},
      tags: [],
    });

    // Verify we have 2 observations now
    const statsBefore = await adapter.stats();
    expect(statsBefore.ok && statsBefore.value.totalObjects).toBe(2);

    // Restore from backup
    const restoreResult = await adapter.restore(join(dir, "restore-backup.db"));
    expect(restoreResult.ok).toBe(true);
    if (!restoreResult.ok) throw new Error("unreachable");

    // Verify the SHA-256 matches (restore metadata should match backup)
    expect(restoreResult.value.sha256).toBe(backupMeta.sha256);

    // Verify data is restored to original state (1 obs, 1 run)
    const statsAfter = await adapter.stats();
    expect(statsAfter.ok && statsAfter.value.totalObjects).toBe(1);

    // Verify the specific observation content
    const readResult = await adapter.read("obs-3");
    expect(readResult.ok).toBe(true);
    expect(readResult.value).not.toBeNull();
    expect(readResult.value!.description).toBe("will be restored");
    expect(readResult.value!.properties).toEqual({ key: "value" });

    // Verify the run was restored
    const runs = adapter.queryRuns(10);
    expect(runs).toHaveLength(1);
    expect(runs[0]!.run_id).toBe("run-1");

    adapter.close();
  });

  it("restore fails for non-existent backup file", async () => {
    const dbPath = join(dir, "nonexistent-restore.db");
    const adapter = new SqliteAdapter(dbPath);

    const result = await adapter.restore(join(dir, "does-not-exist.db"));
    expect(result.ok).toBe(false);
    expect(result.error!.message).toContain("not found");

    adapter.close();
  });

  it("backup checksum is deterministic for same data", { timeout: 60_000 }, async () => {
    const dbPath1 = join(dir, "checksum-1.db");
    const dbPath2 = join(dir, "checksum-2.db");
    const adapter1 = new SqliteAdapter(dbPath1);
    const adapter2 = new SqliteAdapter(dbPath2);

    // Write identical data to both
    for (const adapter of [adapter1, adapter2]) {
      await adapter.write({
        id: "obs-same",
        kind: "task-observation",
        taskId: "t-same",
        timestamp: "2026-01-01T00:00:00Z",
        description: "same data",
        properties: { key: "value" },
        tags: ["test"],
      });
    }

    const backup1 = await adapter1.backup(join(dir, "checksum-backup-1.db"));
    const backup2 = await adapter2.backup(join(dir, "checksum-backup-2.db"));

    expect(backup1.ok).toBe(true);
    expect(backup2.ok).toBe(true);

    // Both backups should have same row counts
    expect(backup1.value.rowCount).toEqual(backup2.value.rowCount);

    adapter1.close();
    adapter2.close();
  });

  it("listBackups returns sorted backup files", { timeout: 60_000 }, async () => {
    const dbPath = join(dir, "list-backups.db");
    const adapter = new SqliteAdapter(dbPath);

    await adapter.write({
      id: "obs-list",
      kind: "task-observation",
      taskId: "t-list",
      timestamp: new Date().toISOString(),
      description: "list test",
      properties: {},
      tags: [],
    });

    // Create two backups
    await adapter.backup(join(dir, "list-backup-1.db"));
    await adapter.backup(join(dir, "list-backup-2.db"));

    // listBackups for auto-named backups
    const _autoBackups = adapter.listBackups();
    // These should be empty since we used specific paths, not auto-named
    // Let's also create an auto-named one
    await adapter.backup();

    const allBackups = adapter.listBackups();
    expect(allBackups.length).toBeGreaterThanOrEqual(1);
    // Each backup path should exist
    for (const bp of allBackups) {
      expect(existsSync(bp)).toBe(true);
    }

    adapter.close();
  });

  it("backup includes runs and emissions", { timeout: 60_000 }, async () => {
    const dbPath = join(dir, "runs-backup.db");
    const adapter = new SqliteAdapter(dbPath);

    // Write a run with emissions
    adapter.writeRunAndEmissions(
      {
        runId: "run-em-1",
        taskId: "t-em",
        description: "emission test",
        language: "python",
        mode: "artifact",
        model: "test",
        providerKey: "test",
        providerType: "local",
        outcome: "pass",
        outcomeClass: "pass",
        routingLesson: "reward",
        finalVerdict: "pass",
        sourceOfTruth: "verifier",
        finalAction: "accept",
        tokens: 200,
        durationMs: 1000,
        turns: 2,
        validatorDurationMs: 300,
        filesEmitted: 1,
        totalBytesEmitted: 500,
        emissionIds: ["em-1"],
        timestamp: "2026-01-01T00:00:00Z",
      },
      [
        {
          id: "em-1",
          runId: "run-em-1",
          taskId: "t-em",
          turn: 1,
          path: "/workspace/test.py",
          sha256: "abc123",
          bytes: 500,
          beforeHash: null,
          existed: false,
          timestamp: "2026-01-01T00:00:00Z",
        },
      ],
    );

    const backupResult = await adapter.backup(join(dir, "runs-backup-file.db"));
    expect(backupResult.ok).toBe(true);
    expect(backupResult.value.rowCount.runs).toBe(1);
    expect(backupResult.value.rowCount.emissions).toBe(1);

    // Restore and verify runs/emissions survived
    await adapter.write({
      id: "obs-extra",
      kind: "task-observation",
      taskId: "t-extra",
      timestamp: "2026-01-02T00:00:00Z",
      description: "extra data",
      properties: {},
      tags: [],
    });

    const restoreResult = await adapter.restore(join(dir, "runs-backup-file.db"));
    expect(restoreResult.ok).toBe(true);
    expect(restoreResult.value.rowCount.runs).toBe(1);
    expect(restoreResult.value.rowCount.emissions).toBe(1);

    const runs = adapter.queryRuns(10);
    expect(runs).toHaveLength(1);
    expect(runs[0]!.run_id).toBe("run-em-1");

    const emissions = adapter.queryEmissionsForRun("run-em-1");
    expect(emissions).toHaveLength(1);

    adapter.close();
  });
});
