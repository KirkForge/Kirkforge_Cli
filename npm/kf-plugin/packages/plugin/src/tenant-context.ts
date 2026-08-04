import type { AuditLogger, AuditSink } from "@kirkforge/core-events";
import { TenantRegistry } from "@kirkforge/core-tenancy";
import type { QuotaManager } from "@kirkforge/core-enterprise";
import type { MemoryStore } from "@kirkforge/memory-palace";

// ── Tenant context ──────────────────────────────────────────────────────────
//
// Threading tenant context through the plugin and orchestrator layer.
// Every operation in a multi-tenant deployment should carry a TenantContext
// so that:
//   1. Memory stores are tenant-scoped (no cross-tenant leakage)
//   2. Audit events carry tenant IDs
//   3. Quota checks are per-tenant
//   4. Policy engine decisions are tenant-scoped
//
// This module provides the TenantContext container and factory helpers.

export interface TenantContext {
  /** Stable tenant ID (from TenantRegistry). */
  tenantId: string;
  /** Actor performing the operation. */
  actorId: string;
  /** Tenant-scoped memory store (isolated per tenant). */
  memoryStore: MemoryStore;
  /** Audit logger (tenant ID is automatically set on all events). */
  auditLogger: AuditLogger;
  /** Quota manager for rate limiting (per-tenant). */
  quotaManager?: QuotaManager;
}

/**
 * Create a tenant-scoped audit logger that automatically sets tenantId
 * and actorId on every audit event.
 */
export function createTenantAuditLogger(
  base: AuditLogger,
  tenantId: string,
  defaultActorId?: string,
): AuditLogger {
  // Wrap the base logger to inject tenant context into every audit record
  return {
    record: (event) =>
      base.record({
        ...event,
        tenantId: event.tenantId || tenantId,
        actorId: event.actorId || defaultActorId || "system",
      }),
    flush: () => base.flush(),
  } as AuditLogger;
}

/**
 * Initialize a full tenant context with isolated memory store,
 * tenant-scoped audit logger, and optional quota enforcement.
 *
 * Usage:
 *   const ctx = await createTenantContext({
 *     tenantId: "t-abc123",
 *     actorId: "user-456",
 *     auditSink: new FileAuditSink({ filePath: "/var/log/kirkforge/audit.jsonl" }),
 *     quotaManager: quotaManager,
 *   });
 *
 *   // Check quota before operation
 *   const quotaCheck = ctx.quotaManager?.checkQuota(ctx.tenantId, "verify_run");
 *
 *   // Use tenant-scoped memory store
 *   await ctx.memoryStore.writeTaskObservation({ ... });
 *
 *   // Audit events automatically include tenant context
 *   await ctx.auditLogger.record({ action: "verify.start", ... });
 */
export async function createTenantContext(
  config: CreateTenantContextConfig,
): Promise<TenantContext> {
  const { tenantId, actorId, auditSink, quotaManager } = config;

  // Create tenant registry and memory store
  const registry = new TenantRegistry();
  // Ensure tenant is registered
  registry.register(config.workspacePath ?? `/tmp/kirkforge/${tenantId}`);

  // Create tenant-scoped memory store
  const memoryResult = await registry.createMemoryStore(tenantId);
  if (!memoryResult.ok) {
    throw memoryResult.error;
  }
  const memoryStore = memoryResult.value;

  // Create tenant-scoped audit logger
  const { AuditLogger } = await import("@kirkforge/core-events");
  const baseLogger = new AuditLogger(auditSink);
  const auditLogger = createTenantAuditLogger(baseLogger, tenantId, actorId);

  return {
    tenantId,
    actorId: actorId ?? "system",
    memoryStore,
    auditLogger,
    quotaManager,
  };
}

export interface CreateTenantContextConfig {
  /** Tenant ID. */
  tenantId: string;
  /** Actor ID. */
  actorId?: string;
  /** Workspace path (used for tenant registry). */
  workspacePath?: string;
  /** Audit sink for audit logging. */
  auditSink: AuditSink;
  /** Optional quota manager for per-tenant rate limiting. */
  quotaManager?: QuotaManager;
}
