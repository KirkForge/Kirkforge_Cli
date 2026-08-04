import { describe, it, expect } from "vitest";
import { QuotaManager, RateLimiter, DEFAULT_QUOTA, type RateLimitConfig } from "../src/quotas.js";

describe("QuotaManager", () => {
  it("uses default quotas when none are set", () => {
    const mgr = new QuotaManager();
    const quota = mgr.getQuota("unknown-tenant");
    expect(quota.maxConcurrentTasks).toBe(4);
    expect(quota.maxDailyTokens).toBe(1000000);
    expect(quota.maxObservations).toBe(10000);
  });

  it("allows setting per-tenant quotas", () => {
    const mgr = new QuotaManager();
    mgr.setQuota("premium", { maxConcurrentTasks: 16, maxDailyTokens: 5000000 });
    const quota = mgr.getQuota("premium");
    expect(quota.maxConcurrentTasks).toBe(16);
    expect(quota.maxDailyTokens).toBe(5000000);
    // Unset values keep defaults
    expect(quota.maxObservations).toBe(10000);
  });

  it("defaults are overridden by constructor", () => {
    const mgr = new QuotaManager({ maxConcurrentTasks: 2 });
    const quota = mgr.getQuota("any");
    expect(quota.maxConcurrentTasks).toBe(2);
    expect(quota.maxDailyTokens).toBe(1000000);
  });

  it("checkQuota allows actions within limits", () => {
    const mgr = new QuotaManager();
    mgr.setQuota("t1", { maxConcurrentTasks: 4 });
    mgr.recordUsage("t1", { concurrentTasks: 2 });
    const result = mgr.checkQuota("t1", "concurrent_task");
    expect(result.ok).toBe(true);
  });

  it("checkQuota denies actions exceeding limits", () => {
    const mgr = new QuotaManager();
    mgr.setQuota("t1", { maxConcurrentTasks: 2 });
    mgr.recordUsage("t1", { concurrentTasks: 2 });
    const result = mgr.checkQuota("t1", "concurrent_task");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.quota).toBe("maxConcurrentTasks");
      expect(result.error.limit).toBe(2);
      expect(result.error.current).toBe(2);
    }
  });

  it("records usage incrementally", () => {
    const mgr = new QuotaManager();
    mgr.recordUsage("t1", { dailyTokens: 100 });
    mgr.recordUsage("t1", { dailyTokens: 200 });
    const usage = mgr.getUsage("t1");
    expect(usage.dailyTokens).toBe(300);
  });

  it("resets hourly counters", () => {
    const mgr = new QuotaManager();
    mgr.recordUsage("t1", { hourlyToolInvocations: 100, hourlyVerifyRuns: 50 });
    mgr.resetHourly("t1");
    const usage = mgr.getUsage("t1");
    expect(usage.hourlyToolInvocations).toBe(0);
    expect(usage.hourlyVerifyRuns).toBe(0);
    // Daily and storage counters are preserved
    expect(usage.dailyTokens).toBe(0);
  });

  it("resets daily counters", () => {
    const mgr = new QuotaManager();
    mgr.recordUsage("t1", { dailyTokens: 50000 });
    mgr.resetDaily("t1");
    const usage = mgr.getUsage("t1");
    expect(usage.dailyTokens).toBe(0);
  });

  it("removes tenant data completely", () => {
    const mgr = new QuotaManager();
    mgr.setQuota("t1", { maxConcurrentTasks: 8 });
    mgr.recordUsage("t1", { concurrentTasks: 3 });
    mgr.removeTenant("t1");
    // Falls back to default quota
    const quota = mgr.getQuota("t1");
    expect(quota.maxConcurrentTasks).toBe(DEFAULT_QUOTA.maxConcurrentTasks);
    // Usage resets to zero
    const usage = mgr.getUsage("t1");
    expect(usage.concurrentTasks).toBe(0);
  });

  it("isolates tenants from each other", () => {
    const mgr = new QuotaManager();
    mgr.setQuota("alpha", { maxConcurrentTasks: 2 });
    mgr.setQuota("beta", { maxConcurrentTasks: 10 });
    mgr.recordUsage("alpha", { concurrentTasks: 2 }); // alpha is full
    const alphaResult = mgr.checkQuota("alpha", "concurrent_task");
    const betaResult = mgr.checkQuota("beta", "concurrent_task");
    expect(alphaResult.ok).toBe(false); // alpha at limit
    expect(betaResult.ok).toBe(true); // beta has room
  });
});

describe("RateLimiter", () => {
  const config: RateLimitConfig = { maxRequests: 5, windowMs: 60000 };

  it("allows requests within limit", () => {
    const limiter = new RateLimiter();
    for (let i = 0; i < 5; i++) {
      const result = limiter.check("tenant:verify", config);
      expect(result.ok).toBe(true);
    }
  });

  it("denies requests exceeding limit", () => {
    const limiter = new RateLimiter();
    for (let i = 0; i < 5; i++) {
      limiter.check("tenant:verify", config);
    }
    const result = limiter.check("tenant:verify", config);
    expect(result.ok).toBe(false);
  });

  it("different keys have independent limits", () => {
    const limiter = new RateLimiter();
    for (let i = 0; i < 5; i++) {
      limiter.check("tenant:a:verify", config);
    }
    const bResult = limiter.check("tenant:b:verify", config);
    expect(bResult.ok).toBe(true);
  });

  it("resets a specific key", () => {
    const limiter = new RateLimiter();
    for (let i = 0; i < 5; i++) {
      limiter.check("tenant:verify", config);
    }
    limiter.reset("tenant:verify");
    const result = limiter.check("tenant:verify", config);
    expect(result.ok).toBe(true);
  });

  it("sliding window expires old entries", () => {
    const limiter = new RateLimiter();
    const now = Date.now();
    const shortConfig: RateLimitConfig = { maxRequests: 2, windowMs: 100 };
    // Two requests now
    limiter.check("tenant:verify", shortConfig, now);
    limiter.check("tenant:verify", shortConfig, now);
    // Third should fail
    expect(limiter.check("tenant:verify", shortConfig, now).ok).toBe(false);
    // After window expires, should succeed
    expect(limiter.check("tenant:verify", shortConfig, now + 150).ok).toBe(true);
  });

  it("reports current count accurately", () => {
    const limiter = new RateLimiter();
    const config: RateLimitConfig = { maxRequests: 10, windowMs: 60000 };
    for (let i = 0; i < 3; i++) {
      limiter.check("tenant:verify", config);
    }
    expect(limiter.getCurrentCount("tenant:verify", 60000)).toBe(3);
  });
});

describe("QuotaManager auto-reset", () => {
  it("auto-resets hourly counters when hour boundary passes", () => {
    const mgr = new QuotaManager();
    const originalDateNow = Date.now;

    // Start at a known time
    const baseTime = 1700000000000; // some known timestamp
    Date.now = () => baseTime;

    // Record some hourly usage
    mgr.recordUsage("t1", {
      hourlyToolInvocations: 50,
      hourlyVerifyRuns: 30,
      hourlyCorrections: 10,
    });
    const usage0 = mgr.getUsage("t1");
    expect(usage0.hourlyToolInvocations).toBe(50);
    expect(usage0.hourlyVerifyRuns).toBe(30);
    expect(usage0.hourlyCorrections).toBe(10);

    // Advance past the hour boundary (1 hour + 1 ms)
    Date.now = () => baseTime + 3600_001;

    // getUsage should trigger auto-reset, clearing hourly counters
    const usage1 = mgr.getUsage("t1");
    expect(usage1.hourlyToolInvocations).toBe(0);
    expect(usage1.hourlyVerifyRuns).toBe(0);
    expect(usage1.hourlyCorrections).toBe(0);

    Date.now = originalDateNow;
  });

  it("auto-resets daily counters when day boundary passes", () => {
    const mgr = new QuotaManager();
    const originalDateNow = Date.now;

    const baseTime = 1700000000000;
    Date.now = () => baseTime;

    mgr.recordUsage("t1", { dailyTokens: 500000 });
    const usage0 = mgr.getUsage("t1");
    expect(usage0.dailyTokens).toBe(500000);

    // Advance past the day boundary (24 hours + 1 ms)
    Date.now = () => baseTime + 86_400_001;

    const usage1 = mgr.getUsage("t1");
    expect(usage1.dailyTokens).toBe(0);

    Date.now = originalDateNow;
  });

  it("checkQuota also triggers auto-reset", () => {
    const mgr = new QuotaManager();
    const originalDateNow = Date.now;

    const baseTime = 1700000000000;
    Date.now = () => baseTime;

    mgr.setQuota("t1", { maxToolInvocationsPerHour: 10 });
    mgr.recordUsage("t1", { hourlyToolInvocations: 10 });
    // At the current time, should be blocked
    expect(mgr.checkQuota("t1", "tool_invocation").ok).toBe(false);

    // Advance past the hour boundary — auto-reset should clear hourly counters
    Date.now = () => baseTime + 3600_001;
    expect(mgr.checkQuota("t1", "tool_invocation").ok).toBe(true);

    Date.now = originalDateNow;
  });

  it("does not reset if the boundary has not passed", () => {
    const mgr = new QuotaManager();
    const originalDateNow = Date.now;

    const baseTime = 1700000000000;
    Date.now = () => baseTime;

    mgr.recordUsage("t1", { hourlyToolInvocations: 100 });
    // Same hour — no reset
    Date.now = () => baseTime + 1800_000; // 30 min later, same hour
    const usage = mgr.getUsage("t1");
    expect(usage.hourlyToolInvocations).toBe(100);

    Date.now = originalDateNow;
  });
});

describe("RateLimiter memory cleanup", () => {
  it("removes stale keys via cleanup when buckets are old", () => {
    const limiter = new RateLimiter();
    const config: RateLimitConfig = { maxRequests: 5, windowMs: 60000 };
    const twoHoursAgo = Date.now() - 2 * 3600_000;

    // Create a key with an old bucket
    limiter.check("old-key", config, twoHoursAgo);

    // Force cleanup by resetting lastCleanup and triggering a check
    (limiter as any).lastCleanup = 0;
    limiter.check("new-key", config); // triggers periodic cleanup

    // old-key should be gone — its bucket is older than 1 hour
    expect(limiter.getCurrentCount("old-key", 60000)).toBe(0);
  });

  it("preserves keys with recent buckets during cleanup", () => {
    const limiter = new RateLimiter();
    const config: RateLimitConfig = { maxRequests: 5, windowMs: 60000 };

    limiter.check("recent-key", config);

    // Force cleanup by resetting lastCleanup and triggering a check
    (limiter as any).lastCleanup = 0;
    limiter.check("trigger", config); // triggers periodic cleanup

    // recent-key should survive — its bucket is within the last hour
    expect(limiter.getCurrentCount("recent-key", 60000)).toBe(1);
  });

  it("deletes empty key entries on check when buckets expire", () => {
    const limiter = new RateLimiter();
    const shortConfig: RateLimitConfig = { maxRequests: 5, windowMs: 100 };

    limiter.check("key-expired", shortConfig, 1000);
    // Advance well past the window — old buckets are filtered and key is deleted
    const result = limiter.check("key-expired", shortConfig, 5000);
    // The old bucket was filtered out, request is allowed again
    expect(result.ok).toBe(true);
  });
});
