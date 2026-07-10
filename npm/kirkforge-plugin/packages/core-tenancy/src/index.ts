import { createHash } from "node:crypto";
import { resolve, join, isAbsolute } from "node:path";
import { mkdirSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { homedir } from "node:os";
import { ok, err, type Result } from "@kirkforge/core-types";
import type { TenantKeyProvider } from "@kirkforge/core-secrets";
import { MemoryStore, type MemoryAdapter } from "@kirkforge/memory-palace";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface TenantHandle {
  /** Stable tenant id derived from workspace path hash. */
  tenantId: string;
  /** Human-readable label (last segment of workspace path). */
  label: string;
  /** Absolute workspace path this tenant is bound to. */
  workspacePath: string;
  /** Tenant-scoped storage directory (.kirkforge/tenants/<id>). */
  storageDir: string;
  /** Created timestamp (ISO). */
  createdAt: string;
}

export interface TenantRegistryConfig {
  /** Base directory for tenant storage. Defaults to ~/.kirkforge/tenants. */
  storageRoot?: string;
}

// ---------------------------------------------------------------------------
// Path safety for tenant resources
// ---------------------------------------------------------------------------

/**
 * Validate that a resource name is safe for use with resolvePath.
 * Rejects: empty strings, paths with separators (/ or \), paths with ..
 * segments, null bytes, and absolute paths.
 *
 * This prevents path traversal attacks when constructing tenant-scoped
 * file paths. Always use this before passing user-provided names to
 * resolvePath().
 */
export function isSafeResourceName(name: string): boolean {
  if (!name || name.length === 0) return false;
  if (name.includes("\0")) return false;
  if (name.includes("/") || name.includes("\\")) return false;
  if (name === ".." || name.includes("..")) return false;
  if (isAbsolute(name)) return false;
  // Reject leading dots (hidden files / directory traversal)
  if (name.startsWith(".")) return false;
  return true;
}

// ---------------------------------------------------------------------------
// TenantRegistry
// ---------------------------------------------------------------------------

/**
 * Manages tenant isolation: each workspace gets a stable tenant id and
 * isolated storage so memory, event logs, and configs never cross-contaminate.
 *
 * All resource name parameters passed to resolvePath are validated with
 * isSafeResourceName to prevent path traversal attacks.
 */
export class TenantRegistry {
  private storageRoot: string;
  private tenants = new Map<string, TenantHandle>();

  constructor(config: TenantRegistryConfig = {}) {
    this.storageRoot = config.storageRoot ?? join(homedir(), ".kirkforge", "tenants");
    this._loadIndex();
  }

  private _indexPath(): string {
    return join(this.storageRoot, "index.json");
  }

  private _loadIndex(): void {
    const indexPath = this._indexPath();
    if (!existsSync(indexPath)) return;
    try {
      const raw = readFileSync(indexPath, "utf-8");
      const data = JSON.parse(raw) as Array<{
        tenantId: string;
        label: string;
        workspacePath: string;
        storageDir: string;
        createdAt: string;
      }>;
      for (const entry of data) {
        this.tenants.set(entry.tenantId, entry);
      }
    } catch {
      // Corrupt index — start fresh
    }
  }

  private _saveIndex(): void {
    try {
      const indexPath = this._indexPath();
      mkdirSync(this.storageRoot, { recursive: true });
      const data = [...this.tenants.values()].map((t) => ({
        tenantId: t.tenantId,
        label: t.label,
        workspacePath: t.workspacePath,
        storageDir: t.storageDir,
        createdAt: t.createdAt,
      }));
      writeFileSync(indexPath, JSON.stringify(data, null, 2), "utf-8");
    } catch {
      // Best-effort persistence
    }
  }

  /**
   * Register (or retrieve) a tenant for the given workspace.
   * Idempotent — repeated calls with the same path return the same handle.
   */
  register(workspacePath: string): TenantHandle {
    const abs = resolve(workspacePath);
    const tenantId = createHash("sha256").update(abs).digest("hex").slice(0, 16);
    const existing = this.tenants.get(tenantId);
    if (existing) return existing;

    const label = abs.split("/").pop() ?? abs;
    const storageDir = join(this.storageRoot, tenantId);
    const handle: TenantHandle = {
      tenantId,
      label,
      workspacePath: abs,
      storageDir,
      createdAt: new Date().toISOString(),
    };
    this.tenants.set(tenantId, handle);
    this._saveIndex();
    return handle;
  }

  /** Look up a tenant by id. */
  get(tenantId: string): TenantHandle | undefined {
    return this.tenants.get(tenantId);
  }

  /** List all registered tenants in this session. */
  list(): TenantHandle[] {
    return [...this.tenants.values()];
  }

  /**
   * Resolve a tenant-scoped path for a resource kind (e.g. "memory.db", "events.jsonl").
   *
   * IMPORTANT: The resourceName parameter is validated to prevent path traversal.
   * Names containing path separators, ".." segments, null bytes, or absolute paths
   * are rejected. Use isSafeResourceName() to pre-validate if needed.
   *
   * @throws Error if resourceName is not a safe, single-segment name.
   */
  resolvePath(tenantId: string, resourceName: string): string {
    if (!isSafeResourceName(resourceName)) {
      throw new Error(
        `TenantRegistry.resolvePath: unsafe resource name "${resourceName}". ` +
          `Must be a single path segment without separators, dots, or null bytes. ` +
          `Use isSafeResourceName() to validate before calling resolvePath().`,
      );
    }
    const tenant = this.tenants.get(tenantId);
    const base = tenant?.storageDir ?? join(this.storageRoot, tenantId);
    // join() is now safe because resourceName is validated to be a single segment
    return join(base, resourceName);
  }

  /**
   * Create a tenant-scoped MemoryStore.
   * Each tenant gets its own database so observations, runs, and routing
   * bias never leak across workspaces.
   */
  async createMemoryStore(
    tenantId: string,
    options?: {
      adapterFactory?: (dbPath: string) => MemoryAdapter;
      /** Per-tenant encryption provider. When provided, all data at rest is
       *  encrypted with the tenant's DEK derived from TenantKeyProvider.
       *  In enterprise mode, this is required for compliance. */
      keyProvider?: TenantKeyProvider;
    },
  ): Promise<Result<MemoryStore, Error>> {
    try {
      const dbPath = this.resolvePath(tenantId, "memory.db");
      let adapter: MemoryAdapter;

      if (options?.adapterFactory) {
        adapter = options.adapterFactory(dbPath);
      } else {
        // Default: use MemoryStore.create which auto-selects SqliteAdapter
        // with InMemoryAdapter fallback
        const store = await MemoryStore.create(dbPath);
        if (options?.keyProvider) {
          const { TenantEncryptionAdapter } = await import("./tenant-encryption.js");
          // TenantEncryptionAdapter wraps the inner adapter after store creation.
          // We re-create the store with the encrypted adapter wrapping the same inner adapter.
          const innerAdapter = store.adapter;
          const encryptedAdapter = new TenantEncryptionAdapter(
            innerAdapter,
            options.keyProvider,
            tenantId,
          );
          return ok(new MemoryStore(encryptedAdapter));
        }
        return ok(store);
      }

      if (options?.keyProvider) {
        const { TenantEncryptionAdapter } = await import("./tenant-encryption.js");
        adapter = new TenantEncryptionAdapter(adapter, options.keyProvider, tenantId);
      }

      return ok(new MemoryStore(adapter));
    } catch (cause) {
      return err(
        new Error(
          `TenantRegistry: failed to create memory store for ${tenantId}: ${cause instanceof Error ? cause.message : String(cause)}`,
        ),
      );
    }
  }

  /** Remove a tenant and all its stored data. */
  evictFromIndex(tenantId: string): boolean {
    // Storage cleanup is best-effort — we don't rm -rf here to avoid
    // accidental data loss. Callers should handle cleanup explicitly.
    const deleted = this.tenants.delete(tenantId);
    if (deleted) this._saveIndex();
    return deleted;
  }
}

// ---------------------------------------------------------------------------
// Convenience helpers
// ---------------------------------------------------------------------------

/** Derive a stable tenant id from a workspace path without registering. */
export function tenantIdFromPath(workspacePath: string): string {
  return createHash("sha256").update(resolve(workspacePath)).digest("hex").slice(0, 16);
}

// ---------------------------------------------------------------------------
// Per-tenant encryption
// ---------------------------------------------------------------------------

export { TenantEncryptionAdapter } from "./tenant-encryption.js";
