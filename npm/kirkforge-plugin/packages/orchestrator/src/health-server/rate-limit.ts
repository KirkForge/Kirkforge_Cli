import type { IncomingMessage, ServerResponse } from "node:http";
import type { Actor } from "@kirkforge/core-rbac";
import { KirkForgeError, toErrorResponse } from "@kirkforge/core-errors";
import type { HealthServerInternals } from "../health-server-shared.js";

/**
 * Per-IP and per-tenant token-bucket rate limiting. Per-tenant is applied
 * first when configured and the actor has a tenant ID; per-IP is always
 * applied when rateLimitPerSec > 0. Returns false (and sends 429) when
 * the limit is hit.
 */
export function checkRateLimit(
  s: HealthServerInternals,
  req: IncomingMessage,
  res: ServerResponse,
  actor?: Actor,
): boolean {
  // Per-tenant rate limiting
  if (s.rateLimitPerSecPerTenant > 0 && actor?.tenantId) {
    const tenantKey = `tenant:${actor.tenantId}`;
    const now = Date.now();
    let tBucket = s.tenantBuckets.get(tenantKey);
    if (!tBucket) {
      tBucket = { tokens: s.rateLimitPerSecPerTenant, lastRefill: now };
      s.tenantBuckets.set(tenantKey, tBucket);
    }
    const tElapsed = (now - tBucket.lastRefill) / 1000;
    tBucket.tokens = Math.min(
      s.rateLimitPerSecPerTenant,
      tBucket.tokens + tElapsed * s.rateLimitPerSecPerTenant,
    );
    tBucket.lastRefill = now;
    if (tBucket.tokens < 1) {
      res.writeHead(429, {
        "Content-Type": "application/json",
        "Retry-After": "1",
      });
      res.end(
        JSON.stringify(
          toErrorResponse(
            new KirkForgeError(
              "TENANT_RATE_LIMITED",
              `Tenant ${actor.tenantId} rate limit exceeded`,
            ),
            undefined,
          ),
        ),
      );
      return false;
    }
    tBucket.tokens -= 1;
  }
  // Per-IP rate limiting
  if (s.rateLimitPerSec <= 0) return true;
  const ip =
    (req.headers["x-forwarded-for"] as string)?.split(",")[0]?.trim() ??
    req.socket.remoteAddress ??
    "unknown";
  const now = Date.now();
  let bucket = s.buckets.get(ip);
  if (!bucket) {
    bucket = { tokens: s.rateLimitPerSec, lastRefill: now };
    s.buckets.set(ip, bucket);
  }
  const elapsed = (now - bucket.lastRefill) / 1000;
  bucket.tokens = Math.min(s.rateLimitPerSec, bucket.tokens + elapsed * s.rateLimitPerSec);
  bucket.lastRefill = now;
  if (bucket.tokens < 1) {
    res.writeHead(429, {
      "Content-Type": "application/json",
      "Retry-After": "1",
    });
    res.end(
      JSON.stringify(
        toErrorResponse(new KirkForgeError("RATE_LIMITED", "Too many requests"), undefined),
      ),
    );
    return false;
  }
  bucket.tokens -= 1;
  return true;
}

const SECURITY_HEADERS: Record<string, string> = {
  "Content-Security-Policy": "default-src 'none'; frame-ancestors 'none'",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
  "X-XSS-Protection": "0",
  "Referrer-Policy": "no-referrer",
  "Cache-Control": "no-store, no-cache, must-revalidate",
  "Strict-Transport-Security": "max-age=63072000; includeSubDomains; preload",
};

/**
 * Consume the request body stream and enforce byte-by-byte size limits.
 * Returns true if the body was consumed (or absent) within limits.
 * Returns false and sends 413 if the body exceeds the limit.
 */
export function consumeAndLimitBody(
  s: HealthServerInternals,
  maxBodyBytes: number,
  req: IncomingMessage,
  res: ServerResponse,
  correlationId: string,
): Promise<boolean> {
  return new Promise((resolve) => {
    // Fast-path: GET/HEAD typically have no body
    const method = req.method?.toUpperCase() ?? "GET";
    const transferEncoding = (req.headers["transfer-encoding"] ?? "").toLowerCase();
    const contentLength = parseInt(req.headers["content-length"] ?? "0", 10);
    const hasBody =
      method === "POST" ||
      method === "PUT" ||
      method === "PATCH" ||
      transferEncoding.includes("chunked") ||
      contentLength > 0;

    if (!hasBody) {
      resolve(true);
      return;
    }

    // Pre-check Content-Length if present
    if (contentLength > maxBodyBytes) {
      s.authFailureCount++;
      res.writeHead(413, { "Content-Type": "application/json", ...SECURITY_HEADERS });
      res.end(
        JSON.stringify({
          error: {
            code: "PAYLOAD_TOO_LARGE",
            message: `Request body exceeds maximum size of ${maxBodyBytes} bytes`,
            status: 413,
            requestId: correlationId,
            timestamp: new Date().toISOString(),
          },
        }),
      );
      req.destroy();
      resolve(false);
      return;
    }

    let received = 0;
    let exceeded = false;

    req.on("data", (chunk: Buffer) => {
      if (exceeded) return;
      received += chunk.length;
      if (received > maxBodyBytes) {
        exceeded = true;
        s.authFailureCount++;
        res.writeHead(413, { "Content-Type": "application/json", ...SECURITY_HEADERS });
        res.end(
          JSON.stringify({
            error: {
              code: "PAYLOAD_TOO_LARGE",
              message: `Request body exceeds maximum size of ${maxBodyBytes} bytes`,
              status: 413,
              requestId: correlationId,
              timestamp: new Date().toISOString(),
            },
          }),
        );
        req.destroy();
        resolve(false);
      }
    });

    req.on("end", () => {
      if (!exceeded) resolve(true);
    });

    req.on("error", () => {
      if (!exceeded) resolve(false);
    });
  });
}
