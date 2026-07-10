/**
 * Per-tenant encryption adapter for MemoryStore.
 *
 * Uses TenantKeyProvider from core-secrets to derive per-tenant Data Encryption
 * Keys (DEKs) from a master Key Encryption Key (KEK). Each tenant's data is
 * encrypted with a unique DEK derived via HMAC-SHA256, supporting key rotation
 * with versioned ciphertext.
 *
 * Wire this into TenantRegistry.createMemoryStore() to enable per-tenant
 * encryption at rest. In enterprise mode, this is required — tenant data must
 * never be stored in plaintext.
 *
 * Design decisions:
 * - description and properties are fully encrypted (these contain sensitive data)
 * - tags are NOT encrypted (they are metadata labels used for querying; encrypting
 *   them with AES-GCM would make tag-based queries impossible since each encryption
 *   produces different ciphertext. Tags like "python" or "production" are less
 *   sensitive than descriptions and properties.)
 * - kind, taskId, timestamp, runId are NOT encrypted (needed for inner adapter queries)
 * - Encrypted properties are stored as { _enc: "v{version}:{iv}:{tag}:{ciphertext}" }
 *   to avoid attempting JSON.parse on ciphertext strings.
 */

import type {
  MemoryAdapter,
  MemoryObject,
  MemoryQuery,
  MemoryStats,
} from "@kirkforge/memory-palace";
import { ok, err, type Result } from "@kirkforge/core-types";
import type { TenantKeyProvider } from "@kirkforge/core-secrets";

// ── Ciphertext detection ──────────────────────────────────────────────────────

/** Pattern matching the TenantKeyProvider ciphertext format: v{version}:{iv}:{tag}:{data} */
const CIPHERTEXT_RE = /^v\d+:[A-Za-z0-9+/=]+:[A-Za-z0-9+/=]+:[A-Za-z0-9+/=]+$/;

/** Returns true if the string looks like versioned ciphertext from TenantKeyProvider. */
function isCiphertext(value: string): boolean {
  return CIPHERTEXT_RE.test(value);
}

// ── Tenant-scoped encryption adapter ────────────────────────────────────────

/**
 * Wraps any MemoryAdapter and encrypts/decrypts MemoryObject payloads using
 * per-tenant Data Encryption Keys (DEKs) derived from a master KEK.
 *
 * On write: encrypts `description` and `properties` with the tenant's current
 * DEK version. Tags are left as plaintext to support tag-based queries.
 * Properties are stored as `{ _enc: "v{version}:..." }` to avoid JSON.parse on
 * ciphertext strings.
 *
 * On read: decrypts using the appropriate key version, allowing transparent
 * key rotation — data written with an old DEK version is decrypted with that
 * version's key, while new writes use the latest version.
 *
 * If decryption fails on ciphertext-formatted data, the adapter returns an
 * error. Legacy unencrypted data (strings that don't match the ciphertext format)
 * passes through unchanged, enabling smooth migration.
 */
export class TenantEncryptionAdapter implements MemoryAdapter {
  constructor(
    private inner: MemoryAdapter,
    private keyProvider: TenantKeyProvider,
    private tenantId: string,
  ) {}

  async write(obj: MemoryObject): Promise<Result<void, Error>> {
    try {
      const encrypted: MemoryObject = {
        ...obj,
        description: this.keyProvider.encryptForTenant(this.tenantId, obj.description),
        properties: {
          _enc: this.keyProvider.encryptForTenant(this.tenantId, JSON.stringify(obj.properties)),
        },
        // Tags are left as plaintext for query support. Tags are metadata labels
        // (e.g., "python", "production") that are less sensitive than description
        // and properties. Encrypting tags with AES-GCM (which produces different
        // ciphertext per encryption due to random IVs) would break tag-based queries.
      };
      return this.inner.write(encrypted);
    } catch (cause) {
      return err(
        new Error(
          `TenantEncryptionAdapter: encryption failed for tenant ${this.tenantId}: ${cause instanceof Error ? cause.message : String(cause)}`,
        ),
      );
    }
  }

  async read(id: string): Promise<Result<MemoryObject | null, Error>> {
    const result = await this.inner.read(id);
    if (!result.ok || !result.value) return result;

    const obj = result.value;

    // If the data doesn't look like ciphertext, it's legacy unencrypted data
    if (!isCiphertext(obj.description) && !(obj.properties && "_enc" in obj.properties)) {
      return result;
    }

    try {
      return ok(this.decryptObject(obj));
    } catch (cause) {
      return err(
        new Error(
          `TenantEncryptionAdapter: decryption failed for tenant ${this.tenantId}: ${cause instanceof Error ? cause.message : String(cause)}`,
        ),
      );
    }
  }

  async query(q: MemoryQuery): Promise<Result<MemoryObject[], Error>> {
    // Tags are stored as plaintext (not encrypted) so they can be queried.
    // kind and since are also not encrypted. Pass the full query through.
    const result = await this.inner.query(q);
    if (!result.ok) return result;

    const decrypted: MemoryObject[] = [];
    for (const obj of result.value) {
      // If the data doesn't look like ciphertext, it's legacy unencrypted data
      if (!isCiphertext(obj.description) && !(obj.properties && "_enc" in obj.properties)) {
        decrypted.push(obj);
        continue;
      }
      try {
        decrypted.push(this.decryptObject(obj));
      } catch (cause) {
        return err(
          new Error(
            `TenantEncryptionAdapter: decryption failed for tenant ${this.tenantId} in query: ${cause instanceof Error ? cause.message : String(cause)}`,
          ),
        );
      }
    }

    return ok(decrypted);
  }

  async stats(): Promise<Result<MemoryStats, Error>> {
    return this.inner.stats();
  }

  writeRun?(run: Parameters<NonNullable<MemoryAdapter["writeRun"]>>[0]): void {
    this.inner.writeRun?.(run);
  }

  writeEmission?(emission: Parameters<NonNullable<MemoryAdapter["writeEmission"]>>[0]): void {
    this.inner.writeEmission?.(emission);
  }

  queryRuns?(limit?: number): Array<Record<string, unknown>> {
    return this.inner.queryRuns?.(limit) ?? [];
  }

  queryEmissionsForRun?(runId: string): Array<Record<string, unknown>> {
    return this.inner.queryEmissionsForRun?.(runId) ?? [];
  }

  writeRunAndEmissions?(
    run: Parameters<NonNullable<MemoryAdapter["writeRunAndEmissions"]>>[0],
    emissions: Parameters<NonNullable<MemoryAdapter["writeRunAndEmissions"]>>[1],
  ): void {
    this.inner.writeRunAndEmissions?.(run, emissions);
  }

  schemaVersion?(): number | null {
    return this.inner.schemaVersion?.() ?? null;
  }

  async persist(): Promise<void> {
    return this.inner.persist();
  }

  // ── Internal ────────────────────────────────────────────────────────────

  private decryptObject(obj: MemoryObject): MemoryObject {
    // Description is stored as ciphertext string — decrypt directly
    const description = this.keyProvider.decryptForTenant(this.tenantId, obj.description);

    // Properties are stored as { _enc: "v{version}:{iv}:{tag}:{ciphertext}" }
    // Decrypt the _enc field and parse the JSON back to the original object
    let properties: Record<string, unknown>;
    if (obj.properties && "_enc" in obj.properties && typeof obj.properties._enc === "string") {
      properties = JSON.parse(
        this.keyProvider.decryptForTenant(this.tenantId, obj.properties._enc),
      );
    } else {
      // Legacy unencrypted properties — pass through unchanged
      properties = obj.properties;
    }

    // Tags are stored as plaintext — no decryption needed
    return { ...obj, description, properties };
  }
}
