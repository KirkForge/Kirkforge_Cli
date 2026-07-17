import { EventBus } from "@kirkforge/core-events";
import { Logger } from "@kirkforge/core-logging";
import { ConfigService } from "@kirkforge/core-config";
import { buildModelConfigAsync } from "@kirkforge/model-config";
import { Orchestrator } from "@kirkforge/orchestrator";
import { MemoryStore } from "@kirkforge/memory-palace";
import { createSecretsManager, TenantKeyProvider } from "@kirkforge/core-secrets";
import { initTelemetry, shutdownTelemetry, isTracingEnabled } from "@kirkforge/core-telemetry";
import type { SecretsManager } from "@kirkforge/core-secrets";
import {
  isEnterpriseMode,
  requireEnterpriseOrDev,
  type EnterpriseConfig,
  QuotaManager,
  QuotaPersistence,
} from "@kirkforge/core-enterprise";
import { PolicyEngine } from "@kirkforge/core-policy";
import { TenantRegistry } from "@kirkforge/core-tenancy";
import {
  AuditLogger,
  MemoryAuditSink,
  createAuditSink,
  type AuditSink,
} from "@kirkforge/core-events";
import { readFileSync } from "fs";
import { URL } from "node:url";

// Config loading is explicit — no auto-loading of .env or .kirkforcerc from cwd.
// Use createBootstrap({...}) to pass configuration, or set env vars externally.

export const VERSION: string = (() => {
  try {
    const pkg = JSON.parse(readFileSync(new URL("../../package.json", import.meta.url), "utf-8"));
    return pkg.version ?? "0.0.0";
  } catch {
    return "0.0.0";
  }
})();

export interface BootstrapOpts {
  provider?: string;
  json?: boolean;
  mode?: string;
  maxTokens?: number;
  temperature?: number;
  /** Disable OpenTelemetry even if OTEL_EXPORTER_OTLP_ENDPOINT is set. */
  noOtel?: boolean;
  /** Workspace path for tenant scoping. Defaults to cwd. */
  workspace?: string;
  /**
   * Allow the bootstrap to proceed without a configured model provider.
   * Commands that never call a model (e.g. verify, health, audit-verify)
   * can set this so the CLI works in installs that have no OLLAMA_BASE_URL
   * or API keys configured.
   */
  allowMissingModel?: boolean;
}

export interface BootstrapResult {
  orchestrator: Orchestrator;
  configService: ConfigService;
  modelConfig: import("@kirkforge/model-config").ModelConfig;
  eventBus: EventBus;
  logger: Logger;
  memoryStore: MemoryStore;
  secretsManager: SecretsManager | null;
  enterpriseConfig: EnterpriseConfig;
  policyEngine: PolicyEngine;
  auditLogger: AuditLogger;
  tenantRegistry: TenantRegistry;
  /** Per-tenant quota manager (enterprise mode only, no-op in dev). */
  quotaManager: QuotaManager;
  /** Call to gracefully shut down telemetry and flush audit. */
  shutdown: () => Promise<void>;
}

export async function createBootstrap(opts: BootstrapOpts): Promise<BootstrapResult> {
  const eventBus = new EventBus();
  const logger = new Logger({
    level: "info",
    format: opts.json ? "json" : "human",
    stream: opts.json ? "stderr" : "stdout",
  });

  // ── Enterprise mode gate ───────────────────────────────────────────────
  // In enterprise mode, startup will throw if critical controls are missing.
  // In dev mode, missing controls produce warnings but don't block startup.
  const enterpriseResult = requireEnterpriseOrDev();
  let enterpriseConfig: EnterpriseConfig;
  if (enterpriseResult.ok) {
    enterpriseConfig = enterpriseResult.value;
  } else {
    // Enterprise mode validation failed — this should only happen in enterprise mode
    logger.error(
      `[bootstrap] Enterprise mode validation failed: ${enterpriseResult.error.message}`,
    );
    if (isEnterpriseMode()) {
      for (const v of enterpriseResult.error.violations) {
        logger.error(`[bootstrap]   ${v.severity}: ${v.control} — ${v.message}`);
        logger.error(`[bootstrap]     Remediation: ${v.remediation}`);
      }
      throw enterpriseResult.error;
    }
    // Dev mode fallback
    enterpriseConfig = {
      enabled: false,
      auth: { configured: false },
      audit: { configured: false, sinkType: "none" },
      policy: { configured: false },
      storage: { backend: "memory", durable: false },
      secrets: { providers: ["env"], envOnlyFallback: true },
    };
  }

  if (enterpriseConfig.enabled) {
    logger.info("[bootstrap] Enterprise mode ENABLED. All critical controls validated.");
  } else {
    logger.debug("[bootstrap] Running in developer mode. Enterprise controls not enforced.");
  }

  // ── Secrets manager ──────────────────────────────────────────────────
  const secretsManager = createSecretsManager();
  logger.info(
    "[bootstrap] Secrets manager initialized with providers: " +
      ["vault", "aws", "gcp", "env"]
        .filter((p) => {
          if (p === "env") return true;
          const e = process.env as Record<string, string | undefined>;
          if (p === "vault") return !!(e.VAULT_ADDR && e.VAULT_TOKEN);
          if (p === "aws") return !!e.AWS_REGION;
          if (p === "gcp") return !!e.GCP_PROJECT_ID;
          return false;
        })
        .join(", "),
  );

  // ── OpenTelemetry ────────────────────────────────────────────────────
  const otelEndpoint = process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
  if (otelEndpoint && !opts.noOtel) {
    initTelemetry({
      serviceName: "kirkforge",
      serviceVersion: VERSION,
      otlpEndpoint: otelEndpoint,
      logger,
    });
    logger.info(`[bootstrap] OpenTelemetry exporting to ${otelEndpoint}`);
  }

  // ── Config ───────────────────────────────────────────────────────────
  const configResult = ConfigService.load();
  const configService: ConfigService = configResult.ok
    ? configResult.value
    : ConfigService.fromConfig({
        workspace: opts.workspace ?? ".",
        orchestrator: { maxConcurrentWorkers: 4, retryAttempts: 3, retryDelayMs: 1000 },
        tools: {
          eslint: { enabled: true },
          secdev: { enabled: true },
        },
        logging: { level: "info", format: "json" },
        memory: { path: ".kirkforge/memory", retentionDays: 30 },
      });

  // ── Policy engine ─────────────────────────────────────────────────────
  const policyEngine = new PolicyEngine();
  const policyFilePath = process.env.POLICY_FILE_PATH ?? process.env["KIRKFORGE_POLICY_FILE"];
  if (policyFilePath) {
    const result = policyEngine.loadFromFile(policyFilePath);
    if (!result.ok) {
      logger.error(`[bootstrap] Failed to load policy file: ${result.error.message}`);
      if (enterpriseConfig.enabled) {
        throw new Error(`Enterprise mode requires a valid policy file: ${result.error.message}`);
      }
    } else {
      logger.info(
        `[bootstrap] Policy loaded from ${policyFilePath} (hash: ${policyEngine.getHash()})`,
      );
    }
  } else if (enterpriseConfig.enabled) {
    throw new Error("Enterprise mode requires a policy file (POLICY_FILE_PATH not set)");
  } else {
    logger.debug("[bootstrap] No policy file configured. Using default-deny policy.");
  }

  // ── Audit logger ──────────────────────────────────────────────────────
  let auditSink: AuditSink;
  const auditSinkType =
    process.env.AUDIT_SINK_TYPE ?? process.env["KIRKFORGE_AUDIT_SINK"] ?? "none";

  if (auditSinkType === "file") {
    const filePath = process.env.AUDIT_FILE_PATH ?? "/var/lib/kirkforge/audit/audit.jsonl";
    auditSink = createAuditSink({ type: "file", filePath });
    logger.info(`[bootstrap] Audit sink: file (${filePath})`);
  } else if (auditSinkType === "http") {
    const httpUrl = process.env.AUDIT_HTTP_URL;
    if (!httpUrl) {
      throw new Error("AUDIT_HTTP_URL is required for HTTP audit sink");
    }
    auditSink = createAuditSink({ type: "http", httpUrl });
    logger.info(`[bootstrap] Audit sink: HTTP (${httpUrl})`);
  } else if (auditSinkType === "memory") {
    auditSink = new MemoryAuditSink();
    logger.warn("[bootstrap] Audit sink: in-memory (NOT durable — do not use in production)");
  } else {
    auditSink = new MemoryAuditSink();
    if (enterpriseConfig.enabled) {
      throw new Error(
        "Enterprise mode requires a durable audit sink (AUDIT_SINK_TYPE=file or http)",
      );
    }
    logger.warn("[bootstrap] No audit sink configured. Using in-memory (ephemeral) audit.");
  }
  const auditLogger = new AuditLogger(auditSink);

  // ── Record startup audit event ────────────────────────────────────────
  await auditLogger.record({
    action: "system.startup",
    outcome: "success",
    actorId: "system",
    tenantId: "",
    reason: enterpriseConfig.enabled ? "Enterprise mode startup" : "Dev mode startup",
    policyHash: policyEngine.getHash(),
  });

  // ── Model config (with secrets resolution) ──────────────────────────
  const env = process.env as Record<string, string | undefined>;
  const modelConfigResult = await buildModelConfigAsync(secretsManager, env);
  let modelConfig: import("@kirkforge/model-config").ModelConfig;
  if (!modelConfigResult.ok) {
    if (opts.allowMissingModel) {
      logger.warn(
        `[bootstrap] No model provider configured; proceeding with a no-provider config because allowMissingModel is set.`,
      );
      modelConfig = { providers: {}, defaultProvider: "none" };
    } else {
      throw new Error(
        `Model config error: ${modelConfigResult.error.message}. Set OLLAMA_BASE_URL or provider API keys.`,
      );
    }
  } else {
    modelConfig = modelConfigResult.value;
  }
  if (opts.provider) {
    if (!modelConfig.providers[opts.provider]) {
      const available = Object.keys(modelConfig.providers);
      throw new Error(
        `Provider "${opts.provider}" not found. Available providers: ${available.join(", ")}`,
      );
    }
    modelConfig.defaultProvider = opts.provider;
  }

  if (opts.maxTokens || opts.temperature !== undefined) {
    const key = opts.provider ?? modelConfig.defaultProvider;
    const pc = modelConfig.providers[key];
    if (pc) {
      if (opts.maxTokens) pc.maxTokens = opts.maxTokens;
      if (opts.temperature !== undefined) pc.temperature = opts.temperature;
    }
  }

  // ── Memory ───────────────────────────────────────────────────────────
  const _memoryPath = configService.get().memory.path || ".kirkforge/memory.json";
  const _memoryBackend = process.env.MEMORY_BACKEND ?? process.env["KIRKFORGE_MEMORY_BACKEND"];

  // ── Tenant registry (single instance used by both memory and returned) ──
  const tenantRegistry = new TenantRegistry();
  const workspacePath = opts.workspace ?? process.cwd();
  const tenantHandle = tenantRegistry.register(workspacePath);

  // ── Per-tenant encryption ────────────────────────────────────────────
  // In enterprise mode, derive per-tenant DEKs from the master KEK for
  // encryption at rest. The master key is loaded from the secrets chain
  // (Vault → AWS → GCP → env) or from KIRKFORGE_TENANT_KEK env var.
  let keyProvider: TenantKeyProvider | undefined;
  const masterKeyHex = process.env["KIRKFORGE_TENANT_KEK"];
  if (enterpriseConfig.enabled && masterKeyHex) {
    const masterKey = Buffer.from(masterKeyHex, "hex");
    if (masterKey.length === 32) {
      keyProvider = new TenantKeyProvider({ masterKey });
      logger.info(
        `[bootstrap] Per-tenant encryption ENABLED (KEK loaded, ${masterKeyHex.slice(0, 8)}…)`,
      );
    } else {
      logger.warn(
        `[bootstrap] Per-tenant encryption: KIRKFORGE_TENANT_KEK must be 32 bytes (64 hex chars), got ${masterKey.length} bytes. Encryption DISABLED.`,
      );
    }
  } else if (enterpriseConfig.enabled) {
    logger.warn(
      "[bootstrap] Enterprise mode: no KIRKFORGE_TENANT_KEK set. Per-tenant encryption DISABLED. Set a 32-byte hex key for production.",
    );
  }

  const memoryResult = await tenantRegistry.createMemoryStore(tenantHandle.tenantId, {
    keyProvider,
  });
  if (!memoryResult.ok) {
    throw new Error(`Failed to create memory store: ${memoryResult.error.message}`);
  }
  const memoryStore = memoryResult.value;

  if (keyProvider) {
    logger.info(`[bootstrap] Memory: encrypted per-tenant (${tenantHandle.tenantId})`);
  } else {
    logger.info(`[bootstrap] Memory: unencrypted (${tenantHandle.tenantId})`);
  }

  // ── Quota manager ───────────────────────────────────────────────────
  const quotaManager = new QuotaManager();
  if (enterpriseConfig.enabled) {
    // In enterprise mode, set up quota persistence for cross-process durability
    const quotaPersistencePath = process.env.QUOTA_PERSISTENCE_PATH ?? ".kirkforge/quotas.json";
    const quotaPersistence = new QuotaPersistence(quotaManager, { filePath: quotaPersistencePath });
    const loadResult = quotaPersistence.load();
    if (loadResult.ok) {
      quotaPersistence.startAutoSave();
      logger.info("[bootstrap] Quota persistence enabled");
    } else {
      logger.warn("[bootstrap] Quota persistence load failed: " + loadResult.error.message);
    }
  } else {
    logger.debug("[bootstrap] Quota manager: in-memory (no persistence in dev mode)");
  }

  // ── Orchestrator ─────────────────────────────────────────────────────
  const orchestrator = new Orchestrator({
    modelConfig,
    providerKey: opts.provider,
    logger,
    eventBus,
    memoryStore,
    policyEngine,
  });

  const shutdown = async () => {
    logger.info("[bootstrap] Graceful shutdown initiated");
    await auditLogger.record({
      action: "system.shutdown",
      outcome: "success",
      actorId: "system",
      tenantId: "",
      reason: "Graceful shutdown",
      policyHash: policyEngine.getHash(),
    });
    await auditLogger.flush();
    await auditLogger.close();
    if (isTracingEnabled()) {
      await shutdownTelemetry();
      logger.info("[bootstrap] Telemetry flushed");
    }
    await memoryStore.adapter.persist?.();
    logger.info("[bootstrap] Memory persisted");
  };

  // Register process-level cleanup
  let _shuttingDown = false;
  const cleanup = async () => {
    if (_shuttingDown) return;
    _shuttingDown = true;
    try {
      await shutdown();
    } catch {}
    process.exit(0);
  };
  process.on("SIGTERM", cleanup);
  process.on("SIGINT", cleanup);
  process.on("exit", () => {
    // Synchronous best-effort for normal exit
    try {
      memoryStore.adapter.persist?.();
    } catch {}
  });

  return {
    orchestrator,
    configService,
    modelConfig,
    eventBus,
    logger,
    memoryStore,
    secretsManager,
    enterpriseConfig,
    policyEngine,
    auditLogger,
    tenantRegistry,
    quotaManager,
    shutdown,
  };
}
