import type { MemoryStore } from "@kirkforge/memory-palace";
import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { homedir } from "node:os";

// ---------------------------------------------------------------------------
// SLO Definitions
// ---------------------------------------------------------------------------

export interface SloTarget {
  /** Metric name. */
  name: string;
  /** Target value (0–1 for rate, or absolute count). */
  target: number;
  /** Evaluation window in milliseconds. */
  windowMs: number;
}

export interface SloWindow {
  name: string;
  windowMs: number;
  total: number;
  good: number;
  bad: number;
  /** Current compliance rate (good / max(1, total)). */
  rate: number;
  /** Remaining error budget (total * target - bad) / max(1, total). */
  budgetRemaining: number;
  /** Budget consumed fraction (bad / (total * (1 - target))). */
  budgetConsumed: number;
  burnRate: number;
  status: "ok" | "warning" | "critical";
}

export interface SloReport {
  targets: SloTarget[];
  windows: SloWindow[];
  computedAt: string;
}

// ---------------------------------------------------------------------------
// Default SLOs
// ---------------------------------------------------------------------------

export const DEFAULT_SLO_TARGETS: SloTarget[] = [
  { name: "task_pass_rate", target: 0.95, windowMs: 7 * 24 * 3600 * 1000 }, // 95% over 7d
  { name: "task_pass_rate", target: 0.99, windowMs: 30 * 24 * 3600 * 1000 }, // 99% over 30d
];

// ── Enterprise SLO targets for auth/policy/audit ────────────────────────────
// Auth failure rate: <1% auth failures over 7d and 30d
// Policy deny rate: <5% policy denials over 7d (some denials are expected)
// Audit write failure rate: <0.1% audit write failures over 7d

export const ENTERPRISE_SLO_TARGETS: SloTarget[] = [
  ...DEFAULT_SLO_TARGETS,
  { name: "auth_failure_rate", target: 0.99, windowMs: 7 * 24 * 3600 * 1000 }, // <1% auth failures over 7d
  { name: "auth_failure_rate", target: 0.999, windowMs: 30 * 24 * 3600 * 1000 }, // <0.1% auth failures over 30d
  { name: "policy_deny_rate", target: 0.95, windowMs: 7 * 24 * 3600 * 1000 }, // <5% policy denials over 7d
  { name: "audit_write_rate", target: 0.999, windowMs: 7 * 24 * 3600 * 1000 }, // <0.1% audit write failures over 7d
];

// Evaluation windows for burn-rate alerting
// "Google SRE workbook" style: error budget consumed over short & long windows
const BURN_RATE_WINDOWS = [
  { name: "1h", ms: 1 * 3600 * 1000, critical: 14.4, warning: 10 },
  { name: "6h", ms: 6 * 3600 * 1000, critical: 6.0, warning: 3 },
  { name: "24h", ms: 24 * 3600 * 1000, critical: 3.0, warning: 1 },
];

const MIN_SAMPLES = 3; // require at least 3 observations before reporting

// ---------------------------------------------------------------------------
// SloMonitor
// ---------------------------------------------------------------------------

export class SloMonitor {
  private store: MemoryStore;
  private targets: SloTarget[];

  constructor(store: MemoryStore, targets: SloTarget[] = DEFAULT_SLO_TARGETS) {
    this.store = store;
    this.targets = targets;
  }

  /**
   * Compute full SLO report from stored task observations.
   */
  async compute(): Promise<SloReport> {
    const now = Date.now();
    const windows: SloWindow[] = [];

    for (const target of this.targets) {
      for (const bw of BURN_RATE_WINDOWS) {
        // Only compute burn rate windows that fit within the target window
        if (bw.ms > target.windowMs) continue;

        const window = await this._computeWindow(target, bw.ms, now);
        if (window) windows.push(window);
      }
    }

    return { targets: this.targets, windows, computedAt: new Date().toISOString() };
  }

  private async _computeWindow(
    target: SloTarget,
    windowMs: number,
    now: number,
  ): Promise<SloWindow | null> {
    const since = new Date(now - windowMs).toISOString();
    const result = await this.store.adapter.query({
      kind: "task-observation",
      since,
      limit: 10000,
    });
    if (!result.ok || !result.value) return null;
    const observations = result.value;
    if (observations.length < MIN_SAMPLES) return null;

    let good = 0;
    let bad = 0;
    let _totalSeconds = 0;

    for (const obs of observations) {
      _totalSeconds += Number(obs.properties.durationMs ?? 0) / 1000;
      const outcome = obs.properties.outcome;
      if (outcome === "pass") {
        good++;
      } else {
        bad++;
      }
    }

    const total = good + bad;
    const rate = total > 0 ? good / total : 1;
    const budgetRemaining = target.target - (1 - rate);
    const budgetConsumed =
      target.target > 0 && 1 - target.target > 0
        ? Math.min(1, (1 - rate) / (1 - target.target))
        : 0;

    // Burn rate: how fast we're consuming error budget
    // burnRate = (bad / total) / (1 - target) * (targetWindowMs / windowMs)
    const burnRate =
      1 - target.target > 0 && windowMs > 0
        ? ((1 - rate) / (1 - target.target)) * (target.windowMs / windowMs)
        : 0;

    let status: SloWindow["status"] = "ok";
    const burnWindow = BURN_RATE_WINDOWS.find((w) => w.ms === windowMs);
    if (burnWindow && burnRate >= burnWindow.critical) {
      status = "critical";
    } else if (burnWindow && burnRate >= burnWindow.warning) {
      status = "warning";
    }

    return {
      name: `${target.name}@${this._formatWindow(windowMs)}`,
      windowMs,
      total,
      good,
      bad,
      rate: Math.round(rate * 10000) / 10000,
      budgetRemaining: Math.round(budgetRemaining * 10000) / 10000,
      budgetConsumed: Math.round(budgetConsumed * 10000) / 10000,
      burnRate: Math.round(burnRate * 100) / 100,
      status,
    };
  }

  private _formatWindow(ms: number): string {
    const h = ms / 3600000;
    return h < 24 ? `${Math.round(h)}h` : `${Math.round(h / 24)}d`;
  }

  // ── SLO state persistence ───────────────────────────────────────────────
  //
  // Persist and restore the AuthPolicySloMonitor's in-memory event ring buffer
  // so that auth/policy/audit SLO calculations survive process restarts.
  // The SloMonitor itself derives its data from the MemoryStore, so it doesn't
  // need separate persistence — it recomputes from stored observations.
  // This section provides helpers for AuthPolicySloMonitor persistence.

  /**
   * Persist SLO state to a JSON file.
   * Saves auth/policy/audit events and computed windows for quick restore.
   */
  static persistState(
    filePath: string,
    events: Array<{ timestamp: number; type: string; actorId?: string; tenantId?: string }>,
  ): void {
    const absPath = resolve(filePath);
    const dir = dirname(absPath);
    if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
    writeFileSync(
      absPath,
      JSON.stringify({ version: 1, events, savedAt: new Date().toISOString() }, null, 2),
      "utf-8",
    );
  }

  /**
   * Restore SLO state from a JSON file.
   * Returns the saved events array, or empty array if the file doesn't exist.
   */
  static restoreState(
    filePath: string,
  ): Array<{ timestamp: number; type: string; actorId?: string; tenantId?: string }> {
    const absPath = resolve(filePath);
    if (!existsSync(absPath)) return [];
    try {
      const raw = readFileSync(absPath, "utf-8");
      const data = JSON.parse(raw) as {
        version: number;
        events: Array<{ timestamp: number; type: string; actorId?: string; tenantId?: string }>;
      };
      if (data.version !== 1) return [];
      return data.events ?? [];
    } catch {
      return [];
    }
  }

  /**
   * Default file path for SLO state persistence.
   */
  static defaultStatePath(): string {
    return resolve(homedir(), ".kirkforge", "slo-state.json");
  }
}

// ---------------------------------------------------------------------------
// Auth/Policy/Audit SLO Monitor
// ---------------------------------------------------------------------------
// Tracks counters for auth success/failure, policy deny, and audit write
// success/failure. Computes burn-rate windows the same way as SloMonitor
// but from an in-memory event ring buffer instead of the MemoryStore.

export interface AuthEvent {
  timestamp: number;
  type:
    | "auth.success"
    | "auth.failure"
    | "policy.deny"
    | "policy.allow"
    | "audit.write.success"
    | "audit.write.failure";
  actorId?: string;
  tenantId?: string;
}

export class AuthPolicySloMonitor {
  private events: AuthEvent[] = [];
  private maxSize: number;
  private targets: SloTarget[];

  constructor(targets: SloTarget[] = ENTERPRISE_SLO_TARGETS, maxSize: number = 100_000) {
    this.targets = targets;
    this.maxSize = maxSize;
  }

  /** Record an auth/policy/audit event. */
  record(event: AuthEvent): void {
    this.events.push(event);
    // Ring buffer: drop oldest when over capacity
    if (this.events.length > this.maxSize) {
      this.events = this.events.slice(-this.maxSize);
    }
  }

  /** Compute SLO windows for auth/policy/audit metrics. */
  compute(now?: number): SloReport {
    const timestamp = now ?? Date.now();
    const windows: SloWindow[] = [];

    for (const target of this.targets) {
      for (const bw of BURN_RATE_WINDOWS) {
        if (bw.ms > target.windowMs) continue;

        const windowStart = timestamp - bw.ms;
        const relevant = this.events.filter(
          (e) => e.timestamp >= windowStart && this._eventMatchesTarget(e, target.name),
        );

        if (relevant.length < MIN_SAMPLES) continue;

        let good = 0;
        let bad = 0;
        for (const e of relevant) {
          if (this._isGood(e, target.name)) {
            good++;
          } else {
            bad++;
          }
        }

        const total = good + bad;
        const rate = total > 0 ? good / total : 1;
        const budgetRemaining = target.target - (1 - rate);
        const budgetConsumed =
          target.target > 0 && 1 - target.target > 0
            ? Math.min(1, (1 - rate) / (1 - target.target))
            : 0;
        const burnRate =
          1 - target.target > 0 && bw.ms > 0
            ? ((1 - rate) / (1 - target.target)) * (target.windowMs / bw.ms)
            : 0;

        let status: SloWindow["status"] = "ok";
        if (burnRate >= bw.critical) {
          status = "critical";
        } else if (burnRate >= bw.warning) {
          status = "warning";
        }

        windows.push({
          name: `${target.name}@${this._formatWindow(bw.ms)}`,
          windowMs: bw.ms,
          total,
          good,
          bad,
          rate: Math.round(rate * 10000) / 10000,
          budgetRemaining: Math.round(budgetRemaining * 10000) / 10000,
          budgetConsumed: Math.round(budgetConsumed * 10000) / 10000,
          burnRate: Math.round(burnRate * 100) / 100,
          status,
        });
      }
    }

    return { targets: this.targets, windows, computedAt: new Date(timestamp).toISOString() };
  }

  private _eventMatchesTarget(event: AuthEvent, targetName: string): boolean {
    switch (targetName) {
      case "auth_failure_rate":
        return event.type === "auth.success" || event.type === "auth.failure";
      case "policy_deny_rate":
        return event.type === "policy.allow" || event.type === "policy.deny";
      case "audit_write_rate":
        return event.type === "audit.write.success" || event.type === "audit.write.failure";
      default:
        return false;
    }
  }

  private _isGood(event: AuthEvent, targetName: string): boolean {
    switch (targetName) {
      case "auth_failure_rate":
        return event.type === "auth.success";
      case "policy_deny_rate":
        return event.type === "policy.allow";
      case "audit_write_rate":
        return event.type === "audit.write.success";
      default:
        return true;
    }
  }

  private _formatWindow(ms: number): string {
    const h = ms / 3600000;
    return h < 24 ? `${Math.round(h)}h` : `${Math.round(h / 24)}d`;
  }

  // ── SLO state persistence ───────────────────────────────────────────────
  //
  // Persist and restore the AuthPolicySloMonitor's in-memory event ring buffer
  // so that auth/policy/audit SLO calculations survive process restarts.
  // The SloMonitor itself derives its data from the MemoryStore, so it doesn't
  // need separate persistence — it recomputes from stored observations.
  // This section provides helpers for AuthPolicySloMonitor persistence.

  /**
   * Persist SLO state to a JSON file.
   * Saves auth/policy/audit events and computed windows for quick restore.
   */
  static persistState(
    filePath: string,
    events: Array<{ timestamp: number; type: string; actorId?: string; tenantId?: string }>,
  ): void {
    const absPath = resolve(filePath);
    const dir = dirname(absPath);
    if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
    writeFileSync(
      absPath,
      JSON.stringify({ version: 1, events, savedAt: new Date().toISOString() }, null, 2),
      "utf-8",
    );
  }

  /**
   * Restore SLO state from a JSON file.
   * Returns the saved events array, or empty array if the file doesn't exist.
   */
  static restoreState(
    filePath: string,
  ): Array<{ timestamp: number; type: string; actorId?: string; tenantId?: string }> {
    const absPath = resolve(filePath);
    if (!existsSync(absPath)) return [];
    try {
      const raw = readFileSync(absPath, "utf-8");
      const data = JSON.parse(raw) as {
        version: number;
        events: Array<{ timestamp: number; type: string; actorId?: string; tenantId?: string }>;
      };
      if (data.version !== 1) return [];
      return data.events ?? [];
    } catch {
      return [];
    }
  }

  /**
   * Default file path for SLO state persistence.
   */
  static defaultStatePath(): string {
    return resolve(homedir(), ".kirkforge", "slo-state.json");
  }
}
