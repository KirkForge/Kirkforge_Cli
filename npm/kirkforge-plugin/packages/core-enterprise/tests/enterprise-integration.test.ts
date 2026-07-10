import { describe, it, expect } from "vitest";
import {
  validateEnterpriseMode,
  enterpriseStartupGate,
  QuotaManager,
  QuotaPersistence,
  RateLimiter,
  DEFAULT_QUOTA as _DEFAULT_QUOTA,
} from "../src/index.js";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

// ── Enterprise mode integration tests ──────────────────────────────────────
// Tests that verify enterprise controls work together as a system.

describe("Enterprise mode integration", () => {
  const validEnterpriseEnv: Record<string, string | undefined> = {
    KIRKFORGE_ENTERPRISE_MODE: "1",
    HEALTH_API_KEY: "a".repeat(32),
    MEMORY_BACKEND: "sqlite",
    POLICY_FILE_PATH: "/tmp/test-policy.json",
    AUDIT_SINK_TYPE: "file",
    AUDIT_FILE_PATH: "/tmp/kirkforge-test-audit.jsonl",
  };

  it("rejects startup without auth in enterprise mode", () => {
    const env: Record<string, string | undefined> = {
      KIRKFORGE_ENTERPRISE_MODE: "1",
      MEMORY_BACKEND: "sqlite",
      POLICY_FILE_PATH: "/tmp/policy.json",
      AUDIT_SINK_TYPE: "file",
      AUDIT_FILE_PATH: "/tmp/audit.jsonl",
    };
    const result = validateEnterpriseMode(env);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      const authViolation = result.error.violations.find((v) => v.control === "auth");
      expect(authViolation).toBeDefined();
      expect(authViolation!.severity).toBe("critical");
    }
  });

  it("accepts startup with all critical controls", () => {
    const result = validateEnterpriseMode(validEnterpriseEnv);
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.enabled).toBe(true);
      expect(result.value.auth.configured).toBe(true);
      expect(result.value.storage.durable).toBe(true);
      expect(result.value.policy.configured).toBe(true);
      expect(result.value.audit.configured).toBe(true);
    }
  });

  it("startup gate throws in enterprise mode with missing controls", () => {
    expect(() => enterpriseStartupGate(undefined, { KIRKFORGE_ENTERPRISE_MODE: "1" })).toThrow();
  });

  it("startup gate returns dev config when enterprise mode is off", () => {
    const config = enterpriseStartupGate(undefined, {});
    expect(config.enabled).toBe(false);
  });

  it("startup gate succeeds with valid enterprise env", () => {
    const config = enterpriseStartupGate(undefined, validEnterpriseEnv);
    expect(config.enabled).toBe(true);
  });
});

describe("Quota + RateLimiter integration", () => {
  it("quota manager enforces limits across tenants", () => {
    const mgr = new QuotaManager();
    mgr.setQuota("tenant-a", { maxConcurrentTasks: 2, maxVerifyRunsPerHour: 100 });
    mgr.setQuota("tenant-b", { maxConcurrentTasks: 10, maxVerifyRunsPerHour: 1000 });

    // tenant-a can run 2 tasks
    expect(mgr.checkQuota("tenant-a", "concurrent_task").ok).toBe(true);
    mgr.recordUsage("tenant-a", { concurrentTasks: 1 });
    expect(mgr.checkQuota("tenant-a", "concurrent_task").ok).toBe(true);
    mgr.recordUsage("tenant-a", { concurrentTasks: 1 });
    expect(mgr.checkQuota("tenant-a", "concurrent_task").ok).toBe(false); // limit hit

    // tenant-b can still run
    expect(mgr.checkQuota("tenant-b", "concurrent_task").ok).toBe(true);
  });

  it("rate limiter enforces per-tenant per-action rates", () => {
    const limiter = new RateLimiter();
    const verifyConfig = { maxRequests: 100, windowMs: 3600000 }; // 100/hour
    const correctConfig = { maxRequests: 50, windowMs: 3600000 }; // 50/hour

    // Tenant can verify up to 100 times
    for (let i = 0; i < 100; i++) {
      expect(limiter.check("tenant-a:verify", verifyConfig).ok).toBe(true);
    }
    // 101st verify is denied
    expect(limiter.check("tenant-a:verify", verifyConfig).ok).toBe(false);

    // But corrections are tracked separately
    expect(limiter.check("tenant-a:correct", correctConfig).ok).toBe(true);

    // And tenant-b is independent
    expect(limiter.check("tenant-b:verify", verifyConfig).ok).toBe(true);
  });

  it("quota persistence round-trips correctly", () => {
    const dir = mkdtempSync(join(tmpdir(), "kirkforge-quota-int-"));
    try {
      const mgr = new QuotaManager();
      mgr.setQuota("tenant-a", { maxConcurrentTasks: 8, maxDailyTokens: 5000000 });
      mgr.recordUsage("tenant-a", { concurrentTasks: 3, dailyTokens: 100000 });

      const persistence = new QuotaPersistence(mgr, {
        filePath: join(dir, "quotas.json"),
        fsyncAfterWrite: false,
        autoSaveIntervalMs: 1000,
      });

      // Save
      const saveResult = persistence.save();
      expect(saveResult.ok).toBe(true);

      // Load into a fresh manager
      const mgr2 = new QuotaManager();
      const persistence2 = new QuotaPersistence(mgr2, {
        filePath: join(dir, "quotas.json"),
      });
      const loadResult = persistence2.load();
      expect(loadResult.ok).toBe(true);

      // Verify data survived
      const quota = mgr2.getQuota("tenant-a");
      expect(quota.maxConcurrentTasks).toBe(8);
      expect(quota.maxDailyTokens).toBe(5000000);

      const usage = mgr2.getUsage("tenant-a");
      expect(usage.concurrentTasks).toBe(3);
      expect(usage.dailyTokens).toBe(100000);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
