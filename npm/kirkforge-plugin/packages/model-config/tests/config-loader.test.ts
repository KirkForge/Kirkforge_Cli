import { describe, it, expect } from "vitest";
import { buildModelConfig } from "../src/config-loader.js";

describe("buildModelConfig default provider selection", () => {
  it("prefers MODEL_DEFAULT_PROVIDER when set", () => {
    const result = buildModelConfig({
      MODEL_DEFAULT_PROVIDER: "openai",
      OPENAI_API_KEY: "sk-test-123",
      OLLAMA_BASE_URL: "http://localhost:11434/v1",
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.value.defaultProvider).toBe("openai");
  });

  it("prefers first provider with an API key over local-ollama", () => {
    const result = buildModelConfig({
      OPENAI_API_KEY: "sk-test-123",
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.value.defaultProvider).toBe("openai");
    expect(Object.keys(result.value.providers)).toContain("local-ollama");
  });

  it("prefers openrouter when only openrouter key is present", () => {
    const result = buildModelConfig({
      OPENROUTER_API_KEY: "sk-or-123",
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.value.defaultProvider).toBe("openrouter-free");
  });

  it("falls back to local-ollama when no API keys are present", () => {
    const result = buildModelConfig({});
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.value.defaultProvider).toBe("local-ollama");
  });

  it("selects first provider with apiKey by insertion order when multiple keys present", () => {
    const result = buildModelConfig({
      OPENAI_API_KEY: "sk-test-123",
      OPENROUTER_API_KEY: "sk-or-123",
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.value.defaultProvider).toBe("openrouter-free");
  });

  it("uses explicit MODEL_DEFAULT_PROVIDER even if provider has no key", () => {
    const result = buildModelConfig({
      MODEL_DEFAULT_PROVIDER: "local-ollama",
      OPENAI_API_KEY: "sk-test-123",
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.value.defaultProvider).toBe("local-ollama");
  });
});
