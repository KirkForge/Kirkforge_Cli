import { ok, err, type Result } from "@kirkforge/core-types";
import { KirkForgeError } from "@kirkforge/core-errors";

// ── Per-tenant quotas and rate limits ──────────────────────────────────────
//
// Enterprise deployments need per-tenant quotas to prevent noisy-neighbor
// effects and enforce fair resource usage. This module provides:
//   - TenantQuota: configurable resource limits per tenant
//   - QuotaManager: tracks usage and enforces limits
//   - RateLimiter: sliding-window rate limiting per tenant per action

// ── Quota types ────────────────────────────────────────────────────────────

export interface TenantQuota {
  /** Maximum concurrent verification tasks. Default: 4. */
  maxConcurrentTasks?: number;
  /** Maximum total storage in MB for tenant data. Default: 1024. */
  maxStorageMb?: number;
  /** Maximum number of observations stored. Default: 10000. */
  maxObservations?: number;
  /** Maximum token budget per day across all model calls. Default: 1000000. */
  maxDailyTokens?: number;
  /** Maximum tool invocations per hour. Default: 1000. */
  maxToolInvocationsPerHour?: number;
  /** Maximum verification runs per hour. Default: 500. */
  maxVerifyRunsPerHour?: number;
  /** Maximum correction prompts per hour. Default: 200. */
  maxCorrectionsPerHour?: number;
}

export const DEFAULT_QUOTA: Required<TenantQuota> = {
  maxConcurrentTasks: 4,
  maxStorageMb: 1024,
  maxObservations: 10000,
  maxDailyTokens: 1000000,
  maxToolInvocationsPerHour: 1000,
  maxVerifyRunsPerHour: 500,
  maxCorrectionsPerHour: 200,
};

export interface QuotaUsage {
  /** Current number of concurrent tasks. */
  concurrentTasks: number;
  /** Current storage usage in MB. */
  storageMb: number;
  /** Current number of observations. */
  observationCount: number;
  /** Tokens used today. */
  dailyTokens: number;
  /** Tool invocations in the current hour window. */
  hourlyToolInvocations: number;
  /** Verification runs in the current hour window. */
  hourlyVerifyRuns: number;
  /** Correction prompts in the current hour window. */
  hourlyCorrections: number;
}

export class QuotaExceededError extends KirkForgeError {
  readonly quota: string;
  readonly limit: number;
  readonly current: number;

  constructor(quota: string, limit: number, current: number) {
    super(
      "QUOTA_EXCEEDED",
      `Tenant quota exceeded: ${quota} (limit=${limit}, current=${current})`,
      { quota, limit, current },
    );
    this.name = "QuotaExceededError";
    this.quota = quota;
    this.limit = limit;
    this.current = current;
  }
}

// ── Quota manager ───────────────────────────────────────────────────────────

export class QuotaManager {
  private quotas = new Map<string, Required<TenantQuota>>();
  private usage = new Map<string, QuotaUsage>();
  private defaultQuota: Required<TenantQuota>;

  // Track the last reset boundary per tenant so counters auto-reset
  private lastHourBoundary = new Map<string, number>();
  private lastDayBoundary = new Map<string, number>();

  constructor(defaultQuota?: TenantQuota) {
    this.defaultQuota = { ...DEFAULT_QUOTA, ...defaultQuota };
  }

  /** Set quota for a specific tenant. */
  setQuota(tenantId: string, quota: TenantQuota): void {
    this.quotas.set(tenantId, { ...DEFAULT_QUOTA, ...quota });
  }

  /** Get quota for a tenant (returns default if not explicitly set). */
  getQuota(tenantId: string): Required<TenantQuota> {
    return this.quotas.get(tenantId) ?? { ...this.defaultQuota };
  }

  /** Get current usage for a tenant (returns zeros if not tracked). */
  getUsage(tenantId: string): QuotaUsage {
    this._autoResetIfStale(tenantId);
    return (
      this.usage.get(tenantId) ?? {
        concurrentTasks: 0,
        storageMb: 0,
        observationCount: 0,
        dailyTokens: 0,
        hourlyToolInvocations: 0,
        hourlyVerifyRuns: 0,
        hourlyCorrections: 0,
      }
    );
  }

  /** Check whether a tenant can perform an action without exceeding quota. */
  checkQuota(tenantId: string, action: QuotaAction): Result<void, QuotaExceededError> {
    this._autoResetIfStale(tenantId);
    const quota = this.getQuota(tenantId);
    const usage = this.usage.get(tenantId) ?? {
      concurrentTasks: 0,
      storageMb: 0,
      observationCount: 0,
      dailyTokens: 0,
      hourlyToolInvocations: 0,
      hourlyVerifyRuns: 0,
      hourlyCorrections: 0,
    };

    switch (action) {
      case "concurrent_task":
        if (usage.concurrentTasks >= quota.maxConcurrentTasks) {
          return err(
            new QuotaExceededError(
              "maxConcurrentTasks",
              quota.maxConcurrentTasks,
              usage.concurrentTasks,
            ),
          );
        }
        break;
      case "storage":
        if (usage.storageMb >= quota.maxStorageMb) {
          return err(new QuotaExceededError("maxStorageMb", quota.maxStorageMb, usage.storageMb));
        }
        break;
      case "observation":
        if (usage.observationCount >= quota.maxObservations) {
          return err(
            new QuotaExceededError(
              "maxObservations",
              quota.maxObservations,
              usage.observationCount,
            ),
          );
        }
        break;
      case "token_spend":
        if (usage.dailyTokens >= quota.maxDailyTokens) {
          return err(
            new QuotaExceededError("maxDailyTokens", quota.maxDailyTokens, usage.dailyTokens),
          );
        }
        break;
      case "tool_invocation":
        if (usage.hourlyToolInvocations >= quota.maxToolInvocationsPerHour) {
          return err(
            new QuotaExceededError(
              "maxToolInvocationsPerHour",
              quota.maxToolInvocationsPerHour,
              usage.hourlyToolInvocations,
            ),
          );
        }
        break;
      case "verify_run":
        if (usage.hourlyVerifyRuns >= quota.maxVerifyRunsPerHour) {
          return err(
            new QuotaExceededError(
              "maxVerifyRunsPerHour",
              quota.maxVerifyRunsPerHour,
              usage.hourlyVerifyRuns,
            ),
          );
        }
        break;
      case "correction":
        if (usage.hourlyCorrections >= quota.maxCorrectionsPerHour) {
          return err(
            new QuotaExceededError(
              "maxCorrectionsPerHour",
              quota.maxCorrectionsPerHour,
              usage.hourlyCorrections,
            ),
          );
        }
        break;
    }
    return ok(undefined);
  }

  /** Record that a tenant has used a resource. */
  recordUsage(tenantId: string, delta: Partial<QuotaUsage>): void {
    const current = this.getUsage(tenantId);
    this.usage.set(tenantId, {
      concurrentTasks: current.concurrentTasks + (delta.concurrentTasks ?? 0),
      storageMb: current.storageMb + (delta.storageMb ?? 0),
      observationCount: current.observationCount + (delta.observationCount ?? 0),
      dailyTokens: current.dailyTokens + (delta.dailyTokens ?? 0),
      hourlyToolInvocations: current.hourlyToolInvocations + (delta.hourlyToolInvocations ?? 0),
      hourlyVerifyRuns: current.hourlyVerifyRuns + (delta.hourlyVerifyRuns ?? 0),
      hourlyCorrections: current.hourlyCorrections + (delta.hourlyCorrections ?? 0),
    });
  }

  /** Reset hourly counters for a tenant (called at hour boundaries). */
  resetHourly(tenantId: string): void {
    const current = this.usage.get(tenantId);
    if (!current) return;
    this.usage.set(tenantId, {
      ...current,
      hourlyToolInvocations: 0,
      hourlyVerifyRuns: 0,
      hourlyCorrections: 0,
    });
  }

  /** Reset daily counters for a tenant (called at day boundaries). */
  resetDaily(tenantId: string): void {
    const current = this.usage.get(tenantId);
    if (!current) return;
    this.usage.set(tenantId, {
      ...current,
      dailyTokens: 0,
    });
  }

  /** Remove a tenant's quota and usage data. */
  removeTenant(tenantId: string): void {
    this.quotas.delete(tenantId);
    this.usage.delete(tenantId);
    this.lastHourBoundary.delete(tenantId);
    this.lastDayBoundary.delete(tenantId);
  }

  /** List all tenant IDs that have quota overrides or usage data. */
  listTenantIds(): string[] {
    return [...new Set([...this.quotas.keys(), ...this.usage.keys()])];
  }

  // ── Auto-reset helpers ──────────────────────────────────────────────────
  //
  // Track hour/day boundaries per tenant. When a boundary has passed since
  // the last reset, automatically reset the relevant counters. This prevents
  // the quota lockout bug where counters accumulate forever because
  // resetHourly()/resetDaily() are never called.

  private _autoResetIfStale(tenantId: string): void {
    const now = Date.now();
    const currentHour = Math.floor(now / 3600_000);
    const currentDay = Math.floor(now / 86_400_000);

    const lastHour = this.lastHourBoundary.get(tenantId);
    const lastDay = this.lastDayBoundary.get(tenantId);

    // Initialize boundaries on first use
    if (lastHour === undefined) {
      this.lastHourBoundary.set(tenantId, currentHour);
    } else if (currentHour > lastHour) {
      // Hour boundary has passed — reset hourly counters
      this.resetHourly(tenantId);
      this.lastHourBoundary.set(tenantId, currentHour);
    }

    if (lastDay === undefined) {
      this.lastDayBoundary.set(tenantId, currentDay);
    } else if (currentDay > lastDay) {
      // Day boundary has passed — reset daily counters
      this.resetDaily(tenantId);
      this.lastDayBoundary.set(tenantId, currentDay);
    }
  }
}

// ── Quota actions ──────────────────────────────────────────────────────────

export type QuotaAction =
  | "concurrent_task"
  | "storage"
  | "observation"
  | "token_spend"
  | "tool_invocation"
  | "verify_run"
  | "correction";

// ── Sliding window rate limiter ────────────────────────────────────────────

export interface RateLimitConfig {
  /** Maximum number of requests in the window. */
  maxRequests: number;
  /** Window duration in milliseconds. */
  windowMs: number;
}

interface TimestampBucket {
  timestamp: number;
  count: number;
}

export class RateLimiter {
  private windows = new Map<string, TimestampBucket[]>();
  private lastCleanup = 0;
  private static CLEANUP_INTERVAL_MS = 60_000; // 1 minute

  /**
   * Check if a request is allowed under the rate limit.
   * Returns ok if allowed, err if rate limit exceeded.
   */
  check(key: string, config: RateLimitConfig, nowMs?: number): Result<void, QuotaExceededError> {
    const now = nowMs ?? Date.now();
    const windowStart = now - config.windowMs;
    let buckets = this.windows.get(key) ?? [];

    // Remove expired buckets
    buckets = buckets.filter((b) => b.timestamp > windowStart);

    // Update stored buckets (prunes expired entries for this key)
    if (buckets.length === 0) {
      this.windows.delete(key);
    } else {
      this.windows.set(key, buckets);
    }

    const currentCount = buckets.reduce((sum, b) => sum + b.count, 0);
    if (currentCount >= config.maxRequests) {
      return err(new QuotaExceededError(`rate:${key}`, config.maxRequests, currentCount));
    }

    // Record this request
    buckets.push({ timestamp: now, count: 1 });
    this.windows.set(key, buckets);

    // Periodic cleanup of abandoned keys to prevent memory leak
    if (now - this.lastCleanup > RateLimiter.CLEANUP_INTERVAL_MS) {
      this._cleanupStaleKeys(now);
      this.lastCleanup = now;
    }

    return ok(undefined);
  }

  /** Get current request count for a key within the window. */
  getCurrentCount(key: string, windowMs: number, nowMs?: number): number {
    const now = nowMs ?? Date.now();
    const buckets = this.windows.get(key) ?? [];
    return buckets.filter((b) => b.timestamp > now - windowMs).reduce((sum, b) => sum + b.count, 0);
  }

  /** Reset rate limit for a specific key. */
  reset(key: string): void {
    this.windows.delete(key);
  }

  /**
   * Manually trigger cleanup of stale keys.
   * Removes keys where all buckets are older than 1 hour.
   */
  cleanup(): void {
    this._cleanupStaleKeys(Date.now());
  }

  private _cleanupStaleKeys(now: number): void {
    for (const [key, buckets] of this.windows) {
      // Remove keys where all buckets are older than 1 hour.
      // Most rate limit windows are seconds to minutes, so 1 hour is a
      // conservative threshold that safely covers all typical windows.
      const recent = buckets.filter((b) => b.timestamp > now - 3_600_000);
      if (recent.length === 0) {
        this.windows.delete(key);
      } else if (recent.length < buckets.length) {
        this.windows.set(key, recent);
      }
    }
  }
}
