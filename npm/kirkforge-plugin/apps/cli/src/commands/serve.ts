import type { Command } from "commander";
import { readFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";
import { createBootstrap } from "../bootstrap.js";
import { startDaemon, type DaemonConfig } from "../serve.js";

/**
 * Parse a JSON config file into a partial `DaemonConfig`. The config file
 * can contain any of the DaemonConfig fields; missing fields fall back to
 * CLI flags, then to environment variables, then to defaults.
 *
 * Recognized env vars (also picked up directly from process.env):
 *   - KIRKFORGE_PORT, KIRKFORGE_HOST
 *   - KIRKFORGE_TLS_CERT, KIRKFORGE_TLS_KEY   (paths to PEM files)
 *   - KIRKFORGE_API_KEY
 *   - KIRKFORGE_RATE_LIMIT_PER_SEC, KIRKFORGE_RATE_LIMIT_PER_SEC_PER_TENANT
 *   - KIRKFORGE_CORS_ORIGIN
 *   - KIRKFORGE_REQUIRE_AUTH
 */
function loadConfigFile(path: string): Partial<DaemonConfig> {
  if (!existsSync(path)) {
    throw new Error(`Config file not found: ${path}`);
  }
  const raw = readFileSync(path, "utf-8");
  const parsed = JSON.parse(raw) as Record<string, unknown>;
  const out: Partial<DaemonConfig> = {};
  if (typeof parsed.port === "number") out.port = parsed.port;
  if (typeof parsed.host === "string") out.host = parsed.host;
  if (typeof parsed.apiKey === "string") out.apiKey = parsed.apiKey;
  if (typeof parsed.rateLimitPerSec === "number") out.rateLimitPerSec = parsed.rateLimitPerSec;
  if (typeof parsed.rateLimitPerSecPerTenant === "number") {
    out.rateLimitPerSecPerTenant = parsed.rateLimitPerSecPerTenant;
  }
  if (typeof parsed.corsOrigin === "string") out.corsOrigin = parsed.corsOrigin;
  if (typeof parsed.requireAuth === "boolean") out.requireAuth = parsed.requireAuth;
  if (typeof parsed.tlsCertPath === "string" && typeof parsed.tlsKeyPath === "string") {
    out.tlsCertPath = parsed.tlsCertPath;
    out.tlsKeyPath = parsed.tlsKeyPath;
  }
  return out;
}

function buildConfigFromEnv(): Partial<DaemonConfig> {
  const out: Partial<DaemonConfig> = {};
  if (process.env.KIRKFORGE_PORT) {
    const n = Number(process.env.KIRKFORGE_PORT);
    if (Number.isFinite(n) && n > 0) out.port = n;
  }
  if (process.env.KIRKFORGE_HOST) out.host = process.env.KIRKFORGE_HOST;
  if (process.env.KIRKFORGE_API_KEY) out.apiKey = process.env.KIRKFORGE_API_KEY;
  if (process.env.KIRKFORGE_RATE_LIMIT_PER_SEC) {
    const n = Number(process.env.KIRKFORGE_RATE_LIMIT_PER_SEC);
    if (Number.isFinite(n) && n > 0) out.rateLimitPerSec = n;
  }
  if (process.env.KIRKFORGE_RATE_LIMIT_PER_SEC_PER_TENANT) {
    const n = Number(process.env.KIRKFORGE_RATE_LIMIT_PER_SEC_PER_TENANT);
    if (Number.isFinite(n) && n > 0) out.rateLimitPerSecPerTenant = n;
  }
  if (process.env.KIRKFORGE_CORS_ORIGIN) out.corsOrigin = process.env.KIRKFORGE_CORS_ORIGIN;
  if (process.env.KIRKFORGE_REQUIRE_AUTH) {
    out.requireAuth = process.env.KIRKFORGE_REQUIRE_AUTH === "1" ||
      process.env.KIRKFORGE_REQUIRE_AUTH.toLowerCase() === "true";
  }
  if (process.env.KIRKFORGE_TLS_CERT && process.env.KIRKFORGE_TLS_KEY) {
    out.tlsCertPath = process.env.KIRKFORGE_TLS_CERT;
    out.tlsKeyPath = process.env.KIRKFORGE_TLS_KEY;
  }
  return out;
}

export function registerServe(program: Command): void {
  program
    .command("serve")
    .description(
      "Start daemon with health-check HTTP server (blocks until SIGTERM)\n\n" +
        "Config precedence (highest to lowest): --config file > --flag value > env var > default",
    )
    .option(
      "-c, --config <path>",
      "Path to a JSON config file (see DaemonConfig in serve.ts for fields)",
    )
    .option("-p, --port <port>", "HTTP port (default 9090; env KIRKFORGE_PORT)")
    .option("-h, --host <host>", "Bind address (default 0.0.0.0; env KIRKFORGE_HOST)")
    .option("--api-key <key>", "Static API key for bearer auth (env KIRKFORGE_API_KEY)")
    .option(
      "--rate-limit <n>",
      "Per-IP requests per second (default 20; env KIRKFORGE_RATE_LIMIT_PER_SEC)",
    )
    .option(
      "--rate-limit-tenant <n>",
      "Per-tenant requests per second (default 0 = disabled; env KIRKFORGE_RATE_LIMIT_PER_SEC_PER_TENANT)",
    )
    .option(
      "--cors-origin <origin>",
      "Allowed CORS origin (env KIRKFORGE_CORS_ORIGIN). Use '*' for any.",
    )
    .option(
      "--require-auth",
      "Require auth for all endpoints (env KIRKFORGE_REQUIRE_AUTH=1)",
    )
    .option(
      "--tls-cert <path>",
      "Path to TLS certificate PEM file (env KIRKFORGE_TLS_CERT). Requires --tls-key.",
    )
    .option(
      "--tls-key <path>",
      "Path to TLS private key PEM file (env KIRKFORGE_TLS_KEY). Requires --tls-cert.",
    )
    .action(async (opts) => {
      // 1. Lowest-priority: env vars
      const envConfig = buildConfigFromEnv();
      // 2. Mid-priority: --config file
      const fileConfig = opts.config ? loadConfigFile(resolve(opts.config)) : {};
      // 3. Highest-priority: explicit CLI flags
      const flagConfig: Partial<DaemonConfig> = {};
      if (opts.port) flagConfig.port = Number(opts.port);
      if (opts.host) flagConfig.host = opts.host;
      if (opts.apiKey) flagConfig.apiKey = opts.apiKey;
      if (opts.rateLimit) flagConfig.rateLimitPerSec = Number(opts.rateLimit);
      if (opts.rateLimitTenant) flagConfig.rateLimitPerSecPerTenant = Number(opts.rateLimitTenant);
      if (opts.corsOrigin) flagConfig.corsOrigin = opts.corsOrigin;
      if (opts.requireAuth) flagConfig.requireAuth = true;
      if (opts.tlsCert) flagConfig.tlsCertPath = opts.tlsCert;
      if (opts.tlsKey) flagConfig.tlsKeyPath = opts.tlsKey;

      const daemonConfig: DaemonConfig = {
        ...envConfig,
        ...fileConfig,
        ...flagConfig,
      };

      // Validate TLS pairing
      if ((daemonConfig.tlsCertPath && !daemonConfig.tlsKeyPath) ||
          (!daemonConfig.tlsCertPath && daemonConfig.tlsKeyPath)) {
        throw new Error("Both --tls-cert and --tls-key must be provided together");
      }

      const {
        orchestrator,
        eventBus,
        shutdown: _shutdown,
        enterpriseConfig,
        policyEngine,
        auditLogger,
        logger,
      } = await createBootstrap({});
      await startDaemon({
        orchestrator,
        eventBus,
        enterpriseConfig,
        policyEngine,
        auditLogger,
        logger,
        config: daemonConfig,
      });
    });
}
