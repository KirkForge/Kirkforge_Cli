import { ok, err, type Result } from "@kirkforge/core-types";
import { KirkForgeError } from "@kirkforge/core-errors";
import { QuotaManager, type TenantQuota, type QuotaUsage } from "./quotas.js";
import { mkdirSync, existsSync, readFileSync, writeFileSync, renameSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { createHash } from "node:crypto";

// ── Quota persistence ──────────────────────────────────────────────────────
//
// Persists quota configuration and usage to a JSON file for cross-process
// durability. In enterprise deployments with SQLite available, this can be
// replaced with a database-backed adapter.
//
// The persistence layer:
//   1. Saves quota overrides per tenant
//   2. Saves usage counters per tenant
//   3. Uses atomic write (write-to-temp + rename) to prevent corruption
//   4. Loads state on construction for crash recovery
//
// For production multi-process deployments, use a Redis or SQL backend
// instead of this file-based approach.

export interface QuotaPersistenceConfig {
  /** Path to the JSON file for persistence. */
  filePath: string;
  /** Whether to fsync after write. Default: true. */
  fsyncAfterWrite?: boolean;
  /** Auto-save interval in milliseconds. Default: 30000 (30 seconds). */
  autoSaveIntervalMs?: number;
}

interface PersistedQuotaState {
  /** Schema version for future migrations. */
  version: number;
  /** Per-tenant quota overrides. */
  quotas: Record<string, TenantQuota>;
  /** Per-tenant usage counters. */
  usage: Record<string, QuotaUsage>;
  /** ISO timestamp of last save. */
  lastSaved: string;
  /** SHA-256 hash of the content for integrity. */
  contentHash: string;
}

export class QuotaPersistenceError extends KirkForgeError {
  constructor(message: string, cause?: string) {
    super("QUOTA_PERSISTENCE_ERROR", message, { cause });
    this.name = "QuotaPersistenceError";
  }
}

export class QuotaPersistence {
  private config: Required<QuotaPersistenceConfig>;
  private quotaManager: QuotaManager;
  private autoSaveTimer: ReturnType<typeof setInterval> | null = null;
  private dirty = false;

  constructor(quotaManager: QuotaManager, config: QuotaPersistenceConfig) {
    this.quotaManager = quotaManager;
    this.config = {
      filePath: resolve(config.filePath),
      fsyncAfterWrite: config.fsyncAfterWrite ?? true,
      autoSaveIntervalMs: config.autoSaveIntervalMs ?? 30_000,
    };
  }

  /**
   * Load persisted state from disk and populate the QuotaManager.
   * Call this once at startup before starting auto-save.
   */
  load(): Result<void, QuotaPersistenceError> {
    try {
      if (!existsSync(this.config.filePath)) {
        return ok(undefined);
      }

      const raw = readFileSync(this.config.filePath, "utf-8");
      const state: PersistedQuotaState = JSON.parse(raw);

      if (state.version !== 1) {
        return err(
          new QuotaPersistenceError(`Unsupported quota persistence version: ${state.version}`),
        );
      }

      // Verify content hash
      const { contentHash, ...content } = state;
      const expectedHash = computeHash(JSON.stringify(content));
      if (contentHash !== expectedHash) {
        return err(
          new QuotaPersistenceError(
            "Quota persistence file integrity check failed. Content may be corrupted.",
          ),
        );
      }

      // Restore quotas
      for (const [tenantId, quota] of Object.entries(state.quotas)) {
        this.quotaManager.setQuota(tenantId, quota);
      }

      // Restore usage
      for (const [tenantId, usage] of Object.entries(state.usage)) {
        this.quotaManager.recordUsage(tenantId, usage);
      }

      this.dirty = false;
      return ok(undefined);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      return err(new QuotaPersistenceError(`Failed to load quota state: ${message}`, message));
    }
  }

  /**
   * Save current state to disk. Uses atomic write (temp file + rename).
   */
  save(): Result<void, QuotaPersistenceError> {
    try {
      // Collect all known tenant IDs from the quota manager
      const allTenantIds = this.quotaManager.listTenantIds();

      const quotas: Record<string, TenantQuota> = {};
      const usage: Record<string, QuotaUsage> = {};

      for (const tenantId of allTenantIds) {
        quotas[tenantId] = this.quotaManager.getQuota(tenantId);
        usage[tenantId] = this.quotaManager.getUsage(tenantId);
      }

      const state: Omit<PersistedQuotaState, "contentHash"> & { contentHash: string } = {
        version: 1,
        quotas,
        usage,
        lastSaved: new Date().toISOString(),
        contentHash: "", // computed below
      };

      // Compute hash and write
      const { contentHash: _, ...content } = state;
      const hash = computeHash(JSON.stringify(content));
      state.contentHash = hash;

      // Ensure directory exists
      const dir = dirname(this.config.filePath);
      if (!existsSync(dir)) {
        mkdirSync(dir, { recursive: true });
      }

      // Atomic write: write to temp file, then rename
      const tempPath = this.config.filePath + ".tmp";
      writeFileSync(tempPath, JSON.stringify(state, null, 2), "utf-8");

      if (this.config.fsyncAfterWrite) {
        // Best-effort fsync — not critical if it fails
        try {
          const fd = __import_internal_fs_openSync(tempPath, "r");
          __import_internal_fs_fsyncSync(fd);
          __import_internal_fs_closeSync(fd);
        } catch {
          // Best-effort
        }
      }

      renameSync(tempPath, this.config.filePath);
      this.dirty = false;
      return ok(undefined);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      return err(new QuotaPersistenceError(`Failed to save quota state: ${message}`, message));
    }
  }

  /**
   * Start auto-save at the configured interval.
   */
  startAutoSave(): void {
    if (this.autoSaveTimer) return;
    this.autoSaveTimer = setInterval(() => {
      if (this.dirty) {
        this.save();
      }
    }, this.config.autoSaveIntervalMs);
  }

  /**
   * Stop auto-save and perform a final save.
   */
  async stopAutoSave(): Promise<void> {
    if (this.autoSaveTimer) {
      clearInterval(this.autoSaveTimer);
      this.autoSaveTimer = null;
    }
    this.save();
  }

  /**
   * Mark state as dirty so it will be saved on next auto-save cycle.
   */
  markDirty(): void {
    this.dirty = true;
  }
}

// Use internal fs imports for the atomic write
import {
  openSync as __import_internal_fs_openSync,
  fsyncSync as __import_internal_fs_fsyncSync,
  closeSync as __import_internal_fs_closeSync,
} from "node:fs";

function computeHash(content: string): string {
  return createHash("sha256").update(content, "utf-8").digest("hex").slice(0, 24);
}
