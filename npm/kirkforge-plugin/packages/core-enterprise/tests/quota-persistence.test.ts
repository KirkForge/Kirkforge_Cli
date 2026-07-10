import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { QuotaManager } from "../src/quotas.js";
import { QuotaPersistence } from "../src/quota-persistence.js";
import { mkdtempSync, rmSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

describe("QuotaPersistence", () => {
  let tmpDir: string;
  let filePath: string;

  beforeEach(() => {
    tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-quota-persist-"));
    filePath = join(tmpDir, "quotas.json");
  });

  afterEach(() => {
    rmSync(tmpDir, { recursive: true, force: true });
  });

  it("saves and loads quota state", () => {
    const manager = new QuotaManager();
    const persistence = new QuotaPersistence(manager, { filePath });

    // Set quotas and usage
    manager.setQuota("tenant-1", { maxConcurrentTasks: 8, maxDailyTokens: 2000000 });
    manager.recordUsage("tenant-1", { concurrentTasks: 3, dailyTokens: 500000 });

    const saveResult = persistence.save();
    expect(saveResult.ok).toBe(true);

    // Create a new manager and persistence, then load
    const manager2 = new QuotaManager();
    const persistence2 = new QuotaPersistence(manager2, { filePath });
    const loadResult = persistence2.load();
    expect(loadResult.ok).toBe(true);

    const quota = manager2.getQuota("tenant-1");
    expect(quota.maxConcurrentTasks).toBe(8);
    expect(quota.maxDailyTokens).toBe(2000000);
  });

  it("returns ok when file does not exist", () => {
    const manager = new QuotaManager();
    const persistence = new QuotaPersistence(manager, {
      filePath: join(tmpDir, "nonexistent.json"),
    });
    const result = persistence.load();
    expect(result.ok).toBe(true);
  });

  it("detects corrupted state with hash mismatch", () => {
    const manager = new QuotaManager();
    const persistence = new QuotaPersistence(manager, { filePath });

    manager.setQuota("tenant-1", { maxConcurrentTasks: 4 });
    persistence.save();

    // Corrupt the file by modifying the content hash
    const raw = readFileSync(filePath, "utf-8");
    const state = JSON.parse(raw);
    state.contentHash = "corrupted-hash";
    writeFileSync(filePath, JSON.stringify(state, null, 2), "utf-8");

    const manager2 = new QuotaManager();
    const persistence2 = new QuotaPersistence(manager2, { filePath });
    const result = persistence2.load();
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("integrity check failed");
    }
  });

  it("handles missing directory by creating it", () => {
    const nestedPath = join(tmpDir, "nested", "dir", "quotas.json");
    const manager = new QuotaManager();
    const persistence = new QuotaPersistence(manager, { filePath: nestedPath });

    manager.setQuota("tenant-1", { maxConcurrentTasks: 2 });
    const result = persistence.save();
    expect(result.ok).toBe(true);
  });

  it("markDirty marks state for auto-save", () => {
    const manager = new QuotaManager();
    const persistence = new QuotaPersistence(manager, { filePath });
    // markDirty is a no-op setter for dirty flag — just ensuring it doesn't throw
    persistence.markDirty();
  });

  it("auto-save can be started and stopped", () => {
    const manager = new QuotaManager();
    const persistence = new QuotaPersistence(manager, {
      filePath,
      autoSaveIntervalMs: 100, // Fast for testing
    });

    persistence.startAutoSave();
    // Should not throw
    persistence.stopAutoSave();
  });

  it("saves empty state correctly", () => {
    const manager = new QuotaManager();
    const persistence = new QuotaPersistence(manager, { filePath });

    const result = persistence.save();
    expect(result.ok).toBe(true);

    // Load into a fresh manager
    const manager2 = new QuotaManager();
    const persistence2 = new QuotaPersistence(manager2, { filePath });
    const loadResult = persistence2.load();
    expect(loadResult.ok).toBe(true);

    // Should have default quotas for any tenant
    const quota = manager2.getQuota("unknown-tenant");
    expect(quota.maxConcurrentTasks).toBe(4); // DEFAULT_QUOTA
  });
});
