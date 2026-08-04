import { jwtVerify, createRemoteJWKSet, createLocalJWKSet, type JWTVerifyGetKey } from "jose";

// Track JWKS instances for cache clearing in tests
const jwksInstances: ReturnType<typeof createRemoteJWKSet>[] = [];
import { ok, err, type Result } from "@kirkforge/core-types";
import { AuthError } from "@kirkforge/core-errors";
import type { OidcConfig, JwtClaims, GroupRoleMapping } from "./index.js";

// ── JOSE/JWKS JWT verification ─────────────────────────────────────────────
//
// Full signature verification using the jose library with JWKS key fetching.
// Supports RS256, RS384, RS512, ES256, ES384, ES512, PS256, PS384, PS512, EdDSA.
//
// JWKS keys are fetched from the issuer's .well-known/openid-configuration
// endpoint (or an explicit jwksUri in OidcConfig). The jose library handles
// key caching, rotation, and refresh internally via createRemoteJWKSet.

const ALLOWED_ALGORITHMS = [
  "RS256",
  "RS384",
  "RS512",
  "ES256",
  "ES384",
  "ES512",
  "PS256",
  "PS384",
  "PS512",
  "EdDSA",
];

// ── OIDC Discovery ─────────────────────────────────────────────────────────

/**
 * Discover the JWKS URI from the issuer's .well-known/openid-configuration.
 * Falls back to {issuer}/.well-known/jwks.json if discovery fails.
 */
async function discoverJwksUri(issuer: string): Promise<string> {
  const url = issuer.replace(/\/$/, "") + "/.well-known/openid-configuration";
  try {
    const res = await fetch(url, {
      signal: AbortSignal.timeout(5000),
      headers: { Accept: "application/json" },
    });
    if (res.ok) {
      const doc = (await res.json()) as Record<string, unknown>;
      if (typeof doc.jwks_uri === "string") {
        return doc.jwks_uri;
      }
    }
  } catch {
    // Discovery failed — fall through to default
  }
  return issuer.replace(/\/$/, "") + "/.well-known/jwks.json";
}

// ── Full JWT Verification ──────────────────────────────────────────────────

export interface VerifyJwtOptions {
  /** Allowable clock skew in seconds. Default: 30. */
  clockSkewSec?: number;
  /** Required scopes (space-separated, all must be present). Optional. */
  requiredScopes?: string[];
  /** Custom JWKS URI override. If not set, auto-discovered from issuer. */
  jwksUri?: string;
  /** HTTP request timeout for JWKS fetch in ms. Default: 5000. */
  timeoutMs?: number;
  /** Local JWKS set for testing or pre-fetched keys. Bypasses JWKS fetch. */
  jwksSet?: { keys: Record<string, unknown>[] };
}

/**
 * Verify a JWT token's signature using JWKS, then validate claims.
 *
 * This is the full verification path:
 * 1. Resolve JWKS URI from OIDC discovery or explicit config
 * 2. Verify the JWT signature using jose (handles key rotation & caching)
 * 3. Validate issuer, audience, expiry (handled by jose)
 * 4. Map payload to JwtClaims with group-to-role resolution
 *
 * Returns the verified claims on success, or an AuthError on failure.
 */
export async function verifyJwt(
  token: string,
  config: OidcConfig,
  groupMapping?: GroupRoleMapping,
  options?: VerifyJwtOptions,
): Promise<Result<JwtClaims, AuthError>> {
  try {
    // ── Resolve JWKS URI ────────────────────────────────────────────────
    let getKey: JWTVerifyGetKey;
    if (options?.jwksSet) {
      // Use local JWKS set — bypasses network fetch entirely
      const localSet = createLocalJWKSet(options.jwksSet);
      getKey = localSet;
    } else {
      const jwksUri = options?.jwksUri ?? config.jwksUri ?? (await discoverJwksUri(config.issuer));

      // ── Verify signature + claims with jose ──────────────────────────────
      // createRemoteJWKSet handles key caching, rotation, and refresh internally.
      const jwksUrl = new URL(jwksUri);
      const remoteSet = createRemoteJWKSet(jwksUrl, {
        timeoutDuration: options?.timeoutMs ?? 5000,
        cooldownDuration: 30_000, // 30s cooldown after failed fetches
      });
      jwksInstances.push(remoteSet);
      getKey = remoteSet;
    }

    const { payload } = await jwtVerify(token, getKey, {
      issuer: config.issuer,
      audience: config.audience,
      clockTolerance: options?.clockSkewSec ?? config.clockSkewSec ?? 30,
      algorithms: ALLOWED_ALGORITHMS,
    });

    // ── Map jose payload to our JwtClaims ────────────────────────────────
    const claims: JwtClaims = {
      sub: (payload.sub as string) ?? "",
      iss: (payload.iss as string) ?? "",
      aud: payload.aud ?? config.audience,
      exp: typeof payload.exp === "number" ? payload.exp : 0,
      iat: typeof payload.iat === "number" ? payload.iat : 0,
      roles: Array.isArray(payload.roles) ? (payload.roles as string[]) : undefined,
      groups: Array.isArray(payload.groups) ? (payload.groups as string[]) : undefined,
      tenant: typeof payload.tenant === "string" ? payload.tenant : undefined,
      scope: typeof payload.scope === "string" ? payload.scope : undefined,
    };

    // ── Optional scope check ─────────────────────────────────────────────
    if (options?.requiredScopes && options.requiredScopes.length > 0) {
      const tokenScopes = (claims.scope ?? "").split(" ").filter(Boolean);
      const missing = options.requiredScopes.filter((s) => !tokenScopes.includes(s));
      if (missing.length > 0) {
        return err(
          new AuthError("INVALID_TOKEN", `Missing required scopes: ${missing.join(", ")}`, {
            required: options.requiredScopes,
            actual: tokenScopes,
          }),
        );
      }
    }

    return ok(claims);
  } catch (e) {
    if (e instanceof AuthError) return err(e);
    const message = e instanceof Error ? e.message : "JWT verification failed";
    return err(new AuthError("INVALID_TOKEN", `JWT verification failed: ${message}`, {}));
  }
}

// ── Clear caches ───────────────────────────────────────────────────────────

/**
 * Clear internal jose JWKS caches. Useful for testing.
 * Note: jose's createRemoteJWKSet manages its own cache internally.
 */
export function clearJwksCache(): void {
  // Clear tracked JWKS instances. jose caches key sets per RemoteJWKSet instance;
  // removing references allows GC to collect stale instances between tests.
  jwksInstances.length = 0;
}
