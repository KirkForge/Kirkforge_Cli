import type { AuditAction, AuditEvent, AuditOutcome, AuditSink } from "./audit.js";

/**
 * Top-level audit recorder. Wraps an `AuditSink` and assigns a sequence
 * number + ID to each event. Production code interacts with this; the
 * underlying sink is only swapped in tests.
 */
export class AuditLogger {
  private sink: AuditSink;
  private sequence = 0;

  constructor(sink: AuditSink) {
    this.sink = sink;
  }

  /** Record an audit event. */
  async record(params: {
    action: AuditAction;
    outcome: AuditOutcome;
    actorId: string;
    tenantId: string;
    reason: string;
    policyHash?: string;
    traceId?: string;
    metadata?: Record<string, unknown>;
  }): Promise<boolean> {
    const event: AuditEvent = {
      id: `audit-${Date.now()}-${this.sequence++}`,
      sequence: this.sequence,
      timestamp: new Date().toISOString(),
      action: params.action,
      outcome: params.outcome,
      actorId: params.actorId,
      tenantId: params.tenantId,
      reason: params.reason,
      chainHash: "", // will be computed by sink
      ...(params.policyHash ? { policyHash: params.policyHash } : {}),
      ...(params.traceId ? { traceId: params.traceId } : {}),
      ...(params.metadata ? { metadata: params.metadata } : {}),
    };
    return this.sink.write(event);
  }

  /** Flush pending events. */
  async flush(): Promise<boolean> {
    return this.sink.flush();
  }

  /** Close the audit logger and release resources. */
  async close(): Promise<void> {
    return this.sink.close();
  }

  /** Get the underlying sink (for testing). */
  getSink(): AuditSink {
    return this.sink;
  }
}
