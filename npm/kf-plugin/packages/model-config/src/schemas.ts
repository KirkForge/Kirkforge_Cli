import { z } from "zod";

export const ModelProviderSchema = z.enum([
  "ollama",
  "openai",
  "openrouter",
  "anthropic",
  "nvidia",
  "deepseek",
]);

export const ModelProviderConfigSchema = z.object({
  provider: ModelProviderSchema,
  baseUrl: z.string(),
  apiKey: z.string().optional(),
  defaultModel: z.string(),
  timeoutMs: z.number().positive().default(120000),
  maxRetries: z.number().int().nonnegative().default(2),
  maxTokens: z.number().int().positive().optional(),
  temperature: z.number().min(0).max(2).optional(),
});

export const ModelConfigSchema = z.object({
  providers: z.record(z.string(), ModelProviderConfigSchema),
  defaultProvider: z.string(),
});

export type ModelProviderConfig = z.infer<typeof ModelProviderConfigSchema>;
export type ModelConfig = z.infer<typeof ModelConfigSchema>;
export type ModelProvider = z.infer<typeof ModelProviderSchema>;
