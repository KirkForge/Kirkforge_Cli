import { HealthServer } from "@kirkforge/orchestrator/health-server";
import type { Orchestrator } from "@kirkforge/orchestrator";
import type { EventBus } from "@kirkforge/core-events";
import type { AuditLogger } from "@kirkforge/core-events";
import type { Logger } from "@kirkforge/core-logging";
import type { PolicyEngine } from "@kirkforge/core-policy";
import type { EnterpriseConfig } from "@kirkforge/core-enterprise";
import type { OidcConfig, GroupRoleMapping, Role } from "@kirkforge/core-rbac";
import { readFileSync, existsSync } from "node:fs";
import { EventLogger } from "@kirkforge/orchestrator/event-log";

export interface ServeOptions {
  orchestrator: Orchestrator;
  eventBus: EventBus;
  logger?: Logger;
  /** Enterprise config from bootstrap. If not provided, dev-mode defaults are used. */
  enterpriseConfig?: EnterpriseConfig;
  /** Policy engine from bootstrap. If not provided, default-deny is used. */
  policyEngine?: PolicyEngine;
  /** Audit logger from bootstrap. If not provided, a no-op logger is used. */
  auditLogger?: AuditLogger;
  /** Daemon config from CLI flags / config file / env vars. */
  config?: DaemonConfig;
}

/**
 * Daemon-level configuration. Distinct from `HealthServerConfig` — this is
 * the CLI-side surface that wires user input into the health server.
 *
 * Resolution order (highest priority wins):
 *   1. CLI flags (`--port`, `--host`, ...)
 *   2. `--config` JSON file
 *   3. Environment variables (`KIRKFORGE_*`)
 *   4. Defaults
 */
export interface DaemonConfig {
  port?: number;
  host?: string;
  apiKey?: string;
  rateLimitPerSec?: number;
  rateLimitPerSecPerTenant?: number;
  corsOrigin?: string;
  requireAuth?: boolean;
  /** Path to TLS cert PEM. Both cert and key must be set together. */
  tlsCertPath?: string;
  /** Path to TLS key PEM. Both cert and key must be set together. */
  tlsKeyPath?: string;
}

/**
 * Start the daemon with health-check HTTP server.
 * Enterprise controls (enterprise mode gate, policy, audit) are wired in
 * createBootstrap. This function focuses on HTTP server setup and lifecycle.
 */
export async function startDaemon(opts: ServeOptions): Promise<void> {
  const log = opts.logger ?? console;
  const cfg = opts.config ?? {};
  const enterpriseConfig = opts.enterpriseConfig ?? {
    enabled: false,
    auth: { configured: false },
    audit: { configured: false, sinkType: "none" as const },
    policy: { configured: false },
    storage: { backend: "memory" as const, durable: false },
    secrets: { providers: ["env"], envOnlyFallback: true },
  };
  const auditLogger =
    opts.auditLogger ??
    ({
      record: async () => true,
      flush: async () => true,
      close: async () => {},
      getSink: () => ({ name: "noop" }),
    } as unknown as AuditLogger);

  if (enterpriseConfig.enabled) {
    log.info?.("[serve] Enterprise mode ENABLED — all controls validated in bootstrap.");
  } else {
    log.info?.("[serve] Running in developer mode.");
  }

  // ── Record serve-specific audit event ─────────────────────────────────
  await auditLogger.record({
    action: "serve.start",
    outcome: "success",
    actorId: "system",
    tenantId: "",
    reason: enterpriseConfig.enabled ? "Enterprise daemon starting" : "Dev daemon starting",
    policyHash: opts.policyEngine?.getHash(),
  });

  // ── OIDC configuration (from enterprise config) ──────────────────────
  let oidcConfig: OidcConfig | undefined;
  if (enterpriseConfig.auth?.oidcIssuer && enterpriseConfig.auth?.oidcAudience) {
    oidcConfig = {
      issuer: enterpriseConfig.auth.oidcIssuer,
      audience: enterpriseConfig.auth.oidcAudience,
    };
    log.info?.(`[serve] OIDC auth configured: issuer=${enterpriseConfig.auth.oidcIssuer}`);
  }

  // ── Group-to-role mapping (from env) ─────────────────────────────────
  const groupRoleMapping = parseGroupRoleMapping(process.env.OIDC_GROUP_ROLE_MAP);

  // ── Resolve TLS config (paths → files) ───────────────────────────────
  let tlsConfig: { cert: string; key: string } | undefined;
  if (cfg.tlsCertPath && cfg.tlsKeyPath) {
    if (!existsSync(cfg.tlsCertPath)) {
      throw new Error(`TLS cert not found: ${cfg.tlsCertPath}`);
    }
    if (!existsSync(cfg.tlsKeyPath)) {
      throw new Error(`TLS key not found: ${cfg.tlsKeyPath}`);
    }
    tlsConfig = {
      cert: readFileSync(cfg.tlsCertPath, "utf-8"),
      key: readFileSync(cfg.tlsKeyPath, "utf-8"),
    };
    log.info?.(`[serve] TLS configured: cert=${cfg.tlsCertPath}, key=${cfg.tlsKeyPath}`);
  }

  // ── Health server with RBAC + policy ──────────────────────────────────
  const healthServer = new HealthServer(opts.orchestrator, {
    port: cfg.port,
    host: cfg.host,
    apiKey: cfg.apiKey ?? process.env.HEALTH_API_KEY ?? undefined,
    oidcConfig,
    groupRoleMapping,
    auditLogger,
    policyEngine: opts.policyEngine,
    rateLimitPerSec: cfg.rateLimitPerSec,
    rateLimitPerSecPerTenant: cfg.rateLimitPerSecPerTenant,
    corsOrigin: cfg.corsOrigin,
    requireAuth: cfg.requireAuth,
    tls: tlsConfig,
  });

  // Wire event audit log if HMAC secret is configured
  const eventLogSecret = process.env.EVENT_LOG_HMAC_SECRET;
  if (eventLogSecret) {
    new EventLogger(opts.eventBus, eventLogSecret);
    log.info?.("[serve] Event audit log enabled (HMAC-signed)");
  }

  await healthServer.start();

  // ── Startup readiness validation ──────────────────────────────────────
  // Verify all critical subsystems are operational before accepting traffic.
  const startupChecks: Array<{ name: string; ok: boolean; detail?: string }> = [];

  // Check 1: Orchestrator health
  try {
    const health = opts.orchestrator.healthCheck();
    startupChecks.push({
      name: "orchestrator",
      ok: health.status === "healthy",
      detail: `status=${health.status}`,
    });
  } catch (e) {
    startupChecks.push({
      name: "orchestrator",
      ok: false,
      detail: e instanceof Error ? e.message : String(e),
    });
  }

  // Check 2: EventBus running
  try {
    const running = opts.eventBus.running;
    startupChecks.push({
      name: "eventBus",
      ok: running,
      detail: running ? "running" : "stopped",
    });
  } catch (e) {
    startupChecks.push({
      name: "eventBus",
      ok: false,
      detail: e instanceof Error ? e.message : String(e),
    });
  }

  // Check 3: Memory store
  try {
    const stats = await opts.orchestrator.getStats();
    startupChecks.push({
      name: "memory",
      ok: true,
      detail: `entries=${stats.memoryEntries ?? 0}`,
    });
  } catch (e) {
    startupChecks.push({
      name: "memory",
      ok: false,
      detail: e instanceof Error ? e.message : String(e),
    });
  }

  // Check 4: Enterprise controls (if enabled)
  if (enterpriseConfig.enabled) {
    startupChecks.push({
      name: "enterprise",
      ok: true,
      detail: "controls validated in bootstrap",
    });
  }

  const allPassed = startupChecks.every((c) => c.ok);
  for (const check of startupChecks) {
    const icon = check.ok ? "✓" : "✗";
    log.info?.(
      `[serve] ${icon} ${check.name}: ${check.ok ? "ok" : "FAIL"}${check.detail ? ` (${check.detail})` : ""}`,
    );
  }

  if (!allPassed) {
    log.error?.("[serve] Startup validation FAILED — not marking as ready");
    // Still start the server, but don't mark as ready so /readyz returns 503
  } else {
    healthServer.ready = true;
    log.info?.("[serve] KirkForge daemon ready — health server listening");
  }

  const gracefulStop = async (signal: string) => {
    log.info?.(`\n[serve] Received ${signal} — shutting down gracefully...`);
    healthServer.ready = false;

    // Record shutdown audit event
    try {
      await auditLogger.record({
        action: "serve.shutdown",
        outcome: "success",
        actorId: "system",
        tenantId: "",
        reason: `${signal} received`,
      });
      await auditLogger.flush();
      await auditLogger.close();
    } catch (e) {
      log.warn?.(
        `[serve] Audit flush during shutdown: ${e instanceof Error ? e.message : String(e)}`,
      );
    }

    // Drain in-flight HTTP requests with timeout
    try {
      await healthServer.stop();
    } catch (e) {
      log.warn?.(`[serve] Health server stop error: ${e instanceof Error ? e.message : String(e)}`);
    }

    // Flush event bus
    try {
      await opts.eventBus.gracefulShutdown(5000);
    } catch {
      // Best-effort drain
    }

    log.info?.("[serve] Shutdown complete");
    process.exit(0);
  };

  process.on("SIGTERM", () => gracefulStop("SIGTERM"));
  process.on("SIGINT", () => gracefulStop("SIGINT"));
}

/**
 * Parse OIDC_GROUP_ROLE_MAP from env.
 * Format: "admin:admins,operator:operators,developer:developers,viewer:viewers"
 * Maps OIDC group names to KirkForge roles.
 */
const VALID_ROLES = new Set(["admin", "operator", "developer", "viewer"]);

function parseGroupRoleMapping(envValue: string | undefined): GroupRoleMapping | undefined {
  if (!envValue) return undefined;
  const mapping: GroupRoleMapping = {};
  for (const pair of envValue.split(",")) {
    const [role, group] = pair.split(":").map((s) => s.trim());
    if (role && group) {
      if (!VALID_ROLES.has(role)) {
        throw new Error(
          `Invalid role "${role}" in OIDC_GROUP_ROLE_MAP. Valid roles: admin, operator, developer, viewer`,
        );
      }
      mapping[group] = role as Role;
    }
  }
  return Object.keys(mapping).length > 0 ? mapping : undefined;
}
