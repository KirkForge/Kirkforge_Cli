import type { IncomingMessage, ServerResponse } from "node:http";
import type { OidcConfig } from "@kirkforge/core-rbac";
import type { AuditLogger } from "@kirkforge/core-events";
import type { PolicyEngine } from "@kirkforge/core-policy";
import type { Logger } from "@kirkforge/core-logging";
import type { Actor, GroupRoleMapping } from "@kirkforge/core-rbac";

/** Public configuration for `HealthServer`. */
export interface HealthServerConfig {
  port?: number;
  host?: string;
  logger?: Logger;
  /** API key for Bearer token auth. Also read from HEALTH_API_KEY env var. */
  apiKey?: string;
  /** Max requests per second per IP (simple rate limiter). Default: 20. */
  rateLimitPerSec?: number;
  /** OIDC configuration for JWT validation. If set, Bearer tokens are validated as JWTs. */
  oidcConfig?: OidcConfig;
  /** Group-to-role mapping for OIDC JWT tokens. */
  groupRoleMapping?: GroupRoleMapping;
  /** Audit logger for auth/policy events. */
  auditLogger?: AuditLogger;
  /** Policy engine for endpoint-level checks. */
  policyEngine?: PolicyEngine;
  /** Request timeout in milliseconds. Default: 30000. */
  requestTimeoutMs?: number;
  /** Max request body size in bytes. Default: 1MB. */
  maxBodyBytes?: number;
  /** Graceful shutdown drain timeout in milliseconds. Default: 10000. */
  drainTimeoutMs?: number;
  /** Allowed CORS origin(s). Default: none (CORS disabled). Set to "*" for any, or specific origin. */
  corsOrigin?: string;
  /** Max requests per second per tenant (authenticated actors). Default: 0 (disabled — per-IP only). */
  rateLimitPerSecPerTenant?: number;
  /** Whether auth is required. Default: true in enterprise mode, false in dev mode. */
  requireAuth?: boolean;
  /** Whether to allow API key fallback when OIDC JWT validation fails.
   *  Default: false in enterprise mode, true in dev mode.
   *  A failed JWT should not silently fall through to API key auth. */
  allowApiKeyFallbackWithOidc?: boolean;
  /** TLS configuration. If set, the server uses HTTPS instead of HTTP. */
  tls?: {
    /** Path to TLS certificate file (PEM). */
    cert: string;
    /** Path to TLS private key file (PEM). */
    key: string;
  };
}

/** Token-bucket state for rate limiting. */
export interface RateBucket {
  tokens: number;
  lastRefill: number;
}

/** Tracks an in-flight request for graceful shutdown draining. */
export interface InFlightRequest {
  req: IncomingMessage;
  res: ServerResponse;
  startedAt: number;
}

/**
 * State object that the helper modules under `health-server/` need to
 * read or mutate. The HealthServer class in health-server.ts satisfies
 * this shape; helper functions take a `HealthServerInternals` parameter
 * so they don't need to know the full class.
 */
export interface HealthServerInternals {
  // Config / auth
  apiKey: string | null;
  oidcConfig?: OidcConfig;
  groupRoleMapping?: GroupRoleMapping;
  auditLogger?: AuditLogger;
  policyEngine?: PolicyEngine;
  logger?: Logger;
  requireAuth: boolean;
  allowApiKeyFallbackWithOidc: boolean;

  // Rate-limit
  rateLimitPerSec: number;
  rateLimitPerSecPerTenant: number;
  buckets: Map<string, RateBucket>;
  tenantBuckets: Map<string, RateBucket>;

  // Counters (mutable)
  requestCount: number;
  authSuccessCount: number;
  authFailureCount: number;
  policyDenyCount: number;

  // Lifecycle
  inFlight: Map<IncomingMessage, InFlightRequest>;
  shuttingDown: boolean;
  drainResolve: (() => void) | null;
}

/** Result of a successful actor resolution. */
export interface ResolvedActor {
  actor: Actor;
  tokenId: string;
}

/** Permission requirements for the v1 endpoints. */
export const ENDPOINT_PERMISSIONS: Record<string, string> = {
  "/healthz": "operator:health",
  "/readyz": "operator:health",
  "/metrics": "viewer:metrics",
  "/metrics/json": "viewer:metrics",
  "/metrics/prometheus": "viewer:metrics",
  "/v1/healthz": "operator:health",
  "/v1/readyz": "operator:health",
  "/v1/metrics": "viewer:metrics",
  "/v1/metrics/json": "viewer:metrics",
  "/v1/policy": "admin:policy",
  "/v1/audit": "admin:audit_export",
  "/v1/tenants": "admin:tenant",
  "/v1/quotas": "admin:tenant",
  "/v1/openapi": "viewer:metrics",
};
