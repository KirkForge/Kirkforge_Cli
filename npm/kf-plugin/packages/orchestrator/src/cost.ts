// ── Model cost estimation ──────────────────────────────────────────────────
//
// Simple cost estimation for model calls based on provider and token counts.
// Used for tracking and reporting delegation costs.

const PROVIDER_COST_RATES: Record<string, { input: number; output: number }> = {
  "local-ollama": { input: 0, output: 0 },
  "openrouter-free": { input: 0, output: 0 },
  "nvidia-free": { input: 0, output: 0 },
  openai: { input: 0.00015, output: 0.0006 },
  anthropic: { input: 0.0008, output: 0.004 },
  deepseek: { input: 0.000014, output: 0.000028 },
  google: { input: 0.0000375, output: 0.00015 },
  xai: { input: 0.0002, output: 0.0008 },
  groq: { input: 0.000059, output: 0.000079 },
  mistral: { input: 0.0002, output: 0.0006 },
  cohere: { input: 0.0003, output: 0.0015 },
};

/**
 * Resolve the cost provider key from a provider resolved string.
 * Maps sub-provider keys (e.g. "openai/gpt-4o" → "openai") to their cost rate key.
 */
export function resolveCostProviderKey(providerResolved: string): string {
  if (PROVIDER_COST_RATES[providerResolved]) return providerResolved;
  const lower = providerResolved.toLowerCase();
  for (const key of Object.keys(PROVIDER_COST_RATES)) {
    if (lower.startsWith(key)) return key;
  }
  return "local-ollama";
}

/**
 * Estimate the cost of a model call in USD based on provider, prompt tokens,
 * and completion tokens. Uses per-1K-token rates.
 */
export function estimateSimpleCost(
  provider: string,
  promptTokens: number,
  completionTokens: number,
): number {
  const key = resolveCostProviderKey(provider);
  const r = PROVIDER_COST_RATES[key] ?? PROVIDER_COST_RATES["local-ollama"]!;
  return (promptTokens * r.input + completionTokens * r.output) / 1000;
}
