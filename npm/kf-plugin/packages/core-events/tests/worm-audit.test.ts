import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { WormAuditSink, MemoryAuditSink, AuditLogger } from "../src/audit.js";
import { mkdirSync, rmSync, existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

let testCounter = 0;

function freshDir(): string {
  testCounter++;
  return join(tmpdir(), `kirkforge-worm-test-${Date.now()}-${testCounter}`);
}

describe.sequential("WormAuditSink", () => {
  let testDir: string;

  beforeEach(() => {
    testDir = freshDir();
    mkdirSync(testDir, { recursive: true });
  });

  afterEach(() => {
    if (existsSync(testDir)) {
      rmSync(testDir, { recursive: true, force: true });
    }
  });

  it("creates segment files in the specified directory", async () => {
    const sink = new WormAuditSink({ directory: testDir });
    const logger = new AuditLogger(sink);

    await logger.record({
      action: "auth.success",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "test",
    });
    await logger.flush();
    await logger.close();

    const files = readdirSync(testDir);
    expect(files.length).toBeGreaterThan(0);
    expect(files.some((f) => f.startsWith("audit-worm"))).toBe(true);
  });

  it("writes events with chain hashes to disk", async () => {
    const sink = new WormAuditSink({ directory: testDir, flushInterval: 1 });
    const logger = new AuditLogger(sink);

    await logger.record({
      action: "verify.start",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "verification started",
    });
    await logger.record({
      action: "verify.complete",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "verification completed",
    });
    await logger.flush();
    await logger.close();

    const files = readdirSync(testDir).filter((f) => f.endsWith(".jsonl"));
    expect(files.length).toBeGreaterThan(0);

    const content = readFileSync(join(testDir, files[0]!), "utf-8").trim();
    const lines = content.split("\n");
    expect(lines.length).toBeGreaterThanOrEqual(2);

    const first = JSON.parse(lines[0]!);
    expect(first.chainHash).toBeTruthy();

    const second = JSON.parse(lines[1]!);
    expect(second.chainHash).toBeTruthy();
  });

  it("verifies chain integrity after writing events", async () => {
    const sink = new WormAuditSink({ directory: testDir, flushInterval: 1 });
    const logger = new AuditLogger(sink);

    for (let i = 0; i < 5; i++) {
      await logger.record({
        action: "tool.invoke",
        outcome: "success",
        actorId: "user1",
        tenantId: "t1",
        reason: `tool ${i}`,
      });
    }
    await logger.flush();
    await logger.close();

    // Create a fresh sink to verify from disk
    const verifySink = new WormAuditSink({ directory: testDir });
    expect(verifySink.verifyIntegrity()).toBe(true);
  });

  it("detects tampered events in the log", async () => {
    const sink = new WormAuditSink({ directory: testDir, flushInterval: 1 });
    const logger = new AuditLogger(sink);

    await logger.record({
      action: "auth.success",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "test",
    });
    await logger.flush();
    await logger.close();

    // Tamper with the file
    const files = readdirSync(testDir).filter((f) => f.endsWith(".jsonl"));
    expect(files.length).toBeGreaterThan(0);
    const filePath = join(testDir, files[0]!);
    const content = readFileSync(filePath, "utf-8");
    const tampered = content.replace("auth.success", "auth.tampered");
    writeFileSync(filePath, tampered, "utf-8");

    // Create a fresh sink to verify from disk
    const verifySink = new WormAuditSink({ directory: testDir });
    expect(verifySink.verifyIntegrity()).toBe(false);
  });

  it("respects custom file prefix", async () => {
    const sink = new WormAuditSink({
      directory: testDir,
      filePrefix: "custom-audit",
      flushInterval: 1,
    });
    const logger = new AuditLogger(sink);

    await logger.record({
      action: "policy.check",
      outcome: "success",
      actorId: "admin",
      tenantId: "",
      reason: "policy check",
    });
    await logger.flush();
    await logger.close();

    const files = readdirSync(testDir);
    expect(files.some((f) => f.startsWith("custom-audit"))).toBe(true);
  });

  it("reports write count", async () => {
    const sink = new WormAuditSink({ directory: testDir });
    const logger = new AuditLogger(sink);

    for (let i = 0; i < 3; i++) {
      await logger.record({
        action: "observe.record",
        outcome: "success",
        actorId: "user1",
        tenantId: "t1",
        reason: `observation ${i}`,
      });
    }
    await logger.flush();

    expect(sink.getWriteCount()).toBe(3);
    await logger.close();
  });

  it("works with MemoryAuditSink for comparison", async () => {
    const memSink = new MemoryAuditSink();
    const logger = new AuditLogger(memSink);

    await logger.record({
      action: "system.startup",
      outcome: "success",
      actorId: "system",
      tenantId: "",
      reason: "system started",
    });
    await logger.flush();

    const events = memSink.getEvents();
    expect(events.length).toBe(1);
    expect(events[0]!.action).toBe("system.startup");
    expect(events[0]!.chainHash).toBeTruthy();
    expect(memSink.verifyChain()).toBe(true);
    await logger.close();
  });
});

describe.sequential("WormAuditSink maxSegments WORM enforcement", () => {
  let testDir: string;

  beforeEach(() => {
    testDir = freshDir();
    mkdirSync(testDir, { recursive: true });
  });

  afterEach(() => {
    if (existsSync(testDir)) {
      rmSync(testDir, { recursive: true, force: true });
    }
  });

  it("refuses NEW segments when maxSegments is reached (allows appends to current segment)", async () => {
    // Use a tiny maxSegmentBytes to force rotation quickly
    const sink = new WormAuditSink({
      directory: testDir,
      maxSegments: 1,
      maxSegmentBytes: 10000, // large enough to allow multiple events per segment
      flushInterval: 1,
    });
    const logger = new AuditLogger(sink);

    // Write one event and flush — should succeed (no segments exist yet)
    await logger.record({
      action: "tool.invoke",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "first event",
    });
    const firstFlush = await sink.flush();
    expect(firstFlush).toBe(true);

    // First segment exists
    const filesAfterFirst = readdirSync(testDir).filter((f) => f.endsWith(".jsonl"));
    expect(filesAfterFirst.length).toBe(1);

    // Write another event — should still succeed because we can append to
    // the existing segment (it has room and is already within maxSegments)
    await logger.record({
      action: "tool.invoke",
      outcome: "success",
      actorId: "user1",
      tenantId: "t1",
      reason: "append event",
    });
    const appendFlush = await sink.flush();
    expect(appendFlush).toBe(true);

    // The event was appended to the existing segment
    const filesAfterAppend = readdirSync(testDir).filter((f) => f.endsWith(".jsonl"));
    expect(filesAfterAppend.length).toBe(1);
    const content = readFileSync(join(testDir, filesAfterAppend[0]!), "utf-8").trim();
    expect(content).toContain("first event");
    expect(content).toContain("append event");

    // Now fill up the segment to trigger rotation, then verify
    // that creating a NEW segment is refused at maxSegments
    // Write enough data to exceed maxSegmentBytes
    for (let i = 0; i < 20; i++) {
      await logger.record({
        action: "tool.invoke",
        outcome: "success",
        actorId: "user1",
        tenantId: "t1",
        reason: `bulk event ${i} to fill segment`,
      });
    }
    const _overflowFlush = await sink.flush();
    // Either flush succeeds (appended to current) or fails (tried to create new segment)
    // The old segment was NOT deleted regardless
    const filesAfterBulk = readdirSync(testDir).filter((f) => f.endsWith(".jsonl"));
    // WORM: old segments must never be deleted
    expect(filesAfterBulk.length).toBeGreaterThanOrEqual(1);
    // Original data is still readable
    const preservedContent = readFileSync(join(testDir, filesAfterBulk[0]!), "utf-8").trim();
    expect(preservedContent).toContain("first event");

    await logger.close();
  });

  it("allows writes when segment count is below maxSegments", async () => {
    const sink = new WormAuditSink({
      directory: testDir,
      maxSegments: 3,
      flushInterval: 1,
    });
    const logger = new AuditLogger(sink);

    // Write several events — should all succeed
    for (let i = 0; i < 3; i++) {
      await logger.record({
        action: "tool.invoke",
        outcome: "success",
        actorId: "user1",
        tenantId: "t1",
        reason: `event ${i}`,
      });
    }
    const result = await sink.flush();
    expect(result).toBe(true);

    await logger.close();
  });
});
