import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { isEnabled, initFlags, getAllFlags, getFlagsByStage, BUILTIN_FLAGS } from "../src/index.js";

describe("core-flags", () => {
  beforeEach(() => {
    // Reset to defaults
    initFlags({ flags: {}, tenantId: undefined });
    // Clear env vars
    for (const def of BUILTIN_FLAGS) {
      delete process.env[`FEATURE_${def.name.toUpperCase()}`];
    }
  });

  afterEach(() => {
    for (const def of BUILTIN_FLAGS) {
      delete process.env[`FEATURE_${def.name.toUpperCase()}`];
    }
  });

  it("defaults to builtin values", () => {
    expect(isEnabled("gradual_degradation")).toBe(false);
    expect(isEnabled("prometheus_metrics")).toBe(true);
    expect(isEnabled("traceparent_propagation")).toBe(true);
  });

  it("unknown flag returns false", () => {
    expect(isEnabled("nonexistent_flag")).toBe(false);
  });

  it("env var overrides default", () => {
    process.env.FEATURE_GRADUAL_DEGRADATION = "true";
    expect(isEnabled("gradual_degradation")).toBe(true);
  });

  it("env var '1' enables flag", () => {
    process.env.FEATURE_TTL_EVICTION = "1";
    expect(isEnabled("ttl_eviction")).toBe(true);
  });

  it("config overrides default", () => {
    initFlags({ flags: { encryption_at_rest: true } });
    expect(isEnabled("encryption_at_rest")).toBe(true);
  });

  it("env var beats config", () => {
    initFlags({ flags: { gradual_degradation: false } });
    process.env.FEATURE_GRADUAL_DEGRADATION = "true";
    expect(isEnabled("gradual_degradation")).toBe(true);
  });

  it("rollout percent - gradual_degradation disabled at 0% rollout", () => {
    // Explicitly clear any env var that might affect the flag
    delete process.env.FEATURE_GRADUAL_DEGRADATION;
    initFlags({ tenantId: "tenant-alpha" });
    // gradual_degradation has rolloutPercent: 0, should be false
    expect(isEnabled("gradual_degradation")).toBe(false);
  });

  it("getAllFlags returns all definitions with enabled state", () => {
    const flags = getAllFlags();
    expect(flags.length).toBe(BUILTIN_FLAGS.length);
    expect(flags[0]).toHaveProperty("name");
    expect(flags[0]).toHaveProperty("enabled");
  });

  it("getFlagsByStage filters correctly", () => {
    const ga = getFlagsByStage("ga");
    expect(ga.every((f) => f.stage === "ga")).toBe(true);
    const beta = getFlagsByStage("beta");
    expect(beta.every((f) => f.stage === "beta")).toBe(true);
  });
});
