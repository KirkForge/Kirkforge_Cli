import { describe, it, expect } from "vitest";
import { MemoryAuditSink, AuditLogger } from "@kirkforge/core-events";
import { authorize, authorizeTenant, type Actor } from "@kirkforge/core-rbac";
import { createAuthAuditHook } from "../src/auth-audit-bridge.js";

describe("createAuthAuditHook", () => {
  function makeAudit() {
    const sink = new MemoryAuditSink();
    const logger = new AuditLogger(sink);
    return { sink, logger };
  }

  const viewer: Actor = {
    id: "user-1",
    role: "viewer",
    tenantId: "t1",
    authMethod: "oidc",
    verifiedAt: new Date().toISOString(),
  };

  const admin: Actor = {
    id: "admin-1",
    role: "admin",
    tenantId: "t0",
    authMethod: "oidc",
    verifiedAt: new Date().toISOString(),
  };

  it("records auth.success on grant", async () => {
    const { sink, logger } = makeAudit();
    const hook = createAuthAuditHook(logger, "default-tenant");
    const result = authorize(viewer, "viewer:status", hook);
    expect(result.ok).toBe(true);
    await logger.flush();
    const events = sink.getEvents();
    const authEvent = events.find((e) => e.action === "auth.success");
    expect(authEvent).toBeDefined();
    expect(authEvent!.actorId).toBe("user-1");
    expect(authEvent!.outcome).toBe("success");
  });

  it("records auth.failure on deny", async () => {
    const { sink, logger } = makeAudit();
    const hook = createAuthAuditHook(logger, "default-tenant");
    const result = authorize(viewer, "admin:config", hook);
    expect(result.ok).toBe(false);
    await logger.flush();
    const events = sink.getEvents();
    const authEvent = events.find((e) => e.action === "auth.failure");
    expect(authEvent).toBeDefined();
    expect(authEvent!.actorId).toBe("user-1");
    expect(authEvent!.outcome).toBe("deny");
  });

  it("records tenant-scoped deny with authorizeTenant", async () => {
    const { sink, logger } = makeAudit();
    const hook = createAuthAuditHook(logger);
    const result = authorizeTenant({ ...viewer, tenantId: "t1" }, "dev:verify", "t2", hook);
    expect(result.ok).toBe(false);
    await logger.flush();
    const events = sink.getEvents();
    const authEvent = events.find((e) => e.action === "auth.failure");
    expect(authEvent).toBeDefined();
    expect(authEvent!.tenantId).toBe("t2");
  });

  it("uses default tenant when no targetTenantId", async () => {
    const { sink, logger } = makeAudit();
    const hook = createAuthAuditHook(logger, "default-tenant");
    authorize(admin, "admin:config", hook);
    await logger.flush();
    const events = sink.getEvents();
    const authEvent = events.find((e) => e.action === "auth.success");
    expect(authEvent).toBeDefined();
    expect(authEvent!.tenantId).toBe("t0"); // actorTenantId
  });
});
