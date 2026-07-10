import { z } from "zod";
import { ok, err, type Result } from "@kirkforge/core-types";
import { KirkForgeError } from "@kirkforge/core-errors";

// ── Zod schemas for configuration ────────────────────────────────────────

const LogLevel = z.enum(["trace", "debug", "info", "warn", "error"]);
const LogFormat = z.enum(["json", "human"]);

export const OrchestratorConfigSchema = z.object({
  maxConcurrentWorkers: z.number().int().min(1).max(64).default(4),
  retryAttempts: z.number().int().min(0).max(10).default(2),
  retryDelayMs: z.number().int().min(0).max(60000).default(1000),
});

export const ToolConfigSchema = z.object({
  eslint: z.object({ enabled: z.boolean(), configFile: z.string().optional() }).optional(),
  secdev: z.object({ enabled: z.boolean() }).optional(),
  gitnexus: z.object({ enabled: z.boolean() }).optional(),
  graphify: z.object({ enabled: z.boolean(), queryBudget: z.number().optional() }).optional(),
});

export const LoggingConfigSchema = z.object({
  level: LogLevel.default("info"),
  format: LogFormat.default("json"),
  output: z.string().optional(),
});

export const MemoryConfigSchema = z.object({
  path: z.string().min(1),
  retentionDays: z.number().int().min(1).max(3650).default(30),
});

export const KirkForgeConfigSchema = z.object({
  workspace: z.string().min(1, "workspace path is required"),
  orchestrator: OrchestratorConfigSchema.default({
    maxConcurrentWorkers: 4,
    retryAttempts: 2,
    retryDelayMs: 1000,
  }),
  tools: ToolConfigSchema.default({
    eslint: { enabled: true },
    secdev: { enabled: true },
    gitnexus: { enabled: true },
    graphify: { enabled: false },
  }),
  logging: LoggingConfigSchema.default({
    level: "info",
    format: "json",
  }),
  memory: MemoryConfigSchema.default({
    path: ".kirkforge/memory",
    retentionDays: 30,
  }),
});

export type ValidatedConfig = z.infer<typeof KirkForgeConfigSchema>;

// ── Provider config schemas ──────────────────────────────────────────────

export const ProviderConfigSchema = z.object({
  type: z.enum([
    "openai",
    "anthropic",
    "google",
    "deepseek",
    "xai",
    "groq",
    "mistral",
    "cohere",
    "ollama",
    "openrouter",
  ]),
  apiKey: z.string().optional(),
  baseUrl: z.string().url().optional(),
  defaultModel: z.string().min(1),
  maxRetries: z.number().int().min(0).max(5).default(2),
  timeoutMs: z.number().int().min(1000).max(300000).default(60000),
  maxTokens: z.number().int().min(1).max(1_000_000).default(16384),
  temperature: z.number().min(0).max(2).default(0.7),
});

export const ProvidersMapSchema = z.record(z.string().min(1), ProviderConfigSchema);

// ── Full app config ──────────────────────────────────────────────────────

export const AppConfigSchema = z.object({
  config: KirkForgeConfigSchema,
  providers: ProvidersMapSchema.optional(),
  api: z
    .object({
      port: z.number().int().min(1).max(65535).default(8080),
      host: z.string().default("0.0.0.0"),
      apiKeys: z.array(z.string().min(16)).optional().default([]),
    })
    .optional(),
  health: z
    .object({
      port: z.number().int().min(1).max(65535).default(9090),
    })
    .optional(),
});

export type AppConfig = z.infer<typeof AppConfigSchema>;

// ── Validation helpers ───────────────────────────────────────────────────

/**
 * Validate and parse a configuration object against the full app schema.
 * Returns a Result with typed config or a structured error.
 */
export function validateConfig(raw: unknown): Result<AppConfig, KirkForgeError> {
  const parsed = AppConfigSchema.safeParse(raw);
  if (!parsed.success) {
    const issues = parsed.error.issues.map((i) => ({
      path: i.path.join("."),
      message: i.message,
      code: i.code,
    }));
    return err(
      new KirkForgeError("VALIDATION_ERROR", "Configuration validation failed", { issues }),
    );
  }
  return ok(parsed.data);
}

/**
 * Validate a partial config for hot-reload safety.
 * Returns a Result with the validated subset or error.
 */
export function validatePartialConfig(raw: unknown): Result<Partial<AppConfig>, KirkForgeError> {
  const parsed = AppConfigSchema.partial().safeParse(raw);
  if (!parsed.success) {
    const issues = parsed.error.issues.map((i) => ({
      path: i.path.join("."),
      message: i.message,
      code: i.code,
    }));
    return err(
      new KirkForgeError("VALIDATION_ERROR", "Partial configuration validation failed", { issues }),
    );
  }
  return ok(parsed.data);
}

/**
 * Validate a single provider configuration entry.
 */
export function validateProvider(
  raw: unknown,
): Result<z.infer<typeof ProviderConfigSchema>, KirkForgeError> {
  const parsed = ProviderConfigSchema.safeParse(raw);
  if (!parsed.success) {
    const issues = parsed.error.issues.map((i) => ({
      path: i.path.join("."),
      message: i.message,
      code: i.code,
    }));
    return err(
      new KirkForgeError("INVALID_CONFIG", "Provider configuration is invalid", { issues }),
    );
  }
  return ok(parsed.data);
}

// ── Defaults ─────────────────────────────────────────────────────────────

/** Generate a minimal valid config for quick-start scenarios. */
export function defaultAppConfig(overrides?: Partial<AppConfig>): AppConfig {
  return AppConfigSchema.parse({
    config: {
      workspace: overrides?.config?.workspace ?? process.cwd(),
    },
    ...overrides,
  });
}

// ── Legacy config helpers (deprecated — use ConfigService instead) ──────

/** @deprecated Use ConfigService instead */
export interface KirkForgeLegacyConfig {
  workspace: string;
  memoryPath: string;
}

/** @deprecated Use ConfigService.getPath() instead */
export function resolveMemoryPath(cwd?: string): string {
  return `${cwd ?? process.cwd()}/.kirkforge/memory`;
}

/** @deprecated Validation handled by ConfigService.load() + Zod schemas */
export function validateEnvVars(): string[] {
  return [];
}
