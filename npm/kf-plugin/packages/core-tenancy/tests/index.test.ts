import { describe, it, expect } from "vitest";
import { tenantIdFromPath, TenantRegistry } from "../src/index.js";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

describe("tenantIdFromPath", () => {
  it("derives a stable 16-char hex id from a path", () => {
    const id = tenantIdFromPath("/home/user/projects/my-app");
    expect(id).toHaveLength(16);
    expect(/^[0-9a-f]{16}$/.test(id)).toBe(true);
  });

  it("returns the same id for the same path", () => {
    expect(tenantIdFromPath("/tmp/test-workspace")).toBe(tenantIdFromPath("/tmp/test-workspace"));
  });

  it("returns different ids for different paths", () => {
    expect(tenantIdFromPath("/tmp/workspace-a")).not.toBe(tenantIdFromPath("/tmp/workspace-b"));
  });

  it("resolves relative paths before hashing", () => {
    const id1 = tenantIdFromPath("/tmp/./test-workspace");
    const id2 = tenantIdFromPath("/tmp/test-workspace");
    expect(id1).toBe(id2);
  });
});

describe("TenantRegistry", () => {
  let tmpDir: string;
  let registry: TenantRegistry;

  function setup() {
    tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-tenancy-test-"));
    registry = new TenantRegistry({ storageRoot: join(tmpDir, "tenants") });
  }

  function cleanup() {
    rmSync(tmpDir, { recursive: true, force: true });
  }

  it("registers a new tenant and returns a handle", () => {
    setup();
    try {
      const handle = registry.register("/tmp/my-workspace");
      expect(handle.tenantId).toHaveLength(16);
      expect(handle.label).toBe("my-workspace");
      expect(handle.workspacePath).toBe("/tmp/my-workspace");
      expect(handle.storageDir).toContain("tenants");
    } finally {
      cleanup();
    }
  });

  it("register is idempotent", () => {
    setup();
    try {
      const h1 = registry.register("/tmp/stable-workspace");
      const h2 = registry.register("/tmp/stable-workspace");
      expect(h1.tenantId).toBe(h2.tenantId);
      expect(h1.createdAt).toBe(h2.createdAt);
    } finally {
      cleanup();
    }
  });

  it("get returns tenant by id", () => {
    setup();
    try {
      const handle = registry.register("/tmp/get-test");
      const found = registry.get(handle.tenantId);
      expect(found).toBeDefined();
      expect(found!.label).toBe("get-test");
    } finally {
      cleanup();
    }
  });

  it("get returns undefined for unknown id", () => {
    setup();
    try {
      expect(registry.get("nonexistent")).toBeUndefined();
    } finally {
      cleanup();
    }
  });

  it("list returns all registered tenants", () => {
    setup();
    try {
      registry.register("/tmp/ws-a");
      registry.register("/tmp/ws-b");
      expect(registry.list()).toHaveLength(2);
    } finally {
      cleanup();
    }
  });

  it("resolvePath returns a tenant-scoped path", () => {
    setup();
    try {
      const handle = registry.register("/tmp/resolve-ws");
      const dbPath = registry.resolvePath(handle.tenantId, "memory.db");
      expect(dbPath).toContain(handle.tenantId);
      expect(dbPath).toContain("memory.db");
    } finally {
      cleanup();
    }
  });

  it("evictFromIndex removes tenant from the index", () => {
    setup();
    try {
      const handle = registry.register("/tmp/evict-me");
      expect(registry.get(handle.tenantId)).toBeDefined();
      const removed = registry.evictFromIndex(handle.tenantId);
      expect(removed).toBe(true);
      expect(registry.get(handle.tenantId)).toBeUndefined();
    } finally {
      cleanup();
    }
  });

  it("evictFromIndex returns false for unknown id", () => {
    setup();
    try {
      expect(registry.evictFromIndex("nonexistent")).toBe(false);
    } finally {
      cleanup();
    }
  });

  it("persists tenants across registry instances via index.json", () => {
    setup();
    try {
      const handle = registry.register("/tmp/persist-ws");
      const tenantId = handle.tenantId;
      const registry2 = new TenantRegistry({ storageRoot: join(tmpDir, "tenants") });
      const found = registry2.get(tenantId);
      expect(found).toBeDefined();
      expect(found!.label).toBe("persist-ws");
    } finally {
      cleanup();
    }
  });
});

// ── Path traversal protection tests ──────────────────────────────────────
import { isSafeResourceName } from "../src/index.js";

describe("isSafeResourceName", () => {
  it("accepts simple safe names", () => {
    expect(isSafeResourceName("memory.db")).toBe(true);
    expect(isSafeResourceName("events.jsonl")).toBe(true);
    expect(isSafeResourceName("config.toml")).toBe(true);
    expect(isSafeResourceName("data")).toBe(true);
  });

  it("rejects empty strings", () => {
    expect(isSafeResourceName("")).toBe(false);
  });

  it("rejects path separators", () => {
    expect(isSafeResourceName("../../../etc/passwd")).toBe(false);
    expect(isSafeResourceName("sub/file.txt")).toBe(false);
    expect(isSafeResourceName("sub\\file.txt")).toBe(false);
  });

  it("rejects .. segments", () => {
    expect(isSafeResourceName("..")).toBe(false);
    expect(isSafeResourceName("..something")).toBe(false);
  });

  it("rejects null bytes", () => {
    expect(isSafeResourceName("bad\0name")).toBe(false);
  });

  it("rejects absolute paths", () => {
    expect(isSafeResourceName("/etc/passwd")).toBe(false);
    expect(isSafeResourceName("C:\\Windows\\System32")).toBe(false);
  });

  it("rejects hidden/dot files", () => {
    expect(isSafeResourceName(".env")).toBe(false);
    expect(isSafeResourceName(".htaccess")).toBe(false);
  });
});

describe("TenantRegistry resolvePath path traversal protection", () => {
  let tmpDir: string;
  let registry: TenantRegistry;

  function setup() {
    tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-traversal-test-"));
    registry = new TenantRegistry({ storageRoot: join(tmpDir, "tenants") });
  }

  function cleanup() {
    rmSync(tmpDir, { recursive: true, force: true });
  }

  it("throws on path traversal via resource name", () => {
    setup();
    try {
      const handle = registry.register("/workspace/safe-zone");
      expect(() => registry.resolvePath(handle.tenantId, "../../../etc/passwd")).toThrow();
    } finally {
      cleanup();
    }
  });

  it("throws on subdirectory resource name", () => {
    setup();
    try {
      const handle = registry.register("/workspace/test");
      expect(() => registry.resolvePath(handle.tenantId, "sub/directory")).toThrow();
    } finally {
      cleanup();
    }
  });

  it("throws on dot-file resource name", () => {
    setup();
    try {
      const handle = registry.register("/workspace/test");
      expect(() => registry.resolvePath(handle.tenantId, ".env")).toThrow();
    } finally {
      cleanup();
    }
  });

  it("accepts safe resource names", () => {
    setup();
    try {
      const handle = registry.register("/workspace/test");
      expect(registry.resolvePath(handle.tenantId, "memory.db")).toContain("memory.db");
      expect(registry.resolvePath(handle.tenantId, "events.jsonl")).toContain("events.jsonl");
      expect(registry.resolvePath(handle.tenantId, "data")).toContain("data");
    } finally {
      cleanup();
    }
  });
});
