import { createHash, createHmac } from "node:crypto";
import type { AuditEvent } from "./audit.js";

/**
 * Generate the initial chain hash (genesis hash).
 * When an HMAC key is provided, the chain uses HMAC-SHA256 for tamper-proofing,
 * preventing recomputation by anyone without the key. Without a key, the chain
 * uses plain SHA-256 and relies on WORM storage for tamper-evidence.
 */
export function initialHash(hmacKey?: string): string {
  if (hmacKey) {
    return createHmac("sha256", hmacKey).update("kirkforge-audit-genesis").digest("hex");
  }
  return createHash("sha256").update("kirkforge-audit-genesis").digest("hex");
}

export function chainHashOf(prevHash: string, event: AuditEvent, hmacKey?: string): string {
  // Full canonical payload: include outcome, reason, and metadata so that
  // tampering with any audit field breaks the chain. Previous versions
  // excluded outcome/reason/metadata, allowing a denied event to be
  // rewritten as "success" without breaking the hash.
  // Recursively sort keys at every depth so nested objects are included
  // in the integrity chain. The previous replacer-array approach only sorted
  // top-level keys and dropped all nested object contents.
  const metadataJson = canonicalJson(event.metadata ?? {});
  const payload = `${prevHash}|${event.action}|${event.outcome}|${event.actorId}|${event.tenantId}|${event.reason}|${event.timestamp}|${event.sequence}|${metadataJson}`;
  if (hmacKey) {
    return createHmac("sha256", hmacKey).update(payload, "utf-8").digest("hex");
  }
  return createHash("sha256").update(payload, "utf-8").digest("hex");
}

/** Recursively sort object keys and stringify, producing a deterministic
 *  JSON representation that includes all nested values. Guards against
 *  circular references and excessive depth to avoid stack overflow. */
function canonicalJson(obj: unknown, depth = 0): string {
  if (depth > 32) return '"<max-depth>"';
  if (obj === null || obj === undefined) return "null";
  if (typeof obj !== "object") return JSON.stringify(obj);
  if (Array.isArray(obj))
    return "[" + (obj as unknown[]).map((v) => canonicalJson(v, depth + 1)).join(",") + "]";
  const sorted = Object.keys(obj as Record<string, unknown>).sort();
  return (
    "{" +
    sorted
      .map(
        (k) =>
          JSON.stringify(k) + ":" + canonicalJson((obj as Record<string, unknown>)[k], depth + 1),
      )
      .join(",") +
    "}"
  );
}
