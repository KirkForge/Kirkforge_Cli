/**
 * Feature flags system for gradual rollout and A/B testing.
 *
 * Flags are read from:
 *   1. Environment variables (FEATURE_<NAME>=true/false)
 *   2. Config file (featureFlags in PluginCoreConfig)
 *   3. Defaults (all flags off)
 *
 * Flag naming convention: snake_case, prefixed with FEATURE_
 *   e.g. FEATURE_GRADUAL_DEGRADATION=true
 *
 * Percent-based rollout: flags can target a percentage of tenants.
 *   e.g. { name: "gradual_degradation", rolloutPercent: 25 } means 25% of tenants get it.
 */

export interface FlagDefinition {
  name: string;
  description: string;
  defaultValue: boolean;
  rolloutPercent?: number; // 0-100, percent of tenants that get this flag
  stage: "alpha" | "beta" | "ga" | "deprecated";
}

export interface FlagsConfig {
  flags?: Record<string, boolean>;
  tenantId?: string;
}

// ── Built-in flags ─────────────────────────────────────────────────────────

export const BUILTIN_FLAGS: FlagDefinition[] = [
  {
    name: "gradual_degradation",
    description: "When true, missing external tools cause warnings instead of errors",
    defaultValue: false,
    rolloutPercent: 0,
    stage: "beta",
  },
  {
    name: "encryption_at_rest",
    description: "Encrypt MemoryStore data at rest via core-secrets",
    defaultValue: false,
    rolloutPercent: 0,
    stage: "beta",
  },
  {
    name: "ttl_eviction",
    description: "Enable TTL-based eviction of old memory entries",
    defaultValue: false,
    rolloutPercent: 0,
    stage: "beta",
  },
  {
    name: "prometheus_metrics",
    description: "Expose Prometheus text-format metrics endpoint",
    defaultValue: true,
    rolloutPercent: 100,
    stage: "ga",
  },
  {
    name: "traceparent_propagation",
    description: "W3C traceparent header propagation on health server",
    defaultValue: true,
    rolloutPercent: 100,
    stage: "ga",
  },
  {
    name: "circuit_breaker_metrics",
    description: "Export circuit breaker state as OTEL metrics",
    defaultValue: true,
    rolloutPercent: 100,
    stage: "ga",
  },
];

// ── Flag store ──────────────────────────────────────────────────────────────

let _configFlags: Record<string, boolean> = {};
let _tenantId: string | undefined;

export function initFlags(config: FlagsConfig = {}): void {
  _configFlags = config.flags ?? {};
  _tenantId = config.tenantId;
}

function hashTenant(tenantId: string): number {
  let hash = 0;
  for (let i = 0; i < tenantId.length; i++) {
    const char = tenantId.charCodeAt(i);
    hash = (hash << 5) - hash + char;
    hash |= 0;
  }
  return Math.abs(hash) % 100;
}

/**
 * Check if a feature flag is enabled.
 * Resolution order: env var > config > rollout percent > default
 */
export function isEnabled(flagName: string): boolean {
  // 1. Environment variable
  const envKey = `FEATURE_${flagName.toUpperCase()}`;
  const envVal = process.env[envKey];
  if (envVal !== undefined) {
    return envVal.toLowerCase() === "true" || envVal === "1";
  }

  // 2. Explicit config
  if (flagName in _configFlags) {
    return _configFlags[flagName]!;
  }

  // 3. Rollout percent (tenant-based)
  const def = BUILTIN_FLAGS.find((f) => f.name === flagName);
  if (def?.rolloutPercent && def.rolloutPercent > 0 && _tenantId) {
    const bucket = hashTenant(_tenantId);
    return bucket < def.rolloutPercent;
  }

  // 4. Built-in default
  return def?.defaultValue ?? false;
}

/**
 * Get all flag definitions and their current state.
 */
export function getAllFlags(): Array<FlagDefinition & { enabled: boolean }> {
  return BUILTIN_FLAGS.map((def) => ({
    ...def,
    enabled: isEnabled(def.name),
  }));
}

/**
 * Get flags by stage.
 */
export function getFlagsByStage(
  stage: FlagDefinition["stage"],
): Array<FlagDefinition & { enabled: boolean }> {
  return getAllFlags().filter((f) => f.stage === stage);
}
