import { ok, err, type Result } from "@kirkforge/core-types";
import { timingSafeEqual } from "node:crypto";
import { AuthError } from "@kirkforge/core-errors";

// ── RBAC for KirkForge ───────────────────────────────────────────────────────
//
// Role-based access control with deny-by-default enforcement.
// Roles: Admin, Operator, Developer, Viewer
// Permissions: fine-grained actions scoped to resources
// Auth: OIDC JWT validation + static API key bearer token

// ── Roles ──────────────────────────────────────────────────────────────────

export type Role = "admin" | "operator" | "developer" | "viewer";

export const ROLES: Record<Role, string> = {
  admin: "admin",
  operator: "operator",
  developer: "developer",
  viewer: "viewer",
};

export const ROLE_DESCRIPTIONS: Record<Role, string> = {
  admin: "Full access: manage config, policy, tenants, keys, audit export",
  operator: "View dashboards, restart services, view health, view audit",
  developer: "Run verification workflows within their tenant",
  viewer: "Read-only access to status and results",
};

// ── Permissions ─────────────────────────────────────────────────────────────

export type Permission =
  // Admin permissions
  | "admin:config"
  | "admin:policy"
  | "admin:tenant"
  | "admin:keys"
  | "admin:audit_export"
  // Operator permissions
  | "operator:health"
  | "operator:restart"
  | "operator:view_audit"
  // Developer permissions
  | "dev:verify"
  | "dev:correct"
  | "dev:observe"
  | "dev:memory_read"
  | "dev:memory_write"
  // Viewer permissions
  | "viewer:status"
  | "viewer:results"
  | "viewer:metrics";

// ── Role → Permission mapping (deny-by-default: only listed perms are granted) ─

const ROLE_PERMISSIONS: Record<Role, Set<Permission>> = {
  admin: new Set<Permission>([
    "admin:config",
    "admin:policy",
    "admin:tenant",
    "admin:keys",
    "admin:audit_export",
    "operator:health",
    "operator:restart",
    "operator:view_audit",
    "dev:verify",
    "dev:correct",
    "dev:observe",
    "dev:memory_read",
    "dev:memory_write",
    "viewer:status",
    "viewer:results",
    "viewer:metrics",
  ]),
  operator: new Set<Permission>([
    "operator:health",
    "operator:restart",
    "operator:view_audit",
    "viewer:status",
    "viewer:results",
    "viewer:metrics",
  ]),
  developer: new Set<Permission>([
    "dev:verify",
    "dev:correct",
    "dev:observe",
    "dev:memory_read",
    "dev:memory_write",
    "viewer:status",
    "viewer:results",
    "viewer:metrics",
  ]),
  viewer: new Set<Permission>(["viewer:status", "viewer:results", "viewer:metrics"]),
};

// ── Actor context ──────────────────────────────────────────────────────────

export interface Actor {
  /** Actor identifier (user or service account). */
  id: string;
  /** Assigned role. */
  role: Role;
  /** Tenant scope. Empty string for platform-level actors. */
  tenantId: string;
  /** Auth method used to establish identity. */
  authMethod: "oidc" | "api_key" | "internal";
  /** Time at which the actor's credentials were verified. */
  verifiedAt: string;
}

// ── Auth audit hook ─────────────────────────────────────────────────────────

/**
 * Callback invoked when an authorization decision is made.
 * Allows the integration layer to emit audit events without core-rbac
 * depending on core-events directly.
 */
export interface AuthDecision {
  /** Whether access was granted. */
  granted: boolean;
  /** Actor making the request. */
  actorId: string;
  /** Actor role. */
  role: Role;
  /** Permission requested. */
  permission: Permission;
  /** Target tenant (for tenant-scoped checks). */
  targetTenantId?: string;
  /** Actor's own tenant. */
  actorTenantId: string;
  /** Reason for deny (empty if granted). */
  reason: string;
  /** ISO timestamp. */
  timestamp: string;
}

export type AuthAuditHook = (decision: AuthDecision) => void;

// ── Authorization ───────────────────────────────────────────────────────────

/**
 * Check whether an actor has a specific permission.
 * Deny-by-default: if the role is unknown or permission is not listed, deny.
 */
export function hasPermission(actor: Actor, permission: Permission): boolean {
  const perms = ROLE_PERMISSIONS[actor.role];
  if (!perms) return false;
  return perms.has(permission);
}

/**
 * Authorize an actor for a specific permission. Returns ok if granted,
 * err(AuthError) if denied. Optionally records the decision via audit hook.
 */
export function authorize(
  actor: Actor,
  permission: Permission,
  auditHook?: AuthAuditHook,
): Result<void, AuthError> {
  if (hasPermission(actor, permission)) {
    auditHook?.({
      granted: true,
      actorId: actor.id,
      role: actor.role,
      permission,
      actorTenantId: actor.tenantId,
      reason: "",
      timestamp: new Date().toISOString(),
    });
    return ok(undefined);
  }
  const reason = `Actor "${actor.id}" (role=${actor.role}) does not have permission "${permission}"`;
  auditHook?.({
    granted: false,
    actorId: actor.id,
    role: actor.role,
    permission,
    actorTenantId: actor.tenantId,
    reason,
    timestamp: new Date().toISOString(),
  });
  return err(
    new AuthError("FORBIDDEN", reason, { actorId: actor.id, role: actor.role, permission }),
  );
}

/**
 * Authorize an actor for a permission scoped to a tenant.
 * Cross-tenant access is always denied unless the actor has admin role.
 * Optionally records the decision via audit hook.
 */
export function authorizeTenant(
  actor: Actor,
  permission: Permission,
  targetTenantId: string,
  auditHook?: AuthAuditHook,
): Result<void, AuthError> {
  // Admin can cross tenant boundaries
  if (actor.role !== "admin" && actor.tenantId !== targetTenantId) {
    const reason = `Actor "${actor.id}" (role=${actor.role}, tenant=${actor.tenantId}) cannot access tenant "${targetTenantId}"`;
    auditHook?.({
      granted: false,
      actorId: actor.id,
      role: actor.role,
      permission,
      targetTenantId,
      actorTenantId: actor.tenantId,
      reason,
      timestamp: new Date().toISOString(),
    });
    return err(
      new AuthError("FORBIDDEN", reason, {
        actorId: actor.id,
        role: actor.role,
        tenantId: actor.tenantId,
        targetTenantId,
        permission,
      }),
    );
  }
  // Delegate to authorize for permission check, wrapping the hook with tenant context
  const tenantHook: AuthAuditHook | undefined = auditHook
    ? (decision) => {
        auditHook({ ...decision, targetTenantId });
      }
    : undefined;
  return authorize(actor, permission, tenantHook);
}

// ── Role resolution from groups/claims ──────────────────────────────────────

/**
 * Map a set of group names or OIDC claims to a role.
 * Uses explicit mapping table; falls back to viewer if no match.
 */
export interface GroupRoleMapping {
  /** Group name or claim value → role. */
  [group: string]: Role;
}

const DEFAULT_GROUP_ROLE_MAPPING: GroupRoleMapping = {
  "kirkforge-admins": "admin",
  "kirkforge-operators": "operator",
  "kirkforge-developers": "developer",
  "kirkforge-viewers": "viewer",
  admins: "admin",
  operators: "operator",
  developers: "developer",
  viewers: "viewer",
};

export function resolveRole(groups: string[], mapping?: GroupRoleMapping): Role {
  const m = mapping ?? DEFAULT_GROUP_ROLE_MAPPING;
  // Highest-privilege role wins (admin > operator > developer > viewer)
  const priority: Role[] = ["admin", "operator", "developer", "viewer"];
  const matchedRoles: Role[] = [];
  for (const group of groups) {
    const role = m[group];
    if (role) matchedRoles.push(role);
  }
  // Deduplicate
  const unique = [...new Set(matchedRoles)];
  // Return highest priority
  for (const p of priority) {
    if (unique.includes(p)) return p;
  }
  return "viewer";
}

// ── OIDC JWT validation ────────────────────────────────────────────────────

export interface OidcConfig {
  /** OIDC issuer URL (e.g. https://auth.example.com/realms/myorg). */
  issuer: string;
  /** Expected audience. */
  audience: string;
  /** JWKS URI for key fetching. Optional — auto-discovered from issuer .well-known. */
  jwksUri?: string;
  /** Clock skew tolerance in seconds. Default: 30. */
  clockSkewSec?: number;
}

export interface JwtClaims {
  sub: string;
  iss: string;
  aud: string | string[];
  exp: number;
  iat: number;
  roles?: string[];
  groups?: string[];
  tenant?: string;
  scope?: string;
  [key: string]: unknown;
}

/**
 * Validate JWT claims (not the signature — that requires jose/jwks which is a
 * production dependency). This function checks:
 *   - issuer matches config
 *   - audience matches config
 *   - token is not expired (with clock skew tolerance)
 *   - issued-at is not in the future
 *
 * Signature verification MUST be done by the caller using their JWT library
 * of choice (jose, jsonwebtoken, etc.) before calling this function.
 */
export function validateJwtClaims(
  claims: JwtClaims,
  config: OidcConfig,
  nowMs?: number,
): Result<JwtClaims, AuthError> {
  const now = nowMs ?? Date.now();
  const clockSkewMs = (config.clockSkewSec ?? 30) * 1000;

  // Issuer check
  if (claims.iss !== config.issuer) {
    return err(
      new AuthError(
        "INVALID_TOKEN",
        `JWT issuer mismatch: expected "${config.issuer}", got "${claims.iss}"`,
        {
          expected: config.issuer,
          actual: claims.iss,
        },
      ),
    );
  }

  // Audience check
  const audiences = Array.isArray(claims.aud) ? claims.aud : [claims.aud];
  if (!audiences.includes(config.audience)) {
    return err(
      new AuthError("INVALID_TOKEN", `JWT audience mismatch: expected "${config.audience}"`, {
        expected: config.audience,
        actual: claims.aud,
      }),
    );
  }

  // Expiry check
  if (claims.exp * 1000 < now - clockSkewMs) {
    return err(
      new AuthError("INVALID_TOKEN", "JWT token expired", {
        exp: new Date(claims.exp * 1000).toISOString(),
        now: new Date(now).toISOString(),
      }),
    );
  }

  // Issued-at future check
  if (claims.iat * 1000 > now + clockSkewMs) {
    return err(
      new AuthError("INVALID_TOKEN", "JWT issued-at is in the future", {
        iat: new Date(claims.iat * 1000).toISOString(),
        now: new Date(now).toISOString(),
      }),
    );
  }

  return ok(claims);
}

/**
 * Extract an Actor from validated JWT claims + OIDC config.
 */
export function actorFromJwt(
  claims: JwtClaims,
  config: OidcConfig,
  groupMapping?: GroupRoleMapping,
): Result<Actor, AuthError> {
  const role = resolveRole(claims.groups ?? claims.roles ?? [], groupMapping);
  const actor: Actor = {
    id: claims.sub,
    role,
    tenantId: claims.tenant ?? "",
    authMethod: "oidc",
    verifiedAt: new Date().toISOString(),
  };
  return ok(actor);
}

// ── Static API key auth ────────────────────────────────────────────────────

/**
 * Validate a bearer token against a static API key using timing-safe comparison.
 * Returns an Actor with the specified role, or an auth error.
 */
export function actorFromApiKey(
  token: string,
  expectedApiKey: string,
  role: Role = "operator",
  tenantId: string = "",
): Result<Actor, AuthError> {
  if (!token || !expectedApiKey) {
    return err(new AuthError("UNAUTHORIZED", "Missing token or API key", {}));
  }

  // Timing-safe comparison: pad both buffers to equal length so the
  // length check does not leak key length via timing.
  const tokenBuf = Buffer.from(token, "utf-8");
  const keyBuf = Buffer.from(expectedApiKey, "utf-8");
  const maxLen = Math.max(tokenBuf.length, keyBuf.length);
  const paddedToken = Buffer.alloc(maxLen);
  const paddedKey = Buffer.alloc(maxLen);
  tokenBuf.copy(paddedToken, maxLen - tokenBuf.length);
  keyBuf.copy(paddedKey, maxLen - keyBuf.length);
  if (!timingSafeEqual(paddedToken, paddedKey)) {
    return err(new AuthError("INVALID_TOKEN", "Invalid token", {}));
  }

  return ok({
    id: `api-key:${role}`,
    role,
    tenantId,
    authMethod: "api_key",
    verifiedAt: new Date().toISOString(),
  });
}

// ── Full JWT verification (signature + JWKS) ──────────────────────────

export { verifyJwt, clearJwksCache } from "./jwt-verify.js";
export type { VerifyJwtOptions } from "./jwt-verify.js";

/** Lazy-load crypto to avoid circular imports at module level. */
