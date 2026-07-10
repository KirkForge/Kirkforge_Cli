import { describe, it, expect } from "vitest";
import { Agent } from "../src/index.js";

function stubProvider() {
  return {
    provider: "local-ollama" as const,
    baseUrl: "http://localhost:9999",
    defaultModel: "stub",
    timeoutMs: 1000,
    maxRetries: 0,
  };
}

describe("Agent", () => {
  it("constructs with provider config", () => {
    const agent = new Agent("test-agent", stubProvider());
    expect(agent.agentId).toBe("test-agent");
  });

  it("throws on network failure", async () => {
    const agent = new Agent("test-agent", stubProvider());
    await expect(
      agent.execute({
        description: "build a server",
        variables: {
          language: "typescript",
          defaultFile: "output.ts",
          languageHint: "Emit TypeScript.",
          checkCommand: "",
        },
      }),
    ).rejects.toThrow();
  });
});
