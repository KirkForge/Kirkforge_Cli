export interface ChatMessage {
  role: "system" | "user" | "assistant";
  content: string;
}

export interface ModelResponse {
  content: string;
  model: string;
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  reasoningTokens?: number;
  finishReason?: string;
}

export interface ModelClientOptions {
  baseUrl: string;
  apiKey?: string;
  defaultModel: string;
  timeoutMs: number;
  maxRetries: number;
  maxTokens?: number;
  temperature?: number;
  topP?: number;
  /** Provider type for adapter selection and circuit breaker key derivation. */
  providerType: string;
}
