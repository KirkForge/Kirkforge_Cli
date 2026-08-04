import { describe, it, expect, beforeEach } from "vitest";
import { verifyJwt, validateJwtClaims, clearJwksCache } from "../src/index.js";
import { SignJWT, exportJWK, generateKeyPair } from "jose";
import type { OidcConfig, GroupRoleMapping } from "../src/index.js";

// Sequential execution to avoid JWKS cache contention and shared mutable state
// between concurrent tests. Each test generates its own keypair.
describe.sequential("verifyJwt", () => {
  const audience = "kirkforge";

  beforeEach(() => {
    clearJwksCache();
  });

  async function makeKeyPair() {
    return generateKeyPair("RS256", { extractable: true });
  }

  async function signToken(
    payload: Record<string, unknown>,
    keyPair: CryptoKeyPair,
    kid = "test-key-1",
  ): Promise<string> {
    return new SignJWT(payload).setProtectedHeader({ alg: "RS256", kid }).sign(keyPair.privateKey);
  }

  function localJwks(publicKeyJwk: Record<string, unknown>, kid = "test-key-1") {
    return { keys: [{ ...publicKeyJwk, kid, use: "sig" }] };
  }

  it("accepts a valid JWT with matching issuer and audience using local JWKS", async () => {
    const keyPair = await makeKeyPair();
    const publicKeyJwk = await exportJWK(keyPair.publicKey);
    const issuer = "https://auth.example.com";
    const now = Math.floor(Date.now() / 1000);
    const token = await signToken(
      {
        sub: "user-1",
        iss: issuer,
        aud: audience,
        exp: now + 3600,
        iat: now,
        groups: ["developers"],
      },
      keyPair,
    );

    const config: OidcConfig = { issuer, audience };
    const result = await verifyJwt(token, config, undefined, {
      jwksSet: localJwks(publicKeyJwk),
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.sub).toBe("user-1");
      expect(result.value.iss).toBe(issuer);
      expect(result.value.groups).toEqual(["developers"]);
    }
  });

  it("rejects a token with wrong issuer using local JWKS", async () => {
    const keyPair = await makeKeyPair();
    const publicKeyJwk = await exportJWK(keyPair.publicKey);
    const issuer = "https://auth.example.com";
    const now = Math.floor(Date.now() / 1000);
    const token = await signToken(
      {
        sub: "user-1",
        iss: "https://evil.com",
        aud: audience,
        exp: now + 3600,
        iat: now,
      },
      keyPair,
    );

    const config: OidcConfig = { issuer, audience };
    const result = await verifyJwt(token, config, undefined, {
      jwksSet: localJwks(publicKeyJwk),
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.code).toBe("INVALID_TOKEN");
    }
  });

  it("rejects a token with wrong audience using local JWKS", async () => {
    const keyPair = await makeKeyPair();
    const publicKeyJwk = await exportJWK(keyPair.publicKey);
    const issuer = "https://auth.example.com";
    const now = Math.floor(Date.now() / 1000);
    const token = await signToken(
      {
        sub: "user-1",
        iss: issuer,
        aud: "wrong-audience",
        exp: now + 3600,
        iat: now,
      },
      keyPair,
    );

    const config: OidcConfig = { issuer, audience };
    const result = await verifyJwt(token, config, undefined, {
      jwksSet: localJwks(publicKeyJwk),
    });
    expect(result.ok).toBe(false);
  });

  it("rejects an expired token using local JWKS", async () => {
    const keyPair = await makeKeyPair();
    const publicKeyJwk = await exportJWK(keyPair.publicKey);
    const issuer = "https://auth.example.com";
    const now = Math.floor(Date.now() / 1000);
    const token = await signToken(
      {
        sub: "user-1",
        iss: issuer,
        aud: audience,
        exp: now - 300, // expired 5 minutes ago
        iat: now - 3600,
      },
      keyPair,
    );

    const config: OidcConfig = { issuer, audience, clockSkewSec: 10 };
    const result = await verifyJwt(token, config, undefined, {
      jwksSet: localJwks(publicKeyJwk),
    });
    expect(result.ok).toBe(false);
  });

  it("resolves roles from group mapping", async () => {
    const keyPair = await makeKeyPair();
    const publicKeyJwk = await exportJWK(keyPair.publicKey);
    const issuer = "https://auth.example.com";
    const now = Math.floor(Date.now() / 1000);
    const token = await signToken(
      {
        sub: "admin-user",
        iss: issuer,
        aud: audience,
        exp: now + 3600,
        iat: now,
        groups: ["platform-admins"],
      },
      keyPair,
    );

    const config: OidcConfig = { issuer, audience };
    const mapping: GroupRoleMapping = { "platform-admins": "admin" };
    const result = await verifyJwt(token, config, mapping, {
      jwksSet: localJwks(publicKeyJwk),
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.groups).toEqual(["platform-admins"]);
    }
  });

  it("rejects token signed with wrong key using local JWKS", async () => {
    const keyPair = await makeKeyPair();
    const publicKeyJwk = await exportJWK(keyPair.publicKey);
    const wrongKeyPair = await makeKeyPair();
    const issuer = "https://auth.example.com";
    const now = Math.floor(Date.now() / 1000);
    const token = await signToken(
      {
        sub: "attacker",
        iss: issuer,
        aud: audience,
        exp: now + 3600,
        iat: now,
      },
      wrongKeyPair,
    );

    const config: OidcConfig = { issuer, audience };
    const result = await verifyJwt(token, config, undefined, {
      jwksSet: localJwks(publicKeyJwk),
    });
    expect(result.ok).toBe(false);
  });

  it("enforces required scopes using local JWKS", async () => {
    const keyPair = await makeKeyPair();
    const publicKeyJwk = await exportJWK(keyPair.publicKey);
    const issuer = "https://auth.example.com";
    const now = Math.floor(Date.now() / 1000);
    const token = await signToken(
      {
        sub: "user-1",
        iss: issuer,
        aud: audience,
        exp: now + 3600,
        iat: now,
        scope: "read write",
      },
      keyPair,
    );

    const config: OidcConfig = { issuer, audience };
    const jwksOpts = { jwksSet: localJwks(publicKeyJwk) };

    // Has required scope
    const resultOk = await verifyJwt(token, config, undefined, {
      ...jwksOpts,
      requiredScopes: ["read"],
    });
    expect(resultOk.ok).toBe(true);

    // Missing required scope
    const resultFail = await verifyJwt(token, config, undefined, {
      ...jwksOpts,
      requiredScopes: ["read", "admin"],
    });
    expect(resultFail.ok).toBe(false);
    if (!resultFail.ok) {
      expect(resultFail.error.message).toContain("Missing required scopes");
    }
  });

  it("returns INVALID_TOKEN when JWKS endpoint is unreachable", async () => {
    const keyPair = await makeKeyPair();
    const issuer = "https://auth-unreachable.example.com";
    const now = Math.floor(Date.now() / 1000);
    const token = await signToken(
      {
        sub: "user-1",
        iss: issuer,
        aud: audience,
        exp: now + 3600,
        iat: now,
      },
      keyPair,
    );

    const config: OidcConfig = { issuer, audience };
    const result = await verifyJwt(token, config);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.code).toBe("INVALID_TOKEN");
      expect(result.error.message).toContain("JWT verification failed");
    }
  });

  it("accepts ES256 tokens using local JWKS", async () => {
    const ecKeyPair = await generateKeyPair("ES256", { extractable: true });
    const ecPublicKeyJwk = await exportJWK(ecKeyPair.publicKey);

    const issuer = "https://auth.example.com";
    const now = Math.floor(Date.now() / 1000);
    const token = await new SignJWT({
      sub: "ec-user",
      iss: issuer,
      aud: audience,
      exp: now + 3600,
      iat: now,
    })
      .setProtectedHeader({ alg: "ES256", kid: "test-ec-key" })
      .sign(ecKeyPair.privateKey);

    const config: OidcConfig = { issuer, audience };
    const result = await verifyJwt(token, config, undefined, {
      jwksSet: { keys: [{ ...ecPublicKeyJwk, kid: "test-ec-key", use: "sig" }] },
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.sub).toBe("ec-user");
    }
  });
});

describe("validateJwtClaims", () => {
  it("accepts valid claims", () => {
    const now = Date.now();
    const claims = {
      sub: "user-1",
      iss: "https://auth.example.com",
      aud: "kirkforge",
      exp: Math.floor(now / 1000) + 3600,
      iat: Math.floor(now / 1000),
    };
    const config: OidcConfig = { issuer: "https://auth.example.com", audience: "kirkforge" };
    const result = validateJwtClaims(claims, config);
    expect(result.ok).toBe(true);
  });

  it("rejects expired claims", () => {
    const now = Date.now();
    const claims = {
      sub: "user-1",
      iss: "https://auth.example.com",
      aud: "kirkforge",
      exp: Math.floor(now / 1000) - 300,
      iat: Math.floor(now / 1000) - 3600,
    };
    const config: OidcConfig = { issuer: "https://auth.example.com", audience: "kirkforge" };
    const result = validateJwtClaims(claims, config);
    expect(result.ok).toBe(false);
  });

  it("rejects wrong issuer", () => {
    const now = Date.now();
    const claims = {
      sub: "user-1",
      iss: "https://evil.com",
      aud: "kirkforge",
      exp: Math.floor(now / 1000) + 3600,
      iat: Math.floor(now / 1000),
    };
    const config: OidcConfig = { issuer: "https://auth.example.com", audience: "kirkforge" };
    const result = validateJwtClaims(claims, config);
    expect(result.ok).toBe(false);
  });
});
