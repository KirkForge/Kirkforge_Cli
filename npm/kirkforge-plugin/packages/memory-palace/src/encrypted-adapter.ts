/**
 * Encrypted adapter wrapper for MemoryStore.
 * Wraps any MemoryAdapter and encrypts/decrypts MemoryObject payloads
 * using AES-256-GCM with a key from core-secrets.
 *
 * Enabled via feature flag: FEATURE_ENCRYPTION_AT_REST=true
 * or by passing an encryption key directly.
 */

import type { MemoryAdapter, MemoryObject, MemoryQuery, MemoryStats } from "./index.js";
import { ok, type Result } from "@kirkforge/core-types";
import { createCipheriv, createDecipheriv, randomBytes, createHash } from "node:crypto";

const ALGORITHM = "aes-256-gcm";
const IV_LENGTH = 12; // GCM recommended IV size

function deriveKey(rawKey: string): Buffer {
  return createHash("sha256").update(rawKey).digest(); // 32 bytes
}

function encrypt(plaintext: string, key: Buffer): string {
  const iv = randomBytes(IV_LENGTH);
  const cipher = createCipheriv(ALGORITHM, key, iv);
  const encrypted = Buffer.concat([cipher.update(plaintext, "utf8"), cipher.final()]);
  const tag = cipher.getAuthTag();
  // Format: iv:tag:ciphertext (all base64)
  return `${iv.toString("base64")}:${tag.toString("base64")}:${encrypted.toString("base64")}`;
}

function decrypt(ciphertext: string, key: Buffer): string {
  const parts = ciphertext.split(":");
  if (parts.length !== 3) throw new Error("Invalid encrypted payload format");
  const iv = Buffer.from(parts[0]!, "base64");
  const tag = Buffer.from(parts[1]!, "base64");
  const encrypted = Buffer.from(parts[2]!, "base64");
  const decipher = createDecipheriv(ALGORITHM, key, iv);
  decipher.setAuthTag(tag);
  const decrypted = Buffer.concat([decipher.update(encrypted), decipher.final()]);
  return decrypted.toString("utf8");
}

export class EncryptedAdapter implements MemoryAdapter {
  private key: Buffer;

  constructor(
    private inner: MemoryAdapter,
    encryptionKey: string,
  ) {
    this.key = deriveKey(encryptionKey);
  }

  async write(obj: MemoryObject): Promise<Result<void, Error>> {
    // Encrypt the properties and description fields
    const encrypted = {
      ...obj,
      description: encrypt(obj.description, this.key),
      properties: JSON.parse(encrypt(JSON.stringify(obj.properties), this.key)),
    };
    return this.inner.write(encrypted);
  }

  async read(id: string): Promise<Result<MemoryObject | null, Error>> {
    const result = await this.inner.read(id);
    if (!result.ok || !result.value) return result;

    try {
      const obj = result.value;
      return ok({
        ...obj,
        description: decrypt(obj.description, this.key),
        properties: JSON.parse(decrypt(JSON.stringify(obj.properties), this.key)),
      });
    } catch {
      // If decryption fails, return the raw object (might be unencrypted legacy data)
      return result;
    }
  }

  async query(q: MemoryQuery): Promise<Result<MemoryObject[], Error>> {
    const result = await this.inner.query(q);
    if (!result.ok) return result;

    const decrypted = result.value.map((obj) => {
      try {
        return {
          ...obj,
          description: decrypt(obj.description, this.key),
          properties: JSON.parse(decrypt(JSON.stringify(obj.properties), this.key)),
        };
      } catch {
        return obj; // Legacy unencrypted data
      }
    });

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
}
