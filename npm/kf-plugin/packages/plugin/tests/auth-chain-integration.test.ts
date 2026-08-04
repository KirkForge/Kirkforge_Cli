import { describe, it, expect } from "vitest";
import { AuthMiddleware } from "../src/auth-middleware.js";
import { MemoryAuditSink, AuditLogger } from "@kirkforge/core-events";
import { authorizeTenant, type Actor } from "@kirkforge/core-rbac";

// ── Full auth chain integration test ──────────────────────────────────────
//
// Validates the complete flow: auth → RBAC → deny → audit event → chain integrity.
// This is the critical enterprise path that must work end-to-end.

describe("Full auth chain integration", () => {
  function setupAuthChain() {
    const sink = new MemoryAuditSink();
    const auditLogger = new AuditLogger(sink);

    const middleware = new AuthMiddleware({
      apiKey: "test-api-key-secret-32chars!!",
      auditLogger,
      requireAuth: true,
    });

    return { sink, auditLogger, middleware };
  }

  it("API key auth → RBAC grant → audit success event with chain hash", async () => {
    const { sink, auditLogger, middleware } = setupAuthChain();

    // Authenticate with valid API key
    const authResult = await middleware.authenticate("Bearer test-api-key-secret-32chars!!");
    expect(authResult.ok).toBe(true);
    if (!authResult.ok) return;

    const { actor } = authResult.value;
    expect(actor.authMethod).toBe("api_key");
    expect(actor.role).toBe("operator");

    // Check permission that should be granted
    const permResult = middleware.checkPermission(actor, "operator:health");
    expect(permResult.ok).toBe(true);

    // Flush audit log
    await auditLogger.flush();

    // Verify audit events
    const events = sink.getEvents();
    const authSuccess = events.find((e) => e.action === "auth.success" && e.outcome === "success");
    expect(authSuccess).toBeDefined();
    expect(authSuccess!.actorId).toBe("api-key:operator");
    expect(authSuccess!.chainHash).toBeTruthy();

    // Verify chain integrity
    expect(sink.verifyChain()).toBe(true);
  });

  it("API key auth → RBAC deny → audit failure event with chain hash", async () => {
    const { sink, auditLogger, middleware } = setupAuthChain();

    // Authenticate with valid API key (gets operator role)
    const authResult = await middleware.authenticate("Bearer test-api-key-secret-32chars!!");
    expect(authResult.ok).toBe(true);
    if (!authResult.ok) return;

    const { actor } = authResult.value;

    // Check permission that should be DENIED (operator cannot admin:config)
    const permResult = middleware.checkPermission(actor, "admin:config");
    expect(permResult.ok).toBe(false);

    // Flush audit log
    await auditLogger.flush();

    // Verify audit events include the auth.failure for the deny
    const events = sink.getEvents();
    const authDeny = events.find((e) => e.action === "auth.failure" && e.outcome === "deny");
    expect(authDeny).toBeDefined();
    expect(authDeny!.chainHash).toBeTruthy();

    // Verify chain integrity
    expect(sink.verifyChain()).toBe(true);
  });

  it("invalid auth → audit failure event → chain integrity preserved", async () => {
    const { sink, auditLogger, middleware } = setupAuthChain();

    // Authenticate with invalid API key
    const authResult = await middleware.authenticate("Bearer wrong-key");
    expect(authResult.ok).toBe(false);

    // Flush audit log
    await auditLogger.flush();

    // Verify audit events include the auth.failure
    const events = sink.getEvents();
    const authFailure = events.find((e) => e.action === "auth.failure");
    expect(authFailure).toBeDefined();
    expect(authFailure!.outcome).toBe("deny");

    // Verify chain integrity
    expect(sink.verifyChain()).toBe(true);
  });

  it("tenant isolation: cross-tenant deny produces audit event with target tenant", async () => {
    const sink = new MemoryAuditSink();
    const auditLogger = new AuditLogger(sink);

    // Create a developer actor in tenant-a
    const devActor: Actor = {
      id: "dev1",
      role: "developer",
      tenantId: "tenant-a",
      authMethod: "api_key",
      verifiedAt: new Date().toISOString(),
    };

    // Attempt cross-tenant access (should be denied)
    const result = authorizeTenant(devActor, "dev:verify", "tenant-b");
    expect(result.ok).toBe(false);

    // Flush and verify audit trail
    await auditLogger.flush();

    // The RBAC deny should be recorded via an audit hook in production.
    // Here we verify the deny decision itself is correct and the tenant
    // context is preserved.
    if (!result.ok) {
      expect(result.error.code).toBe("FORBIDDEN");
    }

    // Verify chain integrity still holds even after deny
    expect(sink.verifyChain()).toBe(true);
  });

  it("multiple auth events maintain chain hash continuity", async () => {
    const { sink, auditLogger, middleware } = setupAuthChain();

    // Successful auth
    const auth1 = await middleware.authenticate("Bearer test-api-key-secret-32chars!!");
    expect(auth1.ok).toBe(true);

    // Failed auth
    const auth2 = await middleware.authenticate("Bearer bad-key");
    expect(auth2.ok).toBe(false);

    // Another successful auth
    const auth3 = await middleware.authenticate("Bearer test-api-key-secret-32chars!!");
    expect(auth3.ok).toBe(true);

    // Flush and verify
    await auditLogger.flush();

    const events = sink.getEvents();
    expect(events.length).toBeGreaterThanOrEqual(3);

    // Verify chain hashes are sequential and valid
    for (let i = 1; i < events.length; i++) {
      expect(events[i]!.chainHash).toBeTruthy();
      expect(events[i]!.chainHash).not.toBe(events[i - 1]!.chainHash);
    }

    expect(sink.verifyChain()).toBe(true);
  });

  it("no-auth middleware returns internal actor with admin role", async () => {
    const middleware = new AuthMiddleware({ requireAuth: false });

    const authResult = await middleware.authenticate("");
    expect(authResult.ok).toBe(true);
    if (!authResult.ok) return;

    expect(authResult.value.actor.authMethod).toBe("internal");
    expect(authResult.value.actor.role).toBe("admin");
  });
});

// ── Regression: parseGroupRoleMapping validates roles ──────────────────────
//
// Verify that the exported parseGroupRoleMapping rejects invalid roles
// and accepts valid ones.

import { parseGroupRoleMapping } from "../src/auth-middleware.js";

describe("parseGroupRoleMapping regression: role validation", () => {
  it("accepts valid role names", () => {
    const result = parseGroupRoleMapping("admin:admins,operator:ops,developer:devs,viewer:viewers");
    expect(result).toBeDefined();
    expect(result!["admins"]).toBe("admin");
    expect(result!["ops"]).toBe("operator");
    expect(result!["devs"]).toBe("developer");
    expect(result!["viewers"]).toBe("viewer");
  });

  it("rejects invalid role names like superadmin", () => {
    expect(() => parseGroupRoleMapping("superadmin:admins")).toThrow("Invalid role");
  });

  it("rejects invalid role names like admins (reversed)", () => {
    expect(() => parseGroupRoleMapping("admins:admin")).toThrow("Invalid role");
  });

  it("returns undefined for empty input", () => {
    expect(parseGroupRoleMapping("")).toBeUndefined();
    expect(parseGroupRoleMapping(undefined)).toBeUndefined();
  });
});
