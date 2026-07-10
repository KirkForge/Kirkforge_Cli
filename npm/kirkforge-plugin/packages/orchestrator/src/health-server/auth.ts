import type { IncomingMessage, ServerResponse } from "node:http";
import { actorFromApiKey, actorFromJwt, authorize, verifyJwt } from "@kirkforge/core-rbac";
import type { Actor } from "@kirkforge/core-rbac";
import type { HealthServerInternals, ResolvedActor } from "../health-server-shared.js";
import { sendForbidden, sendUnauthorized } from "./response.js";

/** Audit a single auth success/failure event. */
export function auditAuth(
  s: HealthServerInternals,
  actorId: string,
  action: "auth.success" | "auth.failure",
  tenantId: string,
  reason: string,
): void {
  if (!s.auditLogger) return;
  s.auditLogger
    .record({
      action,
      outcome: action === "auth.success" ? "success" : "deny",
      actorId,
      tenantId,
      reason,
    })
    .catch(() => {
      // Audit write failure must not crash the server
    });
}

/**
 * Validate a JWT bearer token using full JOSE/JWKS signature verification.
 * Returns null on any verification failure (signature, claims, JWKS).
 * No claims-only fallback — unsigned tokens are never accepted.
 */
export async function validateJwtBearer(
  s: HealthServerInternals,
  token: string,
): Promise<{ actor: Actor } | null> {
  if (!s.oidcConfig) return null;
  try {
    const claimsResult = await verifyJwt(token, s.oidcConfig, s.groupRoleMapping);
    if (!claimsResult.ok) {
      s.logger?.warn(
        `[health-server] JWT verification failed: ${claimsResult.error.message}`,
      );
      return null;
    }
    const actorResult = actorFromJwt(claimsResult.value, s.oidcConfig, s.groupRoleMapping);
    if (!actorResult.ok) return null;
    return { actor: actorResult.value };
  } catch (e) {
    s.logger?.error(
      `[health-server] JWT JWKS verification failed, denying: ${e instanceof Error ? e.message : String(e)}`,
    );
    auditAuth(s, "unknown", "auth.failure", "", "JWT JWKS verification error — no fallback");
    s.authFailureCount++;
    return null;
  }
}

/**
 * Resolve the Actor from the request's Bearer token.
 * - If no API key is configured, requests pass with an internal actor.
 * - If API key is configured and OIDC is configured, try JWT first, then API key.
 * - If only API key is configured, use static key auth.
 * Returns null (and sends response) if auth fails.
 */
 
export async function resolveActor(
  s: HealthServerInternals & { orchestrator: any },
  req: IncomingMessage,
  res: ServerResponse,
): Promise<ResolvedActor | null> {
  // No auth configured — dev mode internal actor (NOT for enterprise/production)
  if (!s.apiKey && !s.oidcConfig) {
    if (s.requireAuth) {
      auditAuth(s, "none", "auth.failure", "", "No auth provider configured but auth is required");
      sendUnauthorized(res, "Auth is required but no provider is configured");
      return null;
    }
    return {
      actor: {
        id: "internal",
        role: "admin",
        tenantId: "",
        authMethod: "internal",
        verifiedAt: new Date().toISOString(),
      },
      tokenId: "internal",
    };
  }
  const authHeader = req.headers.authorization ?? "";
  if (!authHeader.startsWith("Bearer ")) {
    sendUnauthorized(res, "missing Bearer token");
    return null;
  }
  const token = authHeader.slice(7);
  // Try OIDC JWT validation first if configured
  if (s.oidcConfig) {
    const jwtResult = await validateJwtBearer(s, token);
    if (jwtResult) {
      auditAuth(s, jwtResult.actor.id, "auth.success", jwtResult.actor.tenantId, "JWT auth");
      s.authSuccessCount++;
      s.orchestrator.recordAuthEvent(
        "auth.success",
        jwtResult.actor.id,
        jwtResult.actor.tenantId,
      );
      return { actor: jwtResult.actor, tokenId: jwtResult.actor.id };
    }
    if (!s.allowApiKeyFallbackWithOidc) {
      auditAuth(
        s,
        "unknown",
        "auth.failure",
        "",
        "JWT validation failed; API key fallback disabled",
      );
      sendForbidden(res, "invalid JWT token; API key fallback disabled");
      s.authFailureCount++;
      s.orchestrator.recordAuthEvent("auth.failure");
      return null;
    }
  }
  // Static API key auth
  if (s.apiKey) {
    const result = actorFromApiKey(token, s.apiKey);
    if (result.ok) {
      auditAuth(s, result.value.id, "auth.success", result.value.tenantId, "API key auth");
      s.authSuccessCount++;
      s.orchestrator.recordAuthEvent("auth.success", result.value.id, result.value.tenantId);
      return { actor: result.value, tokenId: result.value.id };
    }
    auditAuth(s, "unknown", "auth.failure", "", "Invalid API key");
    s.authFailureCount++;
    s.orchestrator.recordAuthEvent("auth.failure");
    sendForbidden(res, "invalid API key");
    return null;
  }
  sendForbidden(res, "invalid JWT token");
  return null;
}

/** Remove query strings and trailing slashes for permission lookup. */
export function normalizeUrl(url: string): string {
  const path = url.split("?")[0]!.replace(/\/+$/, "") || "/";
  return path;
}

/**
 * Check if the actor has the required permission for the given endpoint URL.
 * If no permission is defined for the URL, deny by default in enterprise mode.
 */
 
export function checkPermission(
  s: HealthServerInternals & { orchestrator: any },
  actor: Actor,
  normalizedUrl: string,
  _tokenId: string,
  _req: IncomingMessage,
  res: ServerResponse,
  endpointPermissions: Record<string, string>,
): boolean {
  const required = endpointPermissions[normalizedUrl];
  if (!required) {
    if (normalizedUrl.startsWith("/v1/")) {
      auditAuth(
        s,
        actor.id,
        "auth.failure",
        actor.tenantId,
        `No RBAC permission mapping for ${normalizedUrl}`,
      );
      s.authFailureCount++;
      sendForbidden(res, `No RBAC permission mapping for ${normalizedUrl}`);
      return false;
    }
    return true;
  }
  const result = authorize(actor, required as Parameters<typeof authorize>[1]);
  if (!result.ok) {
    auditAuth(
      s,
      actor.id,
      "auth.failure",
      actor.tenantId,
      `RBAC deny: ${actor.role} lacks ${required} for ${normalizedUrl}`,
    );
    s.authFailureCount++;
    s.orchestrator.recordAuthEvent("auth.failure", actor.id, actor.tenantId);
    sendForbidden(res, result.error.message);
    return false;
  }
  return true;
}
