import type { ChatMessage, ModelResponse, ModelClientOptions } from "./types.js";
import { ModelClientError } from "./model-client-error.js";
import { chatCompletion, anthropicCompletion } from "./adapters.js";
import { traceModelCall, setModelResponseAttributes } from "./tracing.js";

// ── Circuit breaker ─────────────────────────────────────────────────────

interface CircuitState {
  failures: number;
  lastFailure: number;
  open: boolean;
}

class CircuitBreaker {
  private state = new Map<string, CircuitState>();
  private threshold: number;
  private resetTimeoutMs: number;

  constructor(threshold = 5, resetTimeoutMs = 30000) {
    this.threshold = threshold;
    this.resetTimeoutMs = resetTimeoutMs;
  }

  isOpen(key: string): boolean {
    const s = this.state.get(key);
    if (!s || !s.open) return false;
    if (Date.now() - s.lastFailure > this.resetTimeoutMs) {
      // Half-open: allow one probe request
      s.open = false;
      s.failures = 0;
      return false;
    }
    return true;
  }

  getState(key: string): { open: boolean; failures: number } {
    const s = this.state.get(key);
    if (!s) return { open: false, failures: 0 };
    return { open: s.open, failures: s.failures };
  }

  getAllStates(): Record<string, { open: boolean; failures: number }> {
    const result: Record<string, { open: boolean; failures: number }> = {};
    for (const [key, s] of this.state) {
      result[key] = { open: s.open, failures: s.failures };
    }
    return result;
  }

  recordSuccess(key: string): void {
    this.state.delete(key);
  }

  recordFailure(key: string): void {
    const s = this.state.get(key) ?? { failures: 0, lastFailure: 0, open: false };
    const wasOpen = s.open;
    s.failures++;
    s.lastFailure = Date.now();
    if (s.failures >= this.threshold) {
      s.open = true;
    }
    this.state.set(key, s);
    // Emit metrics if telemetry is available
    if (!wasOpen && s.open) {
      this._emitCbMetric(key, "open");
    }
  }

  private _emitCbMetric(key: string, transition: string): void {
    try {
      // Try dynamic import for OTEL to avoid hard dependency
      import("@kirkforge/core-telemetry")
        .then((m) => {
          m.recordCircuitBreakerState?.(key, transition);
        })
        .catch(() => {});
    } catch {}
  }
}

function jitter(delayMs: number): number {
  return delayMs * (0.75 + Math.random() * 0.5);
}

function shouldRetry(
  e: ModelClientError,
  attempt: number,
  maxRetries: number,
): { retry: boolean; delayMs: number } {
  if (attempt >= maxRetries) return { retry: false, delayMs: 0 };
  switch (e.code) {
    case "RATE_LIMIT":
      return { retry: true, delayMs: jitter(Math.min(e.retryAfterMs ?? 2000, 300000)) };
    case "TIMEOUT":
      return attempt === 0 ? { retry: true, delayMs: jitter(1000) } : { retry: false, delayMs: 0 };
    case "AUTH_ERROR":
    case "PARSE_ERROR":
      return { retry: false, delayMs: 0 };
    case "API_ERROR":
      if (e.statusCode === 400 || e.statusCode === 401 || e.statusCode === 403)
        return { retry: false, delayMs: 0 };
      if (e.statusCode && e.statusCode >= 500)
        return { retry: true, delayMs: jitter(Math.min(1000 * Math.pow(2, attempt), 300000)) };
      if (e.statusCode && e.statusCode >= 400) return { retry: false, delayMs: 0 };
      return { retry: true, delayMs: jitter(Math.min(1000 * Math.pow(2, attempt), 300000)) };
    case "NETWORK_ERROR":
      return { retry: true, delayMs: jitter(Math.min(1000 * Math.pow(2, attempt), 300000)) };
    default:
      return { retry: false, delayMs: 0 };
  }
}

// ── Concurrency limiter ─────────────────────────────────────────────────

class ConcurrencyLimiter {
  private running = 0;
  private queue: Array<() => void> = [];

  constructor(private maxConcurrent: number) {}

  async acquire(): Promise<void> {
    if (this.running < this.maxConcurrent) {
      this.running++;
      return;
    }
    return new Promise<void>((resolve) => {
      this.queue.push(() => {
        this.running++;
        resolve();
      });
    });
  }

  release(): void {
    this.running--;
    const next = this.queue.shift();
    if (next) next();
  }

  get active(): number {
    return this.running;
  }
  get waiting(): number {
    return this.queue.length;
  }
}

export class ModelClient {
  private static circuitBreaker = new CircuitBreaker();
  private static concurrencyLimiter = new ConcurrencyLimiter(10);
  constructor(private readonly config: ModelClientOptions) {}

  /** Set global max concurrent model calls. Default: 10. */
  static setMaxConcurrent(max: number): void {
    ModelClient.concurrencyLimiter = new ConcurrencyLimiter(max);
  }

  /** Get current concurrency stats. */
  static concurrencyStats(): { active: number; waiting: number } {
    return {
      active: ModelClient.concurrencyLimiter.active,
      waiting: ModelClient.concurrencyLimiter.waiting,
    };
  }

  private isAnthropic(): boolean {
    return this.config.providerType === "anthropic";
  }

  async chat(messages: ChatMessage[]): Promise<ModelResponse> {
    await ModelClient.concurrencyLimiter.acquire();
    try {
      const providerKey = this.config.providerType;
      // Disambiguate providers of the same type via baseUrl fragment
      const cbKey =
        `${providerKey}:${this.config.defaultModel}` +
        (this.config.providerType === "openai"
          ? ":" + this.config.baseUrl.replace(/https?:\/\//, "").split("/")[0]
          : "");
      if (ModelClient.circuitBreaker.isOpen(cbKey)) {
        throw ModelClientError.api(503, `Circuit breaker open for ${cbKey}`);
      }
      return traceModelCall(providerKey, this.config.defaultModel, async (span) => {
        for (let attempt = 0; attempt <= this.config.maxRetries; attempt++) {
          try {
            let response: ModelResponse;
            if (this.isAnthropic()) {
              response = await anthropicCompletion(messages, this.config);
            } else {
              const headers: Record<string, string> = {};
              if (this.config.apiKey) headers["Authorization"] = `Bearer ${this.config.apiKey}`;
              response = await chatCompletion(messages, this.config, headers);
            }
            ModelClient.circuitBreaker.recordSuccess(cbKey);
            setModelResponseAttributes(span, {
              promptTokens: response.promptTokens,
              completionTokens: response.completionTokens,
              totalTokens: response.totalTokens,
              reasoningTokens: response.reasoningTokens,
              finishReason: response.finishReason,
            });
            return response;
          } catch (e) {
            if (e instanceof ModelClientError && e.code !== "TIMEOUT") {
              ModelClient.circuitBreaker.recordFailure(cbKey);
            }
            if (!(e instanceof ModelClientError)) throw e;
            const decision = shouldRetry(e, attempt, this.config.maxRetries);
            if (!decision.retry) throw e;
            await new Promise((r) => setTimeout(r, decision.delayMs));
          }
        }
        throw ModelClientError.api(500, "Unreachable");
      });
    } finally {
      ModelClient.concurrencyLimiter.release();
    }
  }

  async complete(systemPrompt: string, userPrompt: string): Promise<ModelResponse> {
    return this.chat([
      { role: "system", content: systemPrompt },
      { role: "user", content: userPrompt },
    ]);
  }
}
