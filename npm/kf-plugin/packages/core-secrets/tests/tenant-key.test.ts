import { describe, it, expect } from "vitest";
import { TenantKeyProvider } from "../src/index.js";
import { randomBytes } from "node:crypto";

describe("TenantKeyProvider", () => {
  function makeProvider(activeVersions = 2) {
    return new TenantKeyProvider({
      masterKey: randomBytes(32),
      activeKeyVersions: activeVersions,
    });
  }

  it("derives a unique key per tenant", () => {
    const provider = makeProvider();
    const key1 = provider.getCurrentKey("tenant-a");
    const key2 = provider.getCurrentKey("tenant-b");
    expect(key1.key).not.toEqual(key2.key);
  });

  it("starts at version 1", () => {
    const provider = makeProvider();
    const key = provider.getCurrentKey("tenant-1");
    expect(key.version).toBe(1);
  });

  it("increments version on rotation", () => {
    const provider = makeProvider();
    const v1 = provider.getCurrentKey("tenant-1");
    const v2 = provider.rotateKey("tenant-1");
    expect(v2.version).toBe(2);
    expect(v2.key).not.toEqual(v1.key);
  });

  it("prunes old key versions based on activeKeyVersions config", () => {
    const provider = makeProvider(2); // keep 2 active
    provider.rotateKey("tenant-1"); // v2
    provider.rotateKey("tenant-1"); // v3
    // With activeKeyVersions=2, should only have v2 and v3
    const keys = provider.getActiveKeys("tenant-1");
    expect(keys).toHaveLength(2);
    expect(keys[0]!.version).toBe(2);
    expect(keys[1]!.version).toBe(3);
  });

  it("encrypts and decrypts with the current key", () => {
    const provider = makeProvider();
    const plaintext = "sensitive-tenant-data";
    const ciphertext = provider.encryptForTenant("tenant-1", plaintext);
    const decrypted = provider.decryptForTenant("tenant-1", ciphertext);
    expect(decrypted).toBe(plaintext);
  });

  it("still decrypts after key rotation (backward compatibility)", () => {
    const provider = makeProvider();
    const plaintext = "data-encrypted-before-rotation";
    const ciphertext = provider.encryptForTenant("tenant-1", plaintext);

    // Rotate key — old key should still be available for decryption
    provider.rotateKey("tenant-1");
    const decrypted = provider.decryptForTenant("tenant-1", ciphertext);
    expect(decrypted).toBe(plaintext);
  });

  it("fails to decrypt after old key version is pruned", () => {
    const provider = makeProvider(1); // only keep 1 version
    const plaintext = "will-be-lost";
    const ciphertext = provider.encryptForTenant("tenant-1", plaintext);

    // Rotate key — with only 1 active version, the old key is pruned
    provider.rotateKey("tenant-1");
    expect(() => provider.decryptForTenant("tenant-1", ciphertext)).toThrow(
      /Key version 1 not found/,
    );
  });

  it("different tenants cannot decrypt each other's data", () => {
    const provider = makeProvider();
    const ciphertext = provider.encryptForTenant("tenant-a", "secret-a");
    expect(() => provider.decryptForTenant("tenant-b", ciphertext)).toThrow();
  });

  it("rejects master keys that are not 32 bytes", () => {
    expect(() => new TenantKeyProvider({ masterKey: Buffer.from("short", "utf-8") })).toThrow(
      "must be exactly 32 bytes",
    );
  });

  it("clears cache for all tenants", () => {
    const provider = makeProvider();
    provider.getCurrentKey("tenant-1");
    provider.getCurrentKey("tenant-2");
    provider.clearCache();
    // After clearing, new keys will be derived fresh (same master key = same keys)
    const k1 = provider.getCurrentKey("tenant-1");
    expect(k1.version).toBe(1); // starts fresh after cache clear
  });

  it("clears cache for a specific tenant", () => {
    const provider = makeProvider();
    provider.getCurrentKey("tenant-1");
    provider.getCurrentKey("tenant-2");
    provider.clearTenantCache("tenant-1");
    // tenant-1 should start fresh, tenant-2 should still have v1
    const k1 = provider.getCurrentKey("tenant-1");
    const k2 = provider.getCurrentKey("tenant-2");
    expect(k1.version).toBe(1);
    expect(k2.version).toBe(1);
  });
});
