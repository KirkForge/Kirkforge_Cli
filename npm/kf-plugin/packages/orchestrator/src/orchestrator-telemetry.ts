import type { Actor } from "@kirkforge/core-rbac";
import type { OrchestratorStats, HealthCheckResult } from "./types.js";
import type { SloReport } from "./slo-monitor.js";
import type { OrchestratorInternals } from "./orchestrator-shared.js";

/**
 * Audit a policy-deny event and update SLO counters in one place. Used
 * whenever the policy engine rejects a model or tool call.
 */
export function auditPolicyDeny(
  s: OrchestratorInternals,
  action: "model.deny" | "tool.deny",
  reason: string,
  policyHash: string,
  actor?: Actor,
): void {
  s.authPolicySlo.record({
    timestamp: Date.now(),
    type: "policy.deny",
    actorId: actor?.id,
    tenantId: actor?.tenantId,
  });
  if (!s.auditLogger) return;
  s.auditLogger
    .record({
      action,
      outcome: "deny",
      actorId: actor?.id ?? "system",
      tenantId: actor?.tenantId ?? "",
      reason,
      policyHash,
    })
    .then((ok) => {
      s.authPolicySlo.record({
        timestamp: Date.now(),
        type: ok ? "audit.write.success" : "audit.write.failure",
      });
    })
    .catch(() => {
      s.authPolicySlo.record({
        timestamp: Date.now(),
        type: "audit.write.failure",
      });
    });
}

/** SLO snapshot from the memory-backed SloMonitor, or null if not configured. */
export function computeSlo(s: OrchestratorInternals): Promise<SloReport | null> {
  if (!s.sloMonitor) return Promise.resolve(null);
  return s.sloMonitor.compute();
}

/** Auth/policy/audit SLO report (always available). */
export function authPolicySloReport(s: OrchestratorInternals): SloReport {
  return s.authPolicySlo.compute();
}

/** Record an auth event for SLO monitoring. */
export function recordAuthEvent(
  s: OrchestratorInternals,
  type: "auth.success" | "auth.failure",
  actorId?: string,
  tenantId?: string,
): void {
  s.authPolicySlo.record({ timestamp: Date.now(), type, actorId, tenantId });
}

/** Record a policy-allow event for SLO monitoring. */
export function recordPolicyAllow(
  s: OrchestratorInternals,
  actorId?: string,
  tenantId?: string,
): void {
  s.authPolicySlo.record({ timestamp: Date.now(), type: "policy.allow", actorId, tenantId });
}

/** Health snapshot used by /healthz and the health CLI command. */
export function healthCheckResult(
  s: OrchestratorInternals,
  stats: OrchestratorStats,
): HealthCheckResult {
  return {
    status: s.shuttingDown ? "shutting_down" : ("healthy" as const),
    stats: { ...stats },
    eventBus: {
      running: s.sharedEventBus.running,
      inflight: s.sharedEventBus.inflightCount,
      bufferSize: s.sharedEventBus.getBufferSize(),
    },
    memory: s.memoryStore ? "connected" : "none",
    providers: Object.keys(s.modelConfig.providers).length,
  };
}
