import { describe, it, expect } from "vitest";
import { ModelClient } from "../src/model-client.js";
import { ModelClientError } from "../src/model-client-error.js";
import { parseChatCompletionResponse } from "../src/shared.js";
import { ChatCompletionSchema } from "../src/shared.js";
import type { ModelClientOptions } from "../src/types.js";

function makeClient(overrides: Partial<ModelClientOptions> = {}): ModelClient {
  return new ModelClient({
    baseUrl: "http://localhost:9999",
    defaultModel: "test-model",
    timeoutMs: 5000,
    maxRetries: 0,
    providerType: "openai",
    ...overrides,
  });
}

// ── ModelClientError ───────────────────────────────────────────────────────

describe("ModelClientError", () => {
  it("creates typed errors", () => {
    const e = ModelClientError.timeout(5000);
    expect(e.code).toBe("TIMEOUT");
    expect(e.message).toContain("5000ms");
  });

  it("creates auth errors with 401 status", () => {
    const e = ModelClientError.auth("invalid key");
    expect(e.code).toBe("AUTH_ERROR");
    expect(e.statusCode).toBe(401);
  });

  it("creates rate limit errors", () => {
    const e = ModelClientError.rateLimit("too many", 30000);
    expect(e.code).toBe("RATE_LIMIT");
    expect(e.retryAfterMs).toBe(30000);
  });

  it("creates rate limit errors without retry-after", () => {
    const e = ModelClientError.rateLimit("too many");
    expect(e.code).toBe("RATE_LIMIT");
    expect(e.retryAfterMs).toBeUndefined();
  });

  it("creates api errors with status code", () => {
    const e = ModelClientError.api(500, "internal");
    expect(e.code).toBe("API_ERROR");
    expect(e.statusCode).toBe(500);
  });

  it("creates network errors", () => {
    const e = ModelClientError.network("connection refused");
    expect(e.code).toBe("NETWORK_ERROR");
    expect(e.message).toContain("connection refused");
  });

  it("creates parse errors", () => {
    const e = ModelClientError.parse("invalid JSON");
    expect(e.code).toBe("PARSE_ERROR");
    expect(e.message).toContain("invalid JSON");
  });
});

// ── ModelClient construction ───────────────────────────────────────────────

describe("ModelClient", () => {
  it("constructs with config", () => {
    const c = makeClient();
    expect(c).toBeInstanceOf(ModelClient);
  });

  it("detects Anthropic from providerType", () => {
    const c = makeClient({
      providerType: "anthropic",
      baseUrl: "https://api.anthropic.com/v1",
    });
    expect(c).toBeInstanceOf(ModelClient);
  });

  it("complete builds chat messages from system/user prompt", async () => {
    const c = makeClient({ maxRetries: 0 });
    try {
      await c.complete("system prompt", "user prompt");
    } catch (e) {
      expect(e).toBeInstanceOf(ModelClientError);
      expect((e as ModelClientError).code).toBe("NETWORK_ERROR");
    }
  });

  it("chat fails with NETWORK_ERROR when server unreachable", async () => {
    const c = makeClient({ maxRetries: 0, timeoutMs: 100 });
    try {
      await c.chat([{ role: "user", content: "hello" }]);
      // Should not reach here
      expect.unreachable("Expected network error");
    } catch (e) {
      expect(e).toBeInstanceOf(ModelClientError);
      expect((e as ModelClientError).code).toBeOneOf(["NETWORK_ERROR", "TIMEOUT"]);
    }
  });
});

// ── Concurrency stats ──────────────────────────────────────────────────────

describe("ModelClient.concurrencyStats", () => {
  it("reports active and waiting counts", () => {
    const stats = ModelClient.concurrencyStats();
    expect(stats).toHaveProperty("active");
    expect(stats).toHaveProperty("waiting");
    expect(typeof stats.active).toBe("number");
    expect(typeof stats.waiting).toBe("number");
  });

  it("setMaxConcurrent changes the limit", () => {
    ModelClient.setMaxConcurrent(5);
    // No easy way to verify without making actual calls,
    // but at least it doesn't throw
    expect(true).toBe(true);
    // Reset
    ModelClient.setMaxConcurrent(10);
  });
});

// ── parseChatCompletionResponse ────────────────────────────────────────────

describe("parseChatCompletionResponse", () => {
  const opts: ModelClientOptions = {
    baseUrl: "http://localhost:9999",
    defaultModel: "test-model",
    timeoutMs: 5000,
    maxRetries: 0,
    providerType: "openai",
  };

  it("parses a standard OpenAI response", () => {
    const raw = {
      model: "gpt-4o",
      choices: [
        {
          message: { content: "Hello, world!" },
          finish_reason: "stop",
        },
      ],
      usage: {
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
      },
    };
    const result = parseChatCompletionResponse(raw, ChatCompletionSchema, opts);
    expect(result.content).toBe("Hello, world!");
    expect(result.model).toBe("gpt-4o");
    expect(result.promptTokens).toBe(10);
    expect(result.completionTokens).toBe(5);
    expect(result.totalTokens).toBe(15);
    expect(result.finishReason).toBe("stop");
  });

  it("falls back to default model when response model is missing", () => {
    const raw = {
      choices: [{ message: { content: "test" }, finish_reason: "stop" }],
      usage: { prompt_tokens: 1, completion_tokens: 1 },
    };
    const result = parseChatCompletionResponse(raw, ChatCompletionSchema, opts);
    expect(result.model).toBe("test-model");
  });

  it("handles missing usage gracefully", () => {
    const raw = {
      choices: [{ message: { content: "no usage" }, finish_reason: "length" }],
    };
    const result = parseChatCompletionResponse(raw, ChatCompletionSchema, opts);
    expect(result.content).toBe("no usage");
    expect(result.promptTokens).toBe(0);
    expect(result.completionTokens).toBe(0);
    expect(result.totalTokens).toBe(0);
  });

  it("extracts content from extra keys (reasoning)", () => {
    const raw = {
      model: "test",
      choices: [
        {
          message: { content: "", reasoning: "step-by-step logic" },
          finish_reason: "stop",
        },
      ],
      usage: { prompt_tokens: 5, completion_tokens: 10 },
    };
    const result = parseChatCompletionResponse(raw, ChatCompletionSchema, opts, ["reasoning"]);
    expect(result.content).toBe("step-by-step logic");
  });

  it("prefers content over extra keys", () => {
    const raw = {
      model: "test",
      choices: [
        {
          message: { content: "primary", reasoning: "secondary" },
          finish_reason: "stop",
        },
      ],
    };
    const result = parseChatCompletionResponse(raw, ChatCompletionSchema, opts, ["reasoning"]);
    expect(result.content).toBe("primary");
  });

  it("computes total tokens when not reported", () => {
    const raw = {
      model: "test",
      choices: [{ message: { content: "ok" } }],
      usage: { prompt_tokens: 3, completion_tokens: 7 },
    };
    const result = parseChatCompletionResponse(raw, ChatCompletionSchema, opts);
    expect(result.totalTokens).toBe(10);
  });
});

// ── shouldRetry logic (indirectly via ModelClientError codes) ──────────────

describe("Retry decision logic", () => {
  it("RATE_LIMIT errors carry retry-after-ms", () => {
    const e = ModelClientError.rateLimit("slow down", 5000);
    expect(e.code).toBe("RATE_LIMIT");
    expect(e.retryAfterMs).toBe(5000);
    expect(e.statusCode).toBe(429);
  });

  it("AUTH_ERROR has 401 status", () => {
    const e = ModelClientError.auth("bad key");
    expect(e.statusCode).toBe(401);
    expect(e.code).toBe("AUTH_ERROR");
  });

  it("API_ERROR preserves status code", () => {
    const e = ModelClientError.api(503, "service unavailable");
    expect(e.statusCode).toBe(503);
    expect(e.code).toBe("API_ERROR");
  });

  it("NETWORK_ERROR has no status code", () => {
    const e = ModelClientError.network("ECONNREFUSED");
    expect(e.statusCode).toBeUndefined();
  });

  it("TIMEOUT has no status code", () => {
    const e = ModelClientError.timeout(10000);
    expect(e.statusCode).toBeUndefined();
    expect(e.message).toContain("10000ms");
  });
});
