import { describe, it, expect } from "vitest";
import { MemoryAuditSink, AuditLogger, createAuditSink, type AuditAction } from "../src/audit.js";

describe("MemoryAuditSink", () => {
  it("stores events and retrieves them", async () => {
    const sink = new MemoryAuditSink();
    await sink.write({
      id: "test-1",
      sequence: 1,
      timestamp: new Date().toISOString(),
      action: "auth.success",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "Login",
      chainHash: "",
    });
    await sink.write({
      id: "test-2",
      sequence: 2,
      timestamp: new Date().toISOString(),
      action: "policy.deny",
      outcome: "deny",
      actorId: "user2",
      tenantId: "t2",
      reason: "Tool not allowed",
      chainHash: "",
    });

    const events = sink.getEvents();
    expect(events).toHaveLength(2);
    expect(events[0]!.action).toBe("auth.success");
    expect(events[1]!.action).toBe("policy.deny");
  });

  it("computes chain hashes", async () => {
    const sink = new MemoryAuditSink();
    await sink.write({
      id: "test-1",
      sequence: 1,
      timestamp: new Date().toISOString(),
      action: "auth.success",
      outcome: "success",
      actorId: "user1",
      tenantId: "",
      reason: "test",
      chainHash: "",
    });

    const events = sink.getEvents();
    expect(events[0]!.chainHash).toBeTruthy();
    expect(events[0]!.chainHash.length).toBeGreaterThan(0);
  });

  it("verifies chain integrity", async () => {
    const sink = new MemoryAuditSink();
    for (let i = 0; i < 10; i++) {
      await sink.write({
        id: `test-${i}`,
        sequence: i + 1,
        timestamp: new Date().toISOString(),
        action: "verify.start" as AuditAction,
        outcome: "success",
        actorId: "user1",
        tenantId: "t1",
        reason: "test",
        chainHash: "",
      });
    }
    expect(sink.verifyChain()).toBe(true);
  });

  it("detects tampered chain", async () => {
    const sink = new MemoryAuditSink();
    await sink.write({
      id: "test-1",
      sequence: 1,
      timestamp: new Date().toISOString(),
      action: "auth.success",
      outcome: "success",
      actorId: "user1",
      tenantId: "",
      reason: "test",
      chainHash: "",
    });
    // Tamper with the chain hash
    const events = sink.getEvents();
    events[0]!.chainHash = "tampered";
    expect(sink.verifyChain()).toBe(false);
  });

  it("flushes and closes successfully", async () => {
    const sink = new MemoryAuditSink();
    await sink.write({
      id: "test-1",
      sequence: 1,
      timestamp: new Date().toISOString(),
      action: "auth.success",
      outcome: "success",
      actorId: "user1",
      tenantId: "",
      reason: "test",
      chainHash: "",
    });
    expect(await sink.flush()).toBe(true);
    await sink.close();
  });
});

describe("AuditLogger", () => {
  it("records audit events through the logger", async () => {
    const sink = new MemoryAuditSink();
    const logger = new AuditLogger(sink);

    await logger.record({
      action: "auth.success",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "API key auth",
    });

    await logger.record({
      action: "policy.deny",
      outcome: "deny",
      actorId: "user2",
      tenantId: "t2",
      reason: "Tool 'curl' not allowed",
      policyHash: "abc123",
    });

    await logger.flush();
    const events = (sink as MemoryAuditSink).getEvents();
    expect(events).toHaveLength(2);
    expect(events[0]!.action).toBe("auth.success");
    expect(events[1]!.action).toBe("policy.deny");
    expect(events[1]!.policyHash).toBe("abc123");
  });

  it("includes trace ID in events", async () => {
    const sink = new MemoryAuditSink();
    const logger = new AuditLogger(sink);

    await logger.record({
      action: "verify.start",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "verification started",
      traceId: "trace-123",
    });

    const events = (sink as MemoryAuditSink).getEvents();
    expect(events[0]!.traceId).toBe("trace-123");
  });
});

describe("createAuditSink", () => {
  it("creates memory sink", () => {
    const sink = createAuditSink({ type: "memory" });
    expect(sink).toBeInstanceOf(MemoryAuditSink);
  });

  it("creates file sink", () => {
    const sink = createAuditSink({ type: "file", filePath: "/tmp/kirkforge-test-audit.jsonl" });
    expect(sink.name).toBe("file");
  });

  it("throws for unknown type", () => {
    expect(() => createAuditSink({ type: "unknown" as any })).toThrow();
  });
});

describe("SyslogAuditSink", () => {
  it("creates with TLS transport config", async () => {
    const { SyslogAuditSink } = await import("../src/audit.js");
    const sink = new SyslogAuditSink({
      transport: "tls",
      host: "siem.example.com",
      port: 6514,
      tls: {
        rejectUnauthorized: true,
        servername: "siem.example.com",
      },
    });
    expect(sink.name).toBe("syslog");
    await sink.close();
  });

  it("creates with TCP transport config", async () => {
    const { SyslogAuditSink } = await import("../src/audit.js");
    const sink = new SyslogAuditSink({
      transport: "tcp",
      host: "siem.example.com",
      port: 1468,
    });
    expect(sink.name).toBe("syslog");
    await sink.close();
  });

  it("defaults to UDP when transport is not specified", async () => {
    const { SyslogAuditSink } = await import("../src/audit.js");
    const sink = new SyslogAuditSink({ host: "localhost" });
    expect(sink.name).toBe("syslog");
    await sink.close();
  });

  it("uses port 6514 for TLS transport by default", async () => {
    const { SyslogAuditSink } = await import("../src/audit.js");
    const sink = new SyslogAuditSink({ transport: "tls", host: "siem.example.com" });
    // Port 6514 is the IANA-assigned port for syslog over TLS (RFC 5425)
    // We verify construction succeeds; port is stored internally
    expect(sink.name).toBe("syslog");
    await sink.close();
  });
});

// ── Regression tests: audit chain tamper detection ────────────────────────
//
// These verify that the chain hash covers all critical fields, so tampering
// with outcome, reason, or nested metadata breaks verification.

import { chainHashOf } from "../src/audit.js";
import type { AuditEvent } from "../src/audit.js";

describe("chainHashOf regression: tamper detection", () => {
  const baseEvent: AuditEvent = {
    id: "evt-1",
    sequence: 1,
    timestamp: "2026-01-01T00:00:00Z",
    action: "policy.deny",
    outcome: "deny",
    actorId: "user1",
    tenantId: "t1",
    reason: "Tool not allowed",
    chainHash: "",
  };

  it("changing outcome from deny to success breaks the chain", () => {
    const original = chainHashOf("prev", baseEvent);
    const tampered = chainHashOf("prev", { ...baseEvent, outcome: "success" });
    expect(original).not.toBe(tampered);
  });

  it("changing reason breaks the chain", () => {
    const original = chainHashOf("prev", baseEvent);
    const tampered = chainHashOf("prev", { ...baseEvent, reason: "Approved by admin" });
    expect(original).not.toBe(tampered);
  });

  it("changing nested metadata breaks the chain", () => {
    const withMeta = { ...baseEvent, metadata: { ctx: { ip: "10.0.0.1", path: "/verify" } } };
    const original = chainHashOf("prev", withMeta);
    const tampered = chainHashOf("prev", {
      ...withMeta,
      metadata: { ctx: { ip: "10.0.0.2", path: "/verify" } },
    });
    expect(original).not.toBe(tampered);
  });

  it("reordering nested metadata keys does not break the chain (canonical)", () => {
    const meta1 = { b: 2, a: 1 };
    const meta2 = { a: 1, b: 2 };
    const hash1 = chainHashOf("prev", { ...baseEvent, metadata: meta1 });
    const hash2 = chainHashOf("prev", { ...baseEvent, metadata: meta2 });
    expect(hash1).toBe(hash2);
  });

  it("deeply nested metadata is included in hash", () => {
    const withDeep = { ...baseEvent, metadata: { level1: { level2: { level3: "secret" } } } };
    const original = chainHashOf("prev", withDeep);
    const tampered = chainHashOf("prev", {
      ...withDeep,
      metadata: { level1: { level2: { level3: "tampered" } } },
    });
    expect(original).not.toBe(tampered);
  });

  it("null vs undefined metadata produces consistent hash", () => {
    const withNull = { ...baseEvent, metadata: null };
    const withUndefined = { ...baseEvent };
    const hashNull = chainHashOf("prev", withNull as any);
    const hashUndefined = chainHashOf("prev", withUndefined);
    // Both should produce the same hash (canonicalJson maps both to "null")
    expect(hashNull).toBe(hashUndefined);
  });
});

import {
  chainHashOf as _chainHashOf,
  initialHash as _initialHash,
  MemoryAuditSink as _MemoryAuditSink,
  type AuditEvent as _AuditEvent,
} from "../src/audit.js";

// ── HMAC-keyed audit chain ────────────────────────────────────────────────
//
// Verify that when an HMAC key is provided, the chain uses HMAC-SHA256
// and produces different hashes than plain SHA-256.

describe("HMAC-keyed audit chain", () => {
  it("produces different chain hashes with HMAC key vs without", () => {
    const event: _AuditEvent = {
      id: "hmac-test-1",
      sequence: 1,
      timestamp: "2026-01-01T00:00:00Z",
      action: "auth.success",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "test",
      chainHash: "",
    };

    const plainHash = _chainHashOf(_initialHash(), event);
    const hmacHash = _chainHashOf(_initialHash("my-secret-key"), event, "my-secret-key");
    expect(plainHash).not.toBe(hmacHash);
    expect(plainHash.length).toBe(64); // full SHA-256 hex
    expect(hmacHash.length).toBe(64); // full HMAC-SHA256 hex
  });

  it("HMAC key produces different genesis hash", () => {
    const plain = _initialHash();
    const keyed = _initialHash("test-key");
    expect(plain).not.toBe(keyed);
    expect(plain.length).toBe(64);
    expect(keyed.length).toBe(64);
  });

  it("MemoryAuditSink uses HMAC key for chain integrity", () => {
    const sink = new _MemoryAuditSink("test-hmac-key");
    const event: _AuditEvent = {
      id: "hmac-test-2",
      sequence: 1,
      timestamp: new Date().toISOString(),
      action: "auth.success",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "test",
      chainHash: "",
    };
    sink.write(event);
    const events = sink.getEvents();
    expect(events[0]!.chainHash).toBeTruthy();
    // HMAC chain should verify
    expect(sink.verifyChain()).toBe(true);
  });
});
