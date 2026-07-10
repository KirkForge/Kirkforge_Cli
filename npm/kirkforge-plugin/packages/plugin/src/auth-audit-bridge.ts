import type { AuthDecision } from "@kirkforge/core-rbac";
import type { AuditLogger } from "@kirkforge/core-events";

/**
 * Bridge RBAC auth decisions to the audit logger.
 *
 * Usage:
 *   const audit = new AuditLogger(new MemoryAuditSink());
 *   const hook = createAuthAuditHook(audit, "tenant-123");
 *   authorize(actor, "admin:config", hook);
 *
 * This keeps core-rbac and core-events decoupled — the plugin layer
 * owns the wiring.
 */
export function createAuthAuditHook(
  audit: AuditLogger,
  defaultTenantId?: string,
): (decision: AuthDecision) => void {
  return (decision: AuthDecision) => {
    audit.record({
      action: decision.granted ? "auth.success" : "auth.failure",
      outcome: decision.granted ? "success" : "deny",
      actorId: decision.actorId,
      tenantId: decision.targetTenantId ?? decision.actorTenantId ?? defaultTenantId ?? "",
      reason: decision.reason || (decision.granted ? "Permission granted" : "Permission denied"),
      metadata: {
        role: decision.role,
        permission: decision.permission,
        actorTenantId: decision.actorTenantId,
      },
    });
  };
}
