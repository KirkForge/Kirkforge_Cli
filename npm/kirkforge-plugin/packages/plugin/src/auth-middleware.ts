import { ok, err, type Result } from "@kirkforge/core-types";
import {
  type Actor,
  type OidcConfig,
  type GroupRoleMapping,
  type Permission,
  type AuthDecision,
  type AuthAuditHook,
  authorize,
  authorizeTenant,
  actorFromApiKey,
  actorFromJwt,
  verifyJwt,
} from "@kirkforge/core-rbac";
import type { AuditLogger } from "@kirkforge/core-events";
import { isEnterpriseMode } from "@kirkforge/core-enterprise";

// ── Auth middleware for KirkForge services ─────────────────────────────────────
//
// Provides authentication and authorization middleware that can be wired into
// MCP handlers, HTTP servers, and CLI entrypoints. Handles:
//   - Bearer token extraction (JWT or API key)
//   - OIDC JWT signature verification via JWKS
//   - Claims validation (issuer, audience, expiry)
//   - Actor resolution (roles from group/claim mapping)
//   - RBAC permission checks with audit logging
//   - Tenant isolation enforcement
//
// Usage in an HTTP server:
//   const middleware = createAuthMiddleware({
//     oidcConfig: { issuer: "https://auth.example.com", audience: "kirkforge-api" },
//     apiKey: process.env.HEALTH_API_KEY,
//     auditLogger,
//   });
//
//   // On each request:
//   const authResult = await middleware.authenticate(request.headers.authorization);
//   if (!authResult.ok) return respond(401, authResult.error.message);
//   const rbacResult = middleware.checkPermission(authResult.value.actor, "dev:verify");
//   if (!rbacResult.ok) return respond(403, rbacResult.error.message);
//
// Usage in MCP server:
//   // Wrap tool handlers with auth + permission checks
//   const wrappedVerify = middleware.requirePermission("dev:verify", async (actor, args) => {
//     return verifyWorkspace(args);
//   });

// ── Types ──────────────────────────────────────────────────────────────────

export interface AuthMiddlewareConfig {
  /** OIDC configuration for JWT validation. Required for enterprise mode. */
  oidcConfig?: OidcConfig;
  /** Static API key for bearer token auth. */
  apiKey?: string;
  /** Group-to-role mapping for OIDC JWT tokens. */
  groupRoleMapping?: GroupRoleMapping;
  /** Audit logger for auth events. */
  auditLogger?: AuditLogger;
  /** Whether auth is required (true in enterprise mode). Default: false. */
  requireAuth?: boolean;
  /** Whether to allow API key fallback when OIDC JWT validation fails.
   *  Default: false in enterprise mode, true in dev mode.
   *  In enterprise mode, a failed JWT should not silently fall through to API key. */
  allowApiKeyFallbackWithOidc?: boolean;
}

export interface AuthenticatedRequest {
  /** Resolved actor identity. */
  actor: Actor;
  /** Auth method used. */
  authMethod: "oidc" | "api_key" | "internal";
  /** Raw token ID (subject or key ID). */
  tokenId: string;
}

export class AuthMiddlewareError extends Error {
  readonly statusCode: number;
  readonly authMethod: string;
  readonly reason: string;

  constructor(statusCode: number, authMethod: string, reason: string) {
    super(`Auth failed: ${reason}`);
    this.name = "AuthMiddlewareError";
    this.statusCode = statusCode;
    this.authMethod = authMethod;
    this.reason = reason;
  }
}

// ── Auth middleware ─────────────────────────────────────────────────────────

export class AuthMiddleware {
  private oidcConfig?: OidcConfig;
  private apiKey?: string;
  private groupRoleMapping?: GroupRoleMapping;
  private auditLogger?: AuditLogger;
  private requireAuth: boolean;
  private allowApiKeyFallbackWithOidc: boolean;

  // Counters for monitoring
  private _authSuccessCount = 0;
  private _authFailureCount = 0;
  private _permissionDenyCount = 0;

  constructor(config: AuthMiddlewareConfig = {}) {
    this.oidcConfig = config.oidcConfig;
    this.apiKey = config.apiKey;
    this.groupRoleMapping = config.groupRoleMapping;
    this.auditLogger = config.auditLogger;
    this.requireAuth = config.requireAuth ?? false;
    this.allowApiKeyFallbackWithOidc = config.allowApiKeyFallbackWithOidc ?? false;
  }

  // ── Authentication ─────────────────────────────────────────────────────

  /**
   * Authenticate a request from a Bearer authorization header.
   * Returns the authenticated actor or an error.
   *
   * Resolution order:
   *   1. No auth configured → internal actor (dev mode)
   *   2. OIDC JWT validation (if configured)
   *   3. API key validation (if configured)
   */
  async authenticate(
    authorizationHeader: string,
  ): Promise<Result<AuthenticatedRequest, AuthMiddlewareError>> {
    // No auth configured → internal actor only in dev mode
    // In enterprise/requireAuth mode, deny access rather than grant admin
    if (!this.apiKey && !this.oidcConfig) {
      if (this.requireAuth) {
        const error = new AuthMiddlewareError(
          500,
          "none",
          "Auth is required but no provider is configured",
        );
        this._recordAuthFailure("none", "auth_required_but_not_configured");
        return err(error);
      }
      return ok({
        actor: {
          id: "internal",
          role: "admin",
          tenantId: "",
          authMethod: "internal",
          verifiedAt: new Date().toISOString(),
        },
        authMethod: "internal",
        tokenId: "internal",
      });
    }

    // Extract Bearer token
    if (!authorizationHeader.startsWith("Bearer ")) {
      const error = new AuthMiddlewareError(401, "none", "Missing Bearer token");
      this._recordAuthFailure("none", "missing_token");
      return err(error);
    }

    const token = authorizationHeader.slice(7);

    // Try OIDC JWT validation first
    if (this.oidcConfig) {
      const jwtResult = await this._validateJwtBearer(token);
      if (jwtResult) {
        this._recordAuthSuccess(jwtResult.actor.id, jwtResult.actor.tenantId, "oidc");
        return ok({
          actor: jwtResult.actor,
          authMethod: "oidc",
          tokenId: jwtResult.actor.id,
        });
      }
      // JWT failed — fall through to API key only if explicitly allowed
      if (!this.allowApiKeyFallbackWithOidc) {
        const error = new AuthMiddlewareError(
          401,
          "oidc",
          "JWT validation failed and API key fallback is not enabled",
        );
        this._recordAuthFailure("oidc", "jwt_failed_no_fallback");
        return err(error);
      }
    }

    // Try API key auth
    if (this.apiKey) {
      const result = actorFromApiKey(token, this.apiKey);
      if (result.ok) {
        this._recordAuthSuccess(result.value.id, result.value.tenantId, "api_key");
        return ok({
          actor: result.value,
          authMethod: "api_key",
          tokenId: result.value.id,
        });
      }
      const error = new AuthMiddlewareError(401, "api_key", "Invalid API key");
      this._recordAuthFailure("api_key", "invalid_key");
      return err(error);
    }

    // OIDC configured but JWT failed, no API key fallback
    const error = new AuthMiddlewareError(401, "oidc", "Invalid JWT token");
    this._recordAuthFailure("oidc", "invalid_jwt");
    return err(error);
  }

  // ── Authorization ──────────────────────────────────────────────────────

  /**
   * Check if the actor has a specific permission.
   * Returns ok() if allowed, err with denial reason if not.
   * Audit-logs the decision.
   */
  checkPermission(actor: Actor, permission: Permission): Result<void, AuthMiddlewareError> {
    const auditHook = this.auditLogger ? this._createAuditHook(actor) : undefined;

    const result = authorize(actor, permission, auditHook);
    if (!result.ok) {
      this._permissionDenyCount++;
      return err(new AuthMiddlewareError(403, actor.authMethod, result.error.message));
    }
    return ok(undefined);
  }

  /**
   * Check if the actor can access a tenant's resources.
   * Returns ok() if allowed, err with denial reason if not.
   */
  checkTenantAccess(actor: Actor, targetTenantId: string): Result<void, AuthMiddlewareError> {
    const auditHook = this.auditLogger ? this._createAuditHook(actor) : undefined;

    const result = authorizeTenant(actor, "dev:verify", targetTenantId, auditHook);
    if (!result.ok) {
      this._permissionDenyCount++;
      return err(new AuthMiddlewareError(403, actor.authMethod, result.error.message));
    }
    return ok(undefined);
  }

  /**
   * Wrap a tool handler with auth + permission check.
   * Returns a function that authenticates, checks permission, then calls the handler.
   */
  requirePermission<T>(
    permission: Permission,
    handler: (actor: Actor, args: T) => Promise<Result<unknown, Error>>,
  ): (
    authorizationHeader: string,
    args: T,
  ) => Promise<Result<unknown, Error | AuthMiddlewareError>> {
    return async (authorizationHeader: string, args: T) => {
      const authResult = await this.authenticate(authorizationHeader);
      if (!authResult.ok) return err(authResult.error);

      const permResult = this.checkPermission(authResult.value.actor, permission);
      if (!permResult.ok) return err(permResult.error);

      return handler(authResult.value.actor, args);
    };
  }

  // ── Stats ──────────────────────────────────────────────────────────────

  get stats() {
    return {
      authSuccessCount: this._authSuccessCount,
      authFailureCount: this._authFailureCount,
      permissionDenyCount: this._permissionDenyCount,
    };
  }

  // ── Private helpers ────────────────────────────────────────────────────

  private async _validateJwtBearer(token: string): Promise<{ actor: Actor } | null> {
    if (!this.oidcConfig) return null;
    try {
      const claimsResult = await verifyJwt(token, this.oidcConfig, this.groupRoleMapping);
      if (!claimsResult.ok) return null;
      const actorResult = actorFromJwt(claimsResult.value, this.oidcConfig, this.groupRoleMapping);
      if (!actorResult.ok) return null;
      return { actor: actorResult.value };
    } catch {
      // Signature verification failed — deny. No fallback to unsigned claims.
      // Allowing unsigned claims would defeat the purpose of JWKS verification.
      return null;
    }
  }

  private _createAuditHook(actor: Actor): AuthAuditHook {
    return (decision: AuthDecision) => {
      this.auditLogger?.record({
        action: decision.granted ? "auth.success" : "auth.failure",
        outcome: decision.granted ? "success" : "deny",
        actorId: decision.actorId,
        tenantId: decision.targetTenantId ?? decision.actorTenantId ?? actor.tenantId ?? "",
        reason: decision.reason || (decision.granted ? "Permission granted" : "Permission denied"),
        metadata: {
          role: decision.role,
          permission: decision.permission,
          authMethod: actor.authMethod,
        },
      });
    };
  }

  private _recordAuthSuccess(actorId: string, tenantId: string, method: string): void {
    this._authSuccessCount++;
    this.auditLogger?.record({
      action: "auth.success",
      outcome: "success",
      actorId,
      tenantId,
      reason: `${method} authentication successful`,
      metadata: { authMethod: method },
    });
  }

  private _recordAuthFailure(method: string, reason: string): void {
    this._authFailureCount++;
    this.auditLogger?.record({
      action: "auth.failure",
      outcome: "deny",
      actorId: "unknown",
      tenantId: "",
      reason: `${method} authentication failed: ${reason}`,
      metadata: { authMethod: method, failureReason: reason },
    });
  }
}

// ── Factory ────────────────────────────────────────────────────────────────

/**
 * Create an auth middleware from environment variables and config.
 * Convenience function for CLI/server startup.
 */
export function createAuthMiddleware(config?: AuthMiddlewareConfig): AuthMiddleware {
  return new AuthMiddleware({
    oidcConfig: config?.oidcConfig,
    apiKey: config?.apiKey ?? process.env.HEALTH_API_KEY ?? undefined,
    groupRoleMapping:
      config?.groupRoleMapping ?? parseGroupRoleMapping(process.env.OIDC_GROUP_ROLE_MAP),
    auditLogger: config?.auditLogger,
    requireAuth: config?.requireAuth ?? isEnterpriseMode(),
  });
}

/**
 * Parse OIDC_GROUP_ROLE_MAP from env.
 * Format: "admin:admins,operator:operators,developer:developers,viewer:viewers"
 */
const VALID_ROLES = new Set(["admin", "operator", "developer", "viewer"]);

export function parseGroupRoleMapping(envValue?: string): GroupRoleMapping | undefined {
  if (!envValue) return undefined;
  const mapping: GroupRoleMapping = {};
  for (const pair of envValue.split(",")) {
    const [role, group] = pair.trim().split(":");
    if (role && group) {
      const trimmedRole = role.trim();
      if (!VALID_ROLES.has(trimmedRole)) {
        throw new Error(
          `Invalid role "${trimmedRole}" in OIDC_GROUP_ROLE_MAP. Valid roles: admin, operator, developer, viewer`,
        );
      }
      mapping[group.trim()] = trimmedRole as import("@kirkforge/core-rbac").Role;
    }
  }
  return Object.keys(mapping).length > 0 ? mapping : undefined;
}
