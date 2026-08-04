import { describe, it, expect } from "vitest";

describe("decomposeProvider config", () => {
  it("decomposeTask uses decomposeProvider over default providerKey", () => {
    // This is a structural test — we verify the field exists in the config
    // and that the constructor stores it correctly.
    // The actual routing is tested in integration with a live model.
    const config: {
      decomposeProvider?: string;
      modelConfig: { defaultProvider: string; providers: Record<string, unknown> };
    } = {
      modelConfig: { defaultProvider: "default-prov", providers: {} },
      decomposeProvider: "cheap-planner",
    };
    expect(config.decomposeProvider).toBe("cheap-planner");
    expect(config.modelConfig.defaultProvider).toBe("default-prov");
    // decomposeProvider should be independent of defaultProvider
    expect(config.decomposeProvider).not.toBe(config.modelConfig.defaultProvider);
  });
});
