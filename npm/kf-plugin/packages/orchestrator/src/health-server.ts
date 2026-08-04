import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { createServer as createHttpsServer } from "node:https";
import { readFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";
import { randomBytes } from "node:crypto";
import type { Orchestrator } from "./index.js";
import { KirkForgeError } from "@kirkforge/core-errors";
import type { OidcConfig, GroupRoleMapping } from "@kirkforge/core-rbac";
import type { AuditLogger } from "@kirkforge/core-events";
import type { PolicyEngine } from "@kirkforge/core-policy";
import type { Logger } from "@kirkforge/core-logging";
import {
  type HealthServerConfig,
  type RateBucket,
  type InFlightRequest,
  ENDPOINT_PERMISSIONS,
} from "./health-server-shared.js";
import { sendError } from "./health-server/response.js";
import { resolveActor, checkPermission, normalizeUrl } from "./health-server/auth.js";
import { checkRateLimit, consumeAndLimitBody } from "./health-server/rate-limit.js";
import { routeV1, handleHealthz, handleReadyz, handleMetricsJson, handleMetricsPrometheus } from "./health-server/handlers.js";

const DEFAULT_REQUEST_TIMEOUT_MS = 30_000;
const DEFAULT_MAX_BODY_BYTES = 1024 * 1024;
const DEFAULT_DRAIN_TIMEOUT_MS = 10_000;

const SECURITY_HEADERS: Record<string, string> = {
  "Content-Security-Policy": "default-src 'none'; frame-ancestors 'none'",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
  "X-XSS-Protection": "0",
  "Referrer-Policy": "no-referrer",
  "Cache-Control": "no-store, no-cache, must-revalidate",
  "Strict-Transport-Security": "max-age=63072000; includeSubDomains; preload",
};

/**
 * Lightweight HTTP health-check server for daemon/bot deployment.
 * Exposes /healthz (liveness), /readyz (readiness), /metrics (json),
 * and /v1/metrics (Prometheus text format).
 * Uses only Node built-ins — no Express dependency.
 *
 * Method bodies live in `health-server/{auth,handlers,rate-limit,response}.ts`;
 * this file is the orchestration shell.
 */
export class HealthServer {
  // Auth config
  apiKey: string | null;
  oidcConfig?: OidcConfig;
  groupRoleMapping?: GroupRoleMapping;
  auditLogger?: AuditLogger;
  policyEngine?: PolicyEngine;
  logger?: Logger;
  requireAuth: boolean = false;
  allowApiKeyFallbackWithOidc: boolean = false;

  // Rate limiting
  rateLimitPerSec: number;
  rateLimitPerSecPerTenant: number;
  buckets = new Map<string, RateBucket>();
  tenantBuckets = new Map<string, RateBucket>();

  // Counters
  requestCount = 0;
  authSuccessCount = 0;
  authFailureCount = 0;
  policyDenyCount = 0;

  // Lifecycle
  inFlight = new Map<IncomingMessage, InFlightRequest>();
  shuttingDown = false;
  drainResolve: (() => void) | null = null;

  private server: ReturnType<typeof createServer> | null = null;
  private _ready = false;
  private cleanupTimer: ReturnType<typeof setInterval> | null = null;
  private requestTimeoutMs: number;
  private maxBodyBytes: number;
  private drainTimeoutMs: number;
  private corsOrigin?: string;
  private tlsConfig?: { cert: string; key: string };

  constructor(
    public orchestrator: Orchestrator,
    public config: HealthServerConfig = {},
  ) {
    this.apiKey = config.apiKey ?? process.env.HEALTH_API_KEY ?? null;
    this.requireAuth = config.requireAuth ?? false;
    this.allowApiKeyFallbackWithOidc = config.allowApiKeyFallbackWithOidc ?? false;
    this.rateLimitPerSec = config.rateLimitPerSec ?? 20;
    this.oidcConfig = config.oidcConfig;
    this.groupRoleMapping = config.groupRoleMapping;
    this.auditLogger = config.auditLogger;
    this.policyEngine = config.policyEngine;
    this.requestTimeoutMs = config.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
    this.maxBodyBytes = config.maxBodyBytes ?? DEFAULT_MAX_BODY_BYTES;
    this.drainTimeoutMs = config.drainTimeoutMs ?? DEFAULT_DRAIN_TIMEOUT_MS;
    this.corsOrigin = config.corsOrigin;
    this.rateLimitPerSecPerTenant = config.rateLimitPerSecPerTenant ?? 0;
    if (config.tls) {
      const certPath = resolve(config.tls.cert);
      const keyPath = resolve(config.tls.key);
      if (!existsSync(certPath)) throw new Error(`TLS cert file not found: ${certPath}`);
      if (!existsSync(keyPath)) throw new Error(`TLS key file not found: ${keyPath}`);
      this.tlsConfig = { cert: certPath, key: keyPath };
    }
  }

  get ready(): boolean {
    return this._ready;
  }
  set ready(v: boolean) {
    this._ready = v;
  }

  get inFlightCount(): number {
    return this.inFlight.size;
  }

  start(): Promise<void> {
    const port = this.config.port ?? parseInt(process.env.HEALTH_PORT ?? "9090", 10);
    const host = this.config.host ?? process.env.HEALTH_HOST ?? "0.0.0.0";
    this.cleanupTimer = setInterval(() => {
      const now = Date.now();
      for (const [key, bucket] of this.buckets) {
        if (now - bucket.lastRefill > 60000) this.buckets.delete(key);
        if (now - bucket.lastRefill > 60000) this.tenantBuckets.delete(key);
      }
    }, 30000);
    return new Promise((resolve, reject) => {
      const requestHandler = async (req: IncomingMessage, res: ServerResponse) => {
        const correlationId =
          (req.headers["x-request-id"] as string | undefined) ??
          (req.headers["x-correlation-id"] as string | undefined) ??
          randomBytes(8).toString("hex");
        try {
          res.setHeader("X-Request-Id", correlationId);
          res.setHeader("X-Correlation-Id", correlationId);

          if (this.corsOrigin) {
            res.setHeader("Access-Control-Allow-Origin", this.corsOrigin);
            res.setHeader("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
            res.setHeader(
              "Access-Control-Allow-Headers",
              "Authorization, X-Request-Id, X-Correlation-Id, traceparent",
            );
            res.setHeader("Access-Control-Max-Age", "86400");
          }

          const method = req.method?.toUpperCase() ?? "GET";
          if (method === "OPTIONS") {
            res.writeHead(204);
            res.end();
            return;
          }
          if (method !== "GET" && method !== "HEAD") {
            sendError(
              res,
              new KirkForgeError("METHOD_NOT_ALLOWED", `${method} is not allowed`),
              correlationId,
            );
            return;
          }
          if (this.shuttingDown) {
            res.writeHead(503, { "Content-Type": "application/json", ...SECURITY_HEADERS });
            res.end(
              JSON.stringify({
                error: {
                  code: "SERVICE_UNAVAILABLE",
                  message: "Server is shutting down",
                  status: 503,
                  requestId: correlationId,
                  timestamp: new Date().toISOString(),
                },
              }),
            );
            return;
          }

          this.inFlight.set(req, { req, res, startedAt: Date.now() });
          this.requestCount++;

          const bodyCheck = await consumeAndLimitBody(
            this,
            this.maxBodyBytes,
            req,
            res,
            correlationId,
          );
          if (!bodyCheck) return;

          for (const [k, v] of Object.entries(SECURITY_HEADERS)) {
            res.setHeader(k, v);
          }
          const traceparent = req.headers["traceparent"];
          if (typeof traceparent === "string" && traceparent.startsWith("00-")) {
            res.setHeader("traceparent", traceparent);
          }

          const authResult = await resolveActor(this, req, res);
          if (!authResult) return;
          if (!checkRateLimit(this, req, res, authResult.actor)) return;

          const url = req.url ?? "/";
          const normalized = normalizeUrl(url);
          if (!checkPermission(this, authResult.actor, normalized, authResult.tokenId, req, res, ENDPOINT_PERMISSIONS)) {
            return;
          }

          if (url.startsWith("/v1/")) return routeV1(url.slice(4), this as unknown as Parameters<typeof routeV1>[1], res);

          if (url === "/healthz") {
            return handleHealthz(this as unknown as Parameters<typeof handleHealthz>[0], res);
          }
          if (url === "/readyz") {
            return handleReadyz(this as unknown as Parameters<typeof handleReadyz>[0], res);
          }
          if (url === "/metrics") {
            return handleMetricsJson(this as unknown as Parameters<typeof handleMetricsJson>[0], res);
          }
          if (url === "/metrics/prometheus") {
            return handleMetricsPrometheus(this as unknown as Parameters<typeof handleMetricsPrometheus>[0], res);
          }
          sendError(res, new KirkForgeError("NOT_FOUND", `Unknown path: ${url}`), correlationId);
        } catch (err: unknown) {
          sendError(res, err instanceof Error ? err : new Error(String(err)), correlationId);
        } finally {
          const startedAt = this.inFlight.get(req)?.startedAt ?? Date.now();
          const durationMs = Date.now() - startedAt;
          const accessLog = {
            method: req.method,
            path: req.url,
            status: res.statusCode,
            durationMs,
            actor: (req as any).__actorId ?? "anonymous",
            correlationId,
            ip: req.socket?.remoteAddress,
            userAgent: req.headers["user-agent"],
          };
          this.logger?.info(
            `[health-server] ${accessLog.method} ${accessLog.path} ${accessLog.status} ${accessLog.durationMs}ms actor=${accessLog.actor}`,
          );
          this.inFlight.delete(req);
          if (this.shuttingDown && this.inFlight.size === 0 && this.drainResolve) {
            this.drainResolve();
          }
        }
      };

      if (this.tlsConfig) {
        const cert = readFileSync(this.tlsConfig.cert, "utf-8");
        const key = readFileSync(this.tlsConfig.key, "utf-8");
        this.server = createHttpsServer({ cert, key }, requestHandler);
      } else {
        this.server = createServer(requestHandler);
      }

      this.server.timeout = this.requestTimeoutMs;
      this.server.requestTimeout = this.requestTimeoutMs;
      this.server.headersTimeout = this.requestTimeoutMs + 5000;

      this.server.on("error", (err) => {
        this.logger?.error(`[health-server] Failed to start: ${err.message}`);
        reject(err);
      });
      this.server.listen(port, host, () => {
        this.logger?.info(
          `[health-server] Listening on ${this.tlsConfig ? "https" : "http"}://${host}:${port}${this.apiKey ? " (auth enabled)" : ""}`,
        );
        resolve();
      });
    });
  }

  async stop(): Promise<void> {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
      this.cleanupTimer = null;
    }
    this.shuttingDown = true;
    this.logger?.info("[health-server] Shutting down — draining in-flight requests");

    return new Promise((resolve, reject) => {
      if (!this.server) {
        this.shuttingDown = false;
        resolve();
        return;
      }

      const drainPromise = new Promise<void>((r) => {
        if (this.inFlight.size === 0) {
          r();
        } else {
          this.drainResolve = r;
        }
      });
      const timeoutPromise = new Promise<void>((r) => setTimeout(r, this.drainTimeoutMs));

      Promise.race([drainPromise, timeoutPromise]).then(() => {
        this.server!.close((err) => {
          this.shuttingDown = false;
          this.drainResolve = null;
          if (err) {
            this.logger?.error(`[health-server] Error closing: ${err.message}`);
            reject(err);
          } else {
            this.logger?.info("[health-server] Stopped — all requests drained");
            resolve();
          }
        });
      });
    });
  }
}
