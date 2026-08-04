import type { AuditEvent, AuditSink } from "../audit.js";
import { initialHash, chainHashOf } from "../audit-chain-hash.js";

/**
 * HTTP audit sink (SIEM integration). Buffers events and POSTs sealed
 * batches to a remote endpoint. On failure, the batch is re-buffered
 * for the next flush.
 */
export class HttpAuditSink implements AuditSink {
  readonly name = "http";
  private url: string;
  private headers: Record<string, string>;
  private buffer: AuditEvent[] = [];
  private flushSize: number;
  private lastHash: string;
  private hmacKey?: string;

  constructor(config: {
    url: string;
    headers?: Record<string, string>;
    flushInterval?: number;
    hmacKey?: string;
  }) {
    this.url = config.url;
    this.headers = { "Content-Type": "application/json", ...(config.headers ?? {}) };
    this.flushSize = config.flushInterval ?? 50;
    this.hmacKey = config.hmacKey ?? process.env["KIRKFORGE_AUDIT_KEY"];
    this.lastHash = initialHash(this.hmacKey);
  }

  async write(event: AuditEvent): Promise<boolean> {
    this.buffer.push(event);
    if (this.buffer.length >= this.flushSize) {
      return this.flush();
    }
    return true;
  }

  async flush(): Promise<boolean> {
    if (this.buffer.length === 0) return true;
    const events = this.buffer.splice(0);
    try {
      const sealed: AuditEvent[] = events.map((event) => {
        const chainHash = chainHashOf(this.lastHash, event, this.hmacKey);
        this.lastHash = chainHash;
        return { ...event, chainHash };
      });
      const res = await fetch(this.url, {
        method: "POST",
        headers: this.headers,
        body: JSON.stringify({ events: sealed, batch: true }),
        signal: AbortSignal.timeout(5000),
      });
      return res.ok;
    } catch (_e) {
      // Re-buffer for retry
      this.buffer.unshift(...events);
      return false;
    }
  }

  async close(): Promise<void> {
    await this.flush();
  }
}
