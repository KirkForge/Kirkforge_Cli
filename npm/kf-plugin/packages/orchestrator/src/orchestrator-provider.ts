import type { ModelProviderConfig } from "@kirkforge/model-config";
import type { TaskInput } from "./types.js";
import type { Recommendation } from "@kirkforge/memory-palace";
import type { OrchestratorInternals } from "./orchestrator-shared.js";

/**
 * Resolve the model provider config to use for this delegation.
 * Falls back to the configured default provider if the named provider
 * is missing. Applies memory-bias model preference on top.
 */
export function resolveProvider(
  s: OrchestratorInternals,
  memoryRecommendation?: Recommendation | null,
): ModelProviderConfig {
  const pc = s.modelConfig.providers[s.providerKey];
  if (pc) return applyMemoryModelBias(s, pc, memoryRecommendation);
  const dp = s.modelConfig.providers[s.modelConfig.defaultProvider];
  if (dp) return applyMemoryModelBias(s, dp, memoryRecommendation);
  throw new Error(`No provider found`);
}

/**
 * If memory bias prefers a known model on the same provider, swap
 * defaultModel. Cross-provider or unknown-model preferences are ignored.
 */
export function applyMemoryModelBias(
  s: OrchestratorInternals,
  providerConfig: ModelProviderConfig,
  memoryRecommendation?: Recommendation | null,
): ModelProviderConfig {
  const preferred = memoryRecommendation?.routingBias?.prefer?.[0];
  const confidence = memoryRecommendation?.routingBias?.confidence ?? 0;
  if (!preferred || confidence < 0.65 || preferred === providerConfig.defaultModel)
    return providerConfig;
  const isProviderModel = preferred.includes(":")
    ? preferred.startsWith(providerConfig.provider + ":")
    : true;
  if (!isProviderModel) {
    s.logger?.info(
      `[orchestrator] Memory bias prefers ${preferred} but it belongs to a different provider than ${providerConfig.provider}; ignoring cross-provider bias`,
    );
    return providerConfig;
  }
  const isKnownModel =
    preferred.includes(":") ||
    preferred === providerConfig.defaultModel ||
    Object.values(s.modelConfig.providers).some((p) => p.defaultModel === preferred);
  if (!isKnownModel) {
    s.logger?.info(
      `[orchestrator] Memory bias prefers ${preferred} which is not a known model for provider ${providerConfig.provider}; ignoring unknown model bias`,
    );
    return providerConfig;
  }
  s.logger?.info(
    `[orchestrator] Memory bias prefers model ${preferred} over ${providerConfig.defaultModel} (${Math.round(confidence * 100)}% confidence)`,
  );
  return { ...providerConfig, defaultModel: preferred };
}

/** Recall a routing recommendation from memory, or null on miss/failure. */
export async function recallMemory(
  s: OrchestratorInternals,
  task: TaskInput,
): Promise<Recommendation | null> {
  if (!s.memoryStore) return null;
  try {
    const result = await s.memoryStore.recall(task.description);
    return result.ok ? result.value : null;
  } catch (e) {
    s.logger?.warn(
      `[orchestrator] Memory recall failed: ${e instanceof Error ? e.message : String(e)}`,
    );
    return null;
  }
}
