import { ok, err, type Result } from "@kirkforge/core-types";
import type { SecretsManager } from "@kirkforge/core-secrets";
import { ConfigError } from "@kirkforge/core-errors";
import { ModelConfigSchema, type ModelConfig, type ModelProviderConfig } from "./schemas.js";

const DEFAULTS = {
  ollama: "http://localhost:11434/v1",
  openrouter: "https://openrouter.ai/api/v1",
  openai: "https://api.openai.com/v1",
  anthropic: "https://api.anthropic.com/v1",
  nvidia: "https://integrate.api.nvidia.com/v1",
  deepseek: "https://api.deepseek.com/v1",
} as const;

function makeProvider(
  provider: ModelProviderConfig["provider"],
  baseUrl: string,
  env: Record<string, string | undefined>,
  defaultModel: string,
  needsKey: boolean,
): ModelProviderConfig | null {
  const apiKey = env[`${provider.toUpperCase()}_API_KEY`];
  if (needsKey && !apiKey) return null;
  return {
    provider,
    baseUrl: env[`${provider.toUpperCase()}_BASE_URL`] ?? baseUrl,
    apiKey,
    defaultModel: env[`${provider.toUpperCase()}_DEFAULT_MODEL`] ?? defaultModel,
    timeoutMs: parseInt(env.MODEL_TIMEOUT_MS ?? "120000", 10),
    maxRetries: provider === "ollama" ? 1 : 2,
  };
}

export function buildModelConfig(
  env?: Record<string, string | undefined>,
): Result<ModelConfig, ConfigError> {
  const envVars = env ?? (process.env as Record<string, string | undefined>);
  const providers: Record<string, ModelProviderConfig> = {};

  const ollama = makeProvider("ollama", DEFAULTS.ollama, envVars, "kimi-k2.6:cloud", false);
  if (ollama) providers["local-ollama"] = ollama;

  const openrouter = makeProvider(
    "openrouter",
    DEFAULTS.openrouter,
    envVars,
    "google/gemma-3-4b-it:free",
    true,
  );
  if (openrouter) providers["openrouter-free"] = openrouter;

  const openai = makeProvider("openai", DEFAULTS.openai, envVars, "gpt-4o-mini", true);
  if (openai) providers["openai"] = openai;

  const anthropic = makeProvider(
    "anthropic",
    DEFAULTS.anthropic,
    envVars,
    "claude-haiku-4-5-20251001",
    true,
  );
  if (anthropic) providers["anthropic"] = anthropic;

  const nvidia = makeProvider("nvidia", DEFAULTS.nvidia, envVars, "minimaxai/minimax-m2.7", true);
  if (nvidia) providers["nvidia-free"] = nvidia;

  const deepseek = makeProvider("deepseek", DEFAULTS.deepseek, envVars, "deepseek-chat", true);
  if (deepseek) providers["deepseek"] = deepseek;

  if (Object.keys(providers).length === 0) {
    return err(
      new ConfigError(
        "No model providers configured. Set OLLAMA_BASE_URL or provider API keys in .env",
      ),
    );
  }

  const defaultProvider =
    envVars.MODEL_DEFAULT_PROVIDER ??
    Object.entries(providers).find(([, p]) => p.apiKey != null)?.[0] ??
    Object.keys(providers)[0]!;

  if (envVars.MODEL_DEFAULT_PROVIDER && !providers[defaultProvider]) {
    return err(
      new ConfigError(
        `MODEL_DEFAULT_PROVIDER "${envVars.MODEL_DEFAULT_PROVIDER}" is not a valid provider key. Available: ${Object.keys(providers).join(", ")}`,
      ),
    );
  }

  const config: ModelConfig = {
    providers,
    defaultProvider,
  };
  const parsed = ModelConfigSchema.safeParse(config);
  if (!parsed.success)
    return err(new ConfigError(`Model config validation failed: ${parsed.error.message}`));
  return ok(parsed.data);
}

// ── Async version with secrets provider ──────────────────────────────────

/**
 * Build model config resolving API keys through a SecretsManager.
 * Keys are looked up using the pattern: secret/{provider}_api_key
 * e.g. "secret/OPENAI_API_KEY" → resolved via vault/aws/gcp/env chain.
 */
export async function buildModelConfigAsync(
  secrets: SecretsManager,
  env?: Record<string, string | undefined>,
): Promise<Result<ModelConfig, ConfigError>> {
  const envVars = env ?? (process.env as Record<string, string | undefined>);

  // Build an enriched env that resolves keys through secrets manager
  const enriched: Record<string, string | undefined> = { ...envVars };

  const providers = ["OLLAMA", "OPENROUTER", "OPENAI", "ANTHROPIC", "NVIDIA", "DEEPSEEK"] as const;
  for (const p of providers) {
    const keyName = `${p}_API_KEY`;
    if (!enriched[keyName]) {
      // Try secrets manager with standard naming convention
      const secretValue = await secrets.get(`secret/${p.toLowerCase()}_api_key`);
      if (secretValue) enriched[keyName] = secretValue;
    }
  }

  return buildModelConfig(enriched);
}
