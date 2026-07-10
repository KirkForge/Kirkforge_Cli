import { describe, it, expect, afterEach } from "vitest";
import {
  initTelemetry,
  shutdownTelemetry,
  isTracingEnabled,
  withSpan,
  withSpanSync,
  getTracer,
} from "../src/index.js";

describe("initTelemetry", () => {
  afterEach(async () => {
    await shutdownTelemetry();
  });

  it("does not enable tracing when config.enabled is false", () => {
    initTelemetry({ serviceName: "test", enabled: false });
    expect(isTracingEnabled()).toBe(false);
  });

  it("isTracingEnabled returns false when not initialized", () => {
    expect(isTracingEnabled()).toBe(false);
  });
});

describe("withSpan", () => {
  it("returns fn result even when telemetry is disabled", async () => {
    const result = await withSpan("test-span", async () => 42);
    expect(result).toBe(42);
  });
});

describe("withSpanSync", () => {
  it("returns fn result even when telemetry is disabled", () => {
    const result = withSpanSync("test-span-sync", () => "hello");
    expect(result).toBe("hello");
  });
});

describe("getTracer", () => {
  it("returns a tracer even when telemetry is not initialized", () => {
    const tracer = getTracer();
    expect(tracer).toBeDefined();
    expect(typeof tracer.startSpan).toBe("function");
  });
});
