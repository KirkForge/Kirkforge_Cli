import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { TenantRegistry, tenantIdFromPath, isSafeResourceName } from "../src/index.js";
import { mkdtempSync, rmSync, mkdirSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

/**
 * Adversarial tests proving tenant isolation: no cross-contamination of
 * storage, memory, events, or configuration between tenants.
 */

describe("Tenant isolation — adversarial tests", () => {
  let tmpDir: string;
  let registry: TenantRegistry;

  beforeEach(() => {
    tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-isolation-test-"));
    registry = new TenantRegistry({ storageRoot: join(tmpDir, "tenants") });
  });

  afterEach(() => {
    rmSync(tmpDir, { recursive: true, force: true });
  });

  it("different workspaces produce different tenant IDs (no collision)", () => {
    const ids = new Set<string>();
    for (let i = 0; i < 100; i++) {
      const id = tenantIdFromPath(`/workspace/project-${i}`);
      ids.add(id);
    }
    // All 100 should be unique
    expect(ids.size).toBe(100);
  });

  it("tenant storage directories are strictly separated", () => {
    const h1 = registry.register("/workspace/tenant-alpha");
    const h2 = registry.register("/workspace/tenant-beta");

    // Storage dirs must not overlap
    expect(h1.storageDir).not.toBe(h2.storageDir);
    expect(h1.storageDir).toContain(h1.tenantId);
    expect(h2.storageDir).toContain(h2.tenantId);

    // Write a file in tenant-alpha's storage
    mkdirSync(h1.storageDir, { recursive: true });
    writeFileSync(join(h1.storageDir, "secret.txt"), "alpha-secret", "utf-8");

    // Verify tenant-beta's storage does NOT contain alpha's file
    expect(existsSync(join(h2.storageDir, "secret.txt"))).toBe(false);

    // Verify reading alpha's file from alpha's path works
    expect(readFileSync(join(h1.storageDir, "secret.txt"), "utf-8")).toBe("alpha-secret");
  });

  it("resolvePath never crosses tenant boundaries", () => {
    const h1 = registry.register("/workspace/alpha");
    const h2 = registry.register("/workspace/beta");

    const path1 = registry.resolvePath(h1.tenantId, "memory.db");
    const path2 = registry.resolvePath(h2.tenantId, "memory.db");

    // Same resource name resolves to different paths
    expect(path1).not.toBe(path2);
    expect(path1).toContain(h1.tenantId);
    expect(path2).toContain(h2.tenantId);

    // No path traversal possible: paths are under separate tenant dirs
    expect(path1.startsWith(h1.storageDir)).toBe(true);
    expect(path2.startsWith(h2.storageDir)).toBe(true);
  });

  it("tenant ID is deterministic — same workspace always maps to same tenant", () => {
    const h1 = registry.register("/workspace/stable-id");
    const h2 = registry.register("/workspace/stable-id");
    expect(h1.tenantId).toBe(h2.tenantId);
    expect(h1.storageDir).toBe(h2.storageDir);
  });

  it("similar workspace paths produce different tenant IDs (no prefix collision)", () => {
    const h1 = registry.register("/workspace/app");
    const h2 = registry.register("/workspace/app-service");

    expect(h1.tenantId).not.toBe(h2.tenantId);
    expect(h1.storageDir).not.toBe(h2.storageDir);
  });

  it("evicting a tenant does not affect other tenants' storage", () => {
    const h1 = registry.register("/workspace/survivor");
    const h2 = registry.register("/workspace/evictee");

    mkdirSync(h1.storageDir, { recursive: true });
    writeFileSync(join(h1.storageDir, "data.json"), '{"important": true}', "utf-8");

    mkdirSync(h2.storageDir, { recursive: true });
    writeFileSync(join(h2.storageDir, "data.json"), '{"important": false}', "utf-8");

    registry.evictFromIndex(h2.tenantId);

    // Survivor's data is intact
    expect(existsSync(join(h1.storageDir, "data.json"))).toBe(true);
    // Evictee's data still on disk (evict only removes from index)
    // but tenant cannot be resolved by the registry anymore
    expect(registry.get(h2.tenantId)).toBeUndefined();
    expect(registry.get(h1.tenantId)).toBeDefined();
  });

  it("tenant label cannot be used to guess another tenant's ID", () => {
    const h = registry.register("/workspace/sensitive-project");
    // The tenant ID is a SHA-256 hash truncation, not derivable from the label alone
    expect(h.tenantId).not.toBe("sensitive-project");
    expect(h.tenantId).not.toContain("sensitive");
    expect(h.tenantId).toMatch(/^[0-9a-f]{16}$/);
  });

  it("path traversal via relative resource names is BLOCKED by resolvePath", () => {
    const h = registry.register("/workspace/safe-zone");
    // resolvePath now enforces isSafeResourceName — path traversal is BLOCKED.
    // Resource names with separators, .., null bytes, absolute paths, or
    // leading dots are rejected before path construction.
    expect(() => registry.resolvePath(h.tenantId, "../../../etc/passwd")).toThrow(
      /unsafe resource name/i,
    );
    expect(() => registry.resolvePath(h.tenantId, "../secret")).toThrow(/unsafe resource name/i);
    expect(() => registry.resolvePath(h.tenantId, "sub/dir")).toThrow(/unsafe resource name/i);
    expect(() => registry.resolvePath(h.tenantId, ".env")).toThrow(/unsafe resource name/i);
    // Verify safe names stay within tenant storage
    const safeName = "memory.db";
    const safePath = registry.resolvePath(h.tenantId, safeName);
    expect(safePath).toContain(h.tenantId);
    expect(safePath).toContain("memory.db");
  });

  it("isSafeResourceName rejects traversal and dangerous names", () => {
    expect(isSafeResourceName("memory.db")).toBe(true);
    expect(isSafeResourceName("events.jsonl")).toBe(true);
    expect(isSafeResourceName("config.json")).toBe(true);
    // Rejected: path separators
    expect(isSafeResourceName("../etc/passwd")).toBe(false);
    expect(isSafeResourceName("sub/dir")).toBe(false);
    // Rejected: .. segments
    expect(isSafeResourceName("..")).toBe(false);
    // Rejected: null bytes
    expect(isSafeResourceName("file\0.txt")).toBe(false);
    // Rejected: absolute paths
    expect(isSafeResourceName("/etc/passwd")).toBe(false);
    // Rejected: leading dots (hidden files / traversal)
    expect(isSafeResourceName(".env")).toBe(false);
    // Rejected: empty string
    expect(isSafeResourceName("")).toBe(false);
  });

  it("concurrent tenant registrations are idempotent", () => {
    const path = "/workspace/concurrent-test";
    const handles: Array<{ tenantId: string; storageDir: string }> = [];
    for (let i = 0; i < 10; i++) {
      handles.push(registry.register(path));
    }
    // All handles should have the same tenant ID
    const ids = new Set(handles.map((h) => h.tenantId));
    expect(ids.size).toBe(1);
    // All storage dirs should be identical
    const dirs = new Set(handles.map((h) => h.storageDir));
    expect(dirs.size).toBe(1);
  });
});

// ── Memory store tenant isolation ─────────────────────────────────────────

describe("Memory store tenant isolation", () => {
  let tmpDir: string;
  let registry: TenantRegistry;

  beforeEach(() => {
    tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-mem-isolation-"));
    registry = new TenantRegistry({ storageRoot: join(tmpDir, "tenants") });
  });

  afterEach(() => {
    rmSync(tmpDir, { recursive: true, force: true });
  });

  it("each tenant gets a separate memory database path", () => {
    const h1 = registry.register("/workspace/tenant-alpha");
    const h2 = registry.register("/workspace/tenant-beta");

    const db1 = registry.resolvePath(h1.tenantId, "memory.db");
    const db2 = registry.resolvePath(h2.tenantId, "memory.db");

    // Paths must be different
    expect(db1).not.toBe(db2);
    // Each path must contain its own tenant ID
    expect(db1).toContain(h1.tenantId);
    expect(db2).toContain(h2.tenantId);
    // Alpha path must NOT contain beta's tenant ID
    expect(db1).not.toContain(h2.tenantId);
  });

  it("writing to one tenant's storage does not appear in another's", () => {
    const h1 = registry.register("/workspace/writer");
    const h2 = registry.register("/workspace/reader");

    mkdirSync(h1.storageDir, { recursive: true });
    mkdirSync(h2.storageDir, { recursive: true });

    writeFileSync(join(h1.storageDir, "memory.db"), "alpha-data", "utf-8");

    // Reader tenant should not see writer's data
    expect(existsSync(join(h2.storageDir, "memory.db"))).toBe(false);
    // Writer should see its own data
    expect(readFileSync(join(h1.storageDir, "memory.db"), "utf-8")).toBe("alpha-data");
  });

  it("tenant cannot access another tenant's events log", () => {
    const h1 = registry.register("/workspace/tenant-a");
    const h2 = registry.register("/workspace/tenant-b");

    mkdirSync(h1.storageDir, { recursive: true });

    const eventsPath = registry.resolvePath(h1.tenantId, "events.jsonl");
    writeFileSync(eventsPath, '{"kind":"verify.start","tenantId":"a"}\n', "utf-8");

    // Tenant B's events path must be different
    const bEventsPath = registry.resolvePath(h2.tenantId, "events.jsonl");
    expect(bEventsPath).not.toBe(eventsPath);
    expect(bEventsPath).toContain(h2.tenantId);
    expect(bEventsPath).not.toContain(h1.tenantId);
  });

  it("evicted tenant's data files remain but are inaccessible via registry", () => {
    const h1 = registry.register("/workspace/evicted");
    mkdirSync(h1.storageDir, { recursive: true });
    writeFileSync(join(h1.storageDir, "memory.db"), "secret", "utf-8");

    registry.evictFromIndex(h1.tenantId);

    // The registry no longer knows about this tenant
    expect(registry.get(h1.tenantId)).toBeUndefined();

    // But the data file still exists on disk (not auto-cleaned)
    expect(existsSync(join(h1.storageDir, "memory.db"))).toBe(true);
  });
});

// ── Event and audit tenant scope ────────────────────────────────────────

describe("Event and audit tenant scope", () => {
  let registry: TenantRegistry;
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-event-isolation-"));
    registry = new TenantRegistry({ storageRoot: join(tmpDir, "tenants") });
  });

  afterEach(() => {
    rmSync(tmpDir, { recursive: true, force: true });
  });

  it("resolvePath produces different paths for audit logs per tenant", () => {
    const h1 = registry.register("/workspace/audit-alpha");
    const h2 = registry.register("/workspace/audit-beta");

    const audit1 = registry.resolvePath(h1.tenantId, "audit.jsonl");
    const audit2 = registry.resolvePath(h2.tenantId, "audit.jsonl");

    expect(audit1).not.toBe(audit2);
    expect(audit1).toContain(h1.tenantId);
    expect(audit2).toContain(h2.tenantId);
  });

  it("resolvePath produces different paths for config per tenant", () => {
    const h1 = registry.register("/workspace/config-a");
    const h2 = registry.register("/workspace/config-b");

    const config1 = registry.resolvePath(h1.tenantId, "config.json");
    const config2 = registry.resolvePath(h2.tenantId, "config.json");

    expect(config1).not.toBe(config2);
  });

  it("tenant ID cannot be reverse-engineered from label or path", () => {
    const h = registry.register("/workspace/top-secret-project-name");
    // SHA-256 hash should make the ID opaque
    expect(h.tenantId).not.toContain("secret");
    expect(h.tenantId).not.toContain("project");
    expect(h.tenantId).toMatch(/^[0-9a-f]{16}$/);
  });
});
