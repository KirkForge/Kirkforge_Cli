// ── Audit sink for KirkForge ──────────────────────────────────────────────────
//
// Provides an append-only audit log with tamper-evidence (chain hashes) and
// external sink adapters (file, HTTP/SIEM, syslog, WORM). In enterprise mode,
// the audit sink is mandatory and must not silently fall back to in-memory.
//
// Audit events are distinct from regular EventBus events — they carry actor
// context, tenant scope, and a decision/action classification.
//
// This file is a thin barrel: types live here, implementations live in
// `audit-sinks/*.ts` and `audit-chain-hash.ts`.

// ── Types ──────────────────────────────────────────────────────────────────

export type AuditAction =
  | "auth.success"
  | "auth.failure"
  | "auth.token_refresh"
  | "policy.check"
  | "policy.deny"
  | "policy.change"
  | "tenant.create"
  | "tenant.evict"
  | "tenant.access"
  | "verify.start"
  | "verify.complete"
  | "correct.start"
  | "correct.complete"
  | "observe.record"
  | "observe.recall"
  | "memory.read"
  | "memory.write"
  | "memory.delete"
  | "secret.access"
  | "secret.resolve"
  | "config.change"
  | "tool.invoke"
  | "tool.deny"
  | "model.invoke"
  | "model.deny"
  | "system.startup"
  | "system.shutdown"
  | "serve.start"
  | "serve.shutdown"
  | "system.error";

export type AuditOutcome = "success" | "deny" | "error" | "skipped";

export interface AuditEvent {
  /** Unique event ID. */
  id: string;
  /** Sequential event number for this sink. */
  sequence: number;
  /** ISO timestamp. */
  timestamp: string;
  /** What happened. */
  action: AuditAction;
  /** Outcome of the action. */
  outcome: AuditOutcome;
  /** Actor who performed the action. */
  actorId: string;
  /** Tenant scope. */
  tenantId: string;
  /** Human-readable reason (especially important for deny). */
  reason: string;
  /** SHA-256 chain hash: hash(prevHash + thisEvent). */
  chainHash: string;
  /** Policy hash at time of event (if applicable). */
  policyHash?: string;
  /** Request/trace correlation ID. */
  traceId?: string;
  /** Additional context. */
  metadata?: Record<string, unknown>;
}

export interface AuditSink {
  /** Human-readable name for logging. */
  readonly name: string;
  /** Write an audit event. Must not throw; return false on failure. */
  write(event: AuditEvent): Promise<boolean>;
  /** Flush any buffered events. */
  flush(): Promise<boolean>;
  /** Close the sink and release resources. */
  close(): Promise<void>;
}

// ── Audit sink configuration ────────────────────────────────────────────────

export interface AuditSinkConfig {
  /** Type of sink: "file" | "http" | "memory". */
  type: "file" | "http" | "syslog" | "memory";
  /** File path for file sink. */
  filePath?: string;
  /** URL for HTTP sink. */
  httpUrl?: string;
  /** HTTP headers (e.g. Authorization). */
  httpHeaders?: Record<string, string>;

  /** Buffer size before forcing flush. Default: 100. */
  flushInterval?: number;
  /** Maximum file size in bytes before rotation (file sink only). Default: 50 MB. */
  maxFileSizeBytes?: number;
  /** Maximum rotated files to keep (file sink only). Default: 10. */
  maxRotatedFiles?: number;
  /** Syslog transport protocol. Default: "udp". Supports "tls" for RFC 5425. */
  syslogTransport?: "udp" | "tcp" | "tls";
  /** Syslog host. Default: "localhost". */
  syslogHost?: string;
  /** Syslog port. Default: 514 (6514 for TLS). */
  syslogPort?: number;
  /** Syslog facility code (0–23). Default: 1. */
  syslogFacility?: number;
  /** Syslog application name. Default: "kirkforge". */
  syslogAppName?: string;
  /** TLS options for syslog over TLS (RFC 5425). Used when syslogTransport is "tls". */
  syslogTls?: {
    /** Path to CA certificate for server verification. */
    ca?: string;
    /** Path to client certificate for mTLS. */
    cert?: string;
    /** Path to client private key for mTLS. */
    key?: string;
    /** Whether to reject unauthorized server certificates. Default: true. */
    rejectUnauthorized?: boolean;
    /** Server name for SNI. Default: syslogHost. */
    servername?: string;
  };
  /** HMAC key for chain integrity. When set, chain hashes use HMAC-SHA256.
   *  Also read from KIRKFORGE_AUDIT_KEY env var. */
  hmacKey?: string;
}

// ── Re-exports for the public surface ────────────────────────────────────────

export { FileAuditSink } from "./audit-sinks/file.js";
export type { FileAuditSinkConfig } from "./audit-sinks/file.js";
export { HttpAuditSink } from "./audit-sinks/http.js";
export { MemoryAuditSink } from "./audit-sinks/memory.js";
export type { SyslogAuditSinkConfig } from "./audit-sinks/syslog.js";
export { SyslogAuditSink } from "./audit-sinks/syslog.js";
export type { WormAuditSinkConfig } from "./audit-sinks/worm.js";
export { WormAuditSink } from "./audit-sinks/worm.js";
export { AuditLogger } from "./audit-logger.js";
export { createAuditSink } from "./audit-factory.js";
export { initialHash, chainHashOf } from "./audit-chain-hash.js";
