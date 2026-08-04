import { describe, it, expect } from "vitest";
import { EventBus } from "../src/index.js";
import { ok } from "@kirkforge/core-types";

describe("EventBus", () => {
  it("emits events to registered handlers", async () => {
    const bus = new EventBus();
    const received: unknown[] = [];
    bus.on("verify.lint", (e) => {
      received.push(e);
      return Promise.resolve(ok(undefined));
    });
    await bus.emit({
      kind: "verify.lint",
      schemaVersion: "v3",
      sequence: 1,
      streamId: "s1",
      taskId: "t1",
      value: { errors: 0, warnings: 0, filesScanned: 0, durationMs: 0, details: [] },
      timestamp: "now",
    });
    expect(received).toHaveLength(1);
  });

  it("deduplicates events", async () => {
    const bus = new EventBus();
    let count = 0;
    bus.on("verify.types", () => {
      count++;
      return Promise.resolve(ok(undefined));
    });
    const ev = {
      kind: "verify.types" as const,
      schemaVersion: "v3" as const,
      sequence: 1,
      streamId: "s1",
      taskId: "t1",
      value: { errors: 0, durationMs: 0, details: [] },
      timestamp: "now",
    };
    await bus.emit(ev);
    await bus.emit(ev);
    expect(count).toBe(1);
  });

  it("tracks running state", () => {
    const bus = new EventBus();
    expect(bus.running).toBe(true);
    bus.shutdown();
    expect(bus.running).toBe(false);
  });

  it("buffers and reports stats", () => {
    const bus = new EventBus({ bufferCapacity: 500 });
    expect(bus.getBufferCapacity()).toBe(500);
    expect(bus.getBufferSize()).toBe(0);
  });
});
