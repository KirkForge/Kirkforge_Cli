import type { AuditEvent, AuditSink } from "../audit.js";
import { initialHash, chainHashOf } from "../audit-chain-hash.js";

/**
 * In-memory audit sink. Useful for tests and short-lived dev runs.
 * NOT suitable for enterprise deployments — use File/Syslog/WORM instead.
 */
export class MemoryAuditSink implements AuditSink {
  readonly name = "memory";
  private events: AuditEvent[] = [];
  private lastHash: string;
  private hmacKey?: string;

  constructor(hmacKey?: string) {
    this.hmacKey = hmacKey ?? process.env["KIRKFORGE_AUDIT_KEY"];
    this.lastHash = initialHash(this.hmacKey);
  }

  async write(event: AuditEvent): Promise<boolean> {
    const chainHash = chainHashOf(this.lastHash, event, this.hmacKey);
    this.events.push({ ...event, chainHash });
    this.lastHash = chainHash;
    return true;
  }

  async flush(): Promise<boolean> {
    return true;
  }

  async close(): Promise<void> {
    // no-op
  }

  /** Get all stored events (for testing). */
  getEvents(): AuditEvent[] {
    return [...this.events];
  }

  /** Verify chain integrity (for testing). */
  verifyChain(): boolean {
    let prev = initialHash(this.hmacKey);
    for (const event of this.events) {
      const expected = chainHashOf(prev, event, this.hmacKey);
      if (event.chainHash !== expected) return false;
      prev = event.chainHash;
    }
    return true;
  }
}
