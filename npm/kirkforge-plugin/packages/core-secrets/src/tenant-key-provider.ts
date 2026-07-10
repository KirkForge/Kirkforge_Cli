import { createHmac, randomBytes, createCipheriv, createDecipheriv } from "node:crypto";

// In multi-tenant deployments, each tenant MUST have a unique Key Encryption
// Key (KEK) so that compromise of one tenant's data does not affect others.
//
// The TenantKeyProvider resolves a Data Encryption Key (DEK) per tenant,
// derived from the tenant ID and a master KEK. Key rotation is supported
// by maintaining a versioned key chain: each tenant can have multiple active
// key versions, and decryption tries all active versions.
//
// Architecture:
//   Master KEK (stored in Vault/KMS)
//     └── Tenant DEK v2 = HKDF-SHA256(KEK, tenantId || "v2")
//     └── Tenant DEK v1 = HKDF-SHA256(KEK, tenantId || "v1")  ← still valid for decryption
//
// Encryption always uses the latest version. Decryption tries all active
// versions, enabling seamless key rotation without downtime.

export interface TenantKeyVersion {
  /** Key version identifier (monotonically increasing). */
  version: number;
  /** The derived key material (32 bytes for AES-256-GCM). */
  key: Buffer;
  /** ISO timestamp when this version became active. */
  activatedAt: string;
}

export interface TenantKeyProviderConfig {
  /** Master Key Encryption Key. Must be exactly 32 bytes (AES-256). */
  masterKey: Buffer;
  /** Number of key versions to keep active for decryption. Default: 2. */
  activeKeyVersions?: number;
}

/**
 * Per-tenant DEK provider. Each tenant gets a versioned chain of derived
 * keys so encryption uses the latest version while decryption tolerates
 * older versions during rotation windows.
 */
export class TenantKeyProvider {
  private masterKey: Buffer;
  private activeKeyVersions: number;
  private keyCache = new Map<string, TenantKeyVersion[]>();

  constructor(config: TenantKeyProviderConfig) {
    if (config.masterKey.length !== 32) {
      throw new Error("TenantKeyProvider: masterKey must be exactly 32 bytes (AES-256)");
    }
    this.masterKey = Buffer.from(config.masterKey); // copy to prevent external mutation
    this.activeKeyVersions = config.activeKeyVersions ?? 2;
  }

  /**
   * Get the current (latest version) encryption key for a tenant.
   * Used for encrypting new data.
   */
  getCurrentKey(tenantId: string): TenantKeyVersion {
    const versions = this.getVersions(tenantId);
    return versions[versions.length - 1]!;
  }

  /**
   * Get all active key versions for a tenant.
   * Used for decryption — try each version until one succeeds.
   */
  getActiveKeys(tenantId: string): TenantKeyVersion[] {
    return this.getVersions(tenantId);
  }

  /**
   * Rotate keys for a tenant: increments the version number and derives
   * a new DEK. Previous versions remain active for decryption until
   * pruned by `pruneOldVersions`.
   */
  rotateKey(tenantId: string): TenantKeyVersion {
    const versions = this.getVersions(tenantId);
    const nextVersion = versions.length > 0 ? versions[versions.length - 1]!.version + 1 : 1;
    const newKey = this.deriveKey(tenantId, nextVersion);
    const newVersion: TenantKeyVersion = {
      version: nextVersion,
      key: newKey,
      activatedAt: new Date().toISOString(),
    };
    versions.push(newVersion);
    this.pruneOldVersions(tenantId);
    return newVersion;
  }

  /**
   * Encrypt a plaintext string for a specific tenant using the current key version.
   * Returns the ciphertext prefixed with the key version for later decryption.
   *
   * Format: v{version}:{iv}:{tag}:{ciphertext} (all base64)
   */
  encryptForTenant(tenantId: string, plaintext: string): string {
    const version = this.getCurrentKey(tenantId);
    const iv = randomBytes(12); // 12 bytes for GCM
    const cipher = createCipheriv("aes-256-gcm", version.key, iv);
    const encrypted = Buffer.concat([cipher.update(plaintext, "utf8"), cipher.final()]);
    const tag = cipher.getAuthTag();
    return `v${version.version}:${iv.toString("base64")}:${tag.toString("base64")}:${encrypted.toString("base64")}`;
  }

  /**
   * Decrypt a ciphertext for a specific tenant, trying all active key versions.
   */
  decryptForTenant(tenantId: string, ciphertext: string): string {
    // Parse version prefix
    const versionMatch = ciphertext.match(/^v(\d+):/);
    if (!versionMatch) {
      throw new Error("Invalid ciphertext format: missing version prefix");
    }
    const versionNum = parseInt(versionMatch[1]!, 10);
    const parts = ciphertext.slice(versionMatch[0].length).split(":");
    if (parts.length !== 3) {
      throw new Error("Invalid ciphertext format: expected v{version}:{iv}:{tag}:{ciphertext}");
    }

    const iv = Buffer.from(parts[0]!, "base64");
    const tag = Buffer.from(parts[1]!, "base64");
    const encrypted = Buffer.from(parts[2]!, "base64");

    // Find the specific key version
    const versions = this.getVersions(tenantId);
    const keyVersion = versions.find((v) => v.version === versionNum);
    if (!keyVersion) {
      throw new Error(
        `Key version ${versionNum} not found for tenant ${tenantId}. ` +
          `Active versions: ${versions.map((v) => v.version).join(", ")}`,
      );
    }

    const decipher = createDecipheriv("aes-256-gcm", keyVersion.key, iv);
    decipher.setAuthTag(tag);
    const decrypted = Buffer.concat([decipher.update(encrypted), decipher.final()]);
    return decrypted.toString("utf8");
  }

  /**
   * Clear cached keys for all tenants. Use this when the master key
   * is rotated and all derived keys need to be recomputed.
   */
  clearCache(): void {
    this.keyCache.clear();
  }

  /**
   * Clear cached keys for a specific tenant.
   */
  clearTenantCache(tenantId: string): void {
    this.keyCache.delete(tenantId);
  }

  // ── Internal ──────────────────────────────────────────────────────────

  private deriveKey(tenantId: string, version: number): Buffer {
    // HKDF-SHA256 derivation: masterKey || tenantId || version || app context
    // Using HMAC-SHA256 as a simple KDF (single-step HKDF)
    const info = `kirkforge-tenant-dek:${tenantId}:v${version}`;
    return createHmac("sha256", this.masterKey).update(info).digest();
  }

  private getVersions(tenantId: string): TenantKeyVersion[] {
    let versions = this.keyCache.get(tenantId);
    if (!versions) {
      // Always start with version 1
      const v1: TenantKeyVersion = {
        version: 1,
        key: this.deriveKey(tenantId, 1),
        activatedAt: new Date().toISOString(),
      };
      versions = [v1];
      this.keyCache.set(tenantId, versions);
    }
    return versions;
  }

  private pruneOldVersions(tenantId: string): void {
    const versions = this.keyCache.get(tenantId);
    if (!versions) return;
    // Keep only the last N active versions
    while (versions.length > this.activeKeyVersions) {
      versions.shift();
    }
  }
}
