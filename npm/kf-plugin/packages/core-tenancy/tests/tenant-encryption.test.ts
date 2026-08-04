import { describe, it, expect } from "vitest";
import { TenantEncryptionAdapter } from "../src/tenant-encryption.js";
import { TenantKeyProvider } from "@kirkforge/core-secrets";
import type {
  MemoryAdapter,
  MemoryObject,
  MemoryQuery,
  MemoryStats,
} from "@kirkforge/memory-palace";
import { ok, type Result } from "@kirkforge/core-types";
import { randomBytes } from "node:crypto";

// ── In-memory adapter for testing ────────────────────────────────────────────

class InMemoryAdapter implements MemoryAdapter {
  private store = new Map<string, MemoryObject>();

  async write(obj: MemoryObject): Promise<Result<void, Error>> {
    this.store.set(obj.id, { ...obj });
    return ok(undefined);
  }

  async read(id: string): Promise<Result<MemoryObject | null, Error>> {
    const obj = this.store.get(id) ?? null;
    return ok(obj);
  }

  async query(q: MemoryQuery): Promise<Result<MemoryObject[], Error>> {
    let results = [...this.store.values()];
    if (q.kind) results = results.filter((o) => o.kind === q.kind);
    if (q.tags && q.tags.length > 0) {
      results = results.filter((o) => q.tags!.every((t) => o.tags.includes(t)));
    }
    if (q.since) results = results.filter((o) => o.timestamp >= q.since!);
    if (q.limit) results = results.slice(0, q.limit);
    return ok(results);
  }

  async stats(): Promise<Result<MemoryStats, Error>> {
    const objs = [...this.store.values()];
    return ok({
      totalObjects: objs.length,
      lastWrite: objs.length > 0 ? objs[objs.length - 1]!.timestamp : "",
    });
  }

  async persist(): Promise<void> {}
}

function makeKeyProvider(): TenantKeyProvider {
  return new TenantKeyProvider({ masterKey: randomBytes(32) });
}

function makeObject(id: string, overrides: Partial<MemoryObject> = {}): MemoryObject {
  return {
    id,
    kind: "task-observation",
    taskId: "task-1",
    timestamp: new Date().toISOString(),
    description: `Test observation ${id}`,
    properties: { language: "python", mode: "artifact" },
    tags: ["python", "test"],
    ...overrides,
  };
}

function setup() {
  const innerAdapter = new InMemoryAdapter();
  const keyProvider = makeKeyProvider();
  const adapter = new TenantEncryptionAdapter(innerAdapter, keyProvider, "tenant-1");
  return { innerAdapter, keyProvider, adapter };
}

// ── Tests ────────────────────────────────────────────────────────────────────

describe("TenantEncryptionAdapter", () => {
  it("encrypts description and properties on write; tags are plaintext for queries", async () => {
    const { innerAdapter, adapter } = setup();
    const obj = makeObject("enc-1");
    const result = await adapter.write(obj);
    expect(result.ok).toBe(true);

    // Verify the inner adapter stores encrypted data
    const stored = await innerAdapter.read("enc-1");
    expect(stored.ok).toBe(true);
    const storedObj = stored.value!;

    // Description should be ciphertext (starts with v{version}:)
    expect(storedObj.description).toMatch(/^v\d+:/);
    expect(storedObj.description).not.toBe(obj.description);

    // Properties should have _enc key with ciphertext
    expect(storedObj.properties).toHaveProperty("_enc");
    expect(typeof storedObj.properties._enc).toBe("string");
    expect(storedObj.properties._enc).toMatch(/^v\d+:/);

    // Tags are stored as plaintext for query support
    expect(storedObj.tags).toEqual(obj.tags);
  });

  it("round-trips: write then read returns original data", async () => {
    const { adapter } = setup();
    const obj = makeObject("round-trip-1", {
      description: "Sensitive tenant data that must be encrypted",
      properties: { language: "typescript", mode: "hard-prompt", tokens: 500 },
      tags: ["typescript", "production"],
    });

    await adapter.write(obj);
    const result = await adapter.read("round-trip-1");
    expect(result.ok).toBe(true);

    const decrypted = result.value!;
    expect(decrypted.description).toBe(obj.description);
    expect(decrypted.properties).toEqual(obj.properties);
    expect(decrypted.tags).toEqual(obj.tags);
    // Non-encrypted fields pass through unchanged
    expect(decrypted.id).toBe(obj.id);
    expect(decrypted.kind).toBe(obj.kind);
    expect(decrypted.taskId).toBe(obj.taskId);
  });

  it("round-trips complex nested properties", async () => {
    const { adapter } = setup();
    const obj = makeObject("complex-1", {
      properties: {
        nested: { a: [1, 2, 3], b: { deep: true } },
        array: ["x", "y", "z"],
        number: 42,
        null_val: null,
        bool: false,
      },
    });

    await adapter.write(obj);
    const result = await adapter.read("complex-1");
    expect(result.ok).toBe(true);
    expect(result.value!.properties).toEqual(obj.properties);
  });

  it("queries by plaintext tags correctly", async () => {
    const { adapter } = setup();
    const obj1 = makeObject("tag-1", { tags: ["python", "prod"] });
    const obj2 = makeObject("tag-2", { tags: ["typescript", "dev"] });

    await adapter.write(obj1);
    await adapter.write(obj2);

    // Query with plaintext tags — tags are stored as plaintext for queryability
    const result = await adapter.query({ tags: ["python"] });
    expect(result.ok).toBe(true);
    expect(result.value.length).toBe(1);
    expect(result.value[0]!.id).toBe("tag-1");
    expect(result.value[0]!.description).toBe(obj1.description);
  });

  it("queries by kind without tags (kind is not encrypted)", async () => {
    const { adapter } = setup();
    const obj = makeObject("kind-1", { kind: "emission" });
    await adapter.write(obj);

    const result = await adapter.query({ kind: "emission" });
    expect(result.ok).toBe(true);
    expect(result.value.length).toBe(1);
    expect(result.value[0]!.id).toBe("kind-1");
  });

  it("returns error on decryption failure for ciphertext data", async () => {
    const { innerAdapter, adapter } = setup();
    const obj = makeObject("fail-decrypt-1");
    await adapter.write(obj);

    // Tamper with the stored ciphertext — replace description with invalid ciphertext
    const stored = await innerAdapter.read("fail-decrypt-1");
    stored.value!.description = "v999:invalid:ciphertext:data";
    await innerAdapter.write(stored.value!);

    const result = await adapter.read("fail-decrypt-1");
    // Should return an error, not silently return garbage
    expect(result.ok).toBe(false);
  });

  it("passes through unencrypted legacy data unchanged", async () => {
    const { innerAdapter, adapter } = setup();
    // Write unencrypted data directly to the inner adapter
    const legacyObj = makeObject("legacy-1");
    await innerAdapter.write(legacyObj);

    const result = await adapter.read("legacy-1");
    expect(result.ok).toBe(true);
    // Legacy data should pass through unchanged
    expect(result.value!.description).toBe(legacyObj.description);
    expect(result.value!.properties).toEqual(legacyObj.properties);
  });

  it("isolates tenant data — different tenants cannot decrypt each other's data", async () => {
    const { innerAdapter, keyProvider } = setup();
    const adapterA = new TenantEncryptionAdapter(innerAdapter, keyProvider, "tenant-a");
    const adapterB = new TenantEncryptionAdapter(innerAdapter, keyProvider, "tenant-b");

    const secretData = makeObject("iso-1", {
      description: "Tenant A secret",
      properties: { secret: "only-for-a" },
    });

    await adapterA.write(secretData);

    // Tenant B reads the same record — should fail to decrypt
    const result = await adapterB.read("iso-1");
    expect(result.ok).toBe(false);
  });

  it("works with key rotation — old data still readable", async () => {
    const { keyProvider, adapter } = setup();
    const obj = makeObject("rotate-1", {
      description: "Data before rotation",
      properties: { version: 1 },
    });

    await adapter.write(obj);

    // Rotate the key
    keyProvider.rotateKey("tenant-1");

    // Old data should still be decryptable
    const result = await adapter.read("rotate-1");
    expect(result.ok).toBe(true);
    expect(result.value!.description).toBe("Data before rotation");

    // New writes should use the new key version
    const obj2 = makeObject("rotate-2", {
      description: "Data after rotation",
      properties: { version: 2 },
    });
    await adapter.write(obj2);

    const result2 = await adapter.read("rotate-2");
    expect(result2.ok).toBe(true);
    expect(result2.value!.description).toBe("Data after rotation");
  });

  it("stats() delegates to inner adapter", async () => {
    const { adapter } = setup();
    const obj = makeObject("stats-isolated-1");
    await adapter.write(obj);

    const stats = await adapter.stats();
    expect(stats.ok).toBe(true);
    expect(stats.value!.totalObjects).toBe(1);
  });
});
