import type { IncomingMessage, ServerResponse } from "node:http";
import type { HealthServerInternals } from "../health-server-shared.js";

/**
 * Permissive context type for the v1 endpoint handlers. The HealthServer
 * class satisfies this shape; we keep the parameters loose so the
 * handlers can be passed `this` from a request handler without every
 * field on the class being public.
 */
 
export interface HandlerContext extends HealthServerInternals {
  orchestrator: any;
  // Optional non-required view fields used by some handlers
  ready?: boolean;
   
  config?: any;
}

/** GET /v1/policy — return current policy + hash. */
export function handlePolicy(ctx: HandlerContext, res: ServerResponse): void {
  if (!ctx.policyEngine) {
    res.writeHead(404, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: "policy_engine_not_configured" }));
    return;
  }
   
  const policy = (ctx.policyEngine as any).getPolicy?.();
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(
    JSON.stringify({
      policy,
      hash: (ctx.policyEngine as any).getHash?.(),
    }),
  );
}

/** GET /v1/audit — pointer to the audit-verify CLI command. */
export function handleAuditVerify(_req: IncomingMessage, res: ServerResponse): void {
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(
    JSON.stringify({
      status: "available",
      message: "Use `kirkforge audit-verify --file <path>` to verify audit chain integrity",
    }),
  );
}

/** GET /v1/tenants — placeholder listing (full API planned). */
export function handleTenants(res: ServerResponse): void {
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(
    JSON.stringify({
      tenants: [],
      message: "Tenant management via API is planned for a future release",
    }),
  );
}

/** GET /v1/quotas — placeholder quota status. */
export function handleQuotas(res: ServerResponse): void {
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(
    JSON.stringify({
      quotas: { default: "Per-tenant quota management via API is active" },
      message:
        "Set per-tenant quotas via the QuotaManager. API CRUD is planned for a future release.",
    }),
  );
}

const OPENAPI_SPEC = {
  openapi: "3.0.3",
  info: {
    title: "KirkForge Health & Metrics API",
    version: "1.0.0",
    description: "Deterministic verification and routing layer for coding agents",
  },
  servers: [{ url: "/v1", description: "Versioned API" }],
  paths: {
    "/healthz": {
      get: {
        operationId: "getLiveness",
        summary: "Liveness probe",
        description: "Returns 200 if the service is running. Returns 503 if shutting down.",
        tags: ["health"],
        responses: {
          "200": {
            description: "Service is healthy",
            content: {
              "application/json": {
                schema: {
                  type: "object",
                  properties: { status: { type: "string", enum: ["healthy"] } },
                },
              },
            },
          },
          "503": { description: "Service is unhealthy or shutting down" },
        },
      },
    },
    "/readyz": {
      get: {
        operationId: "getReadiness",
        summary: "Readiness probe",
        description:
          "Returns 200 if the service is ready to accept requests. Checks event bus and memory store health.",
        tags: ["health"],
        responses: {
          "200": {
            description: "Service is ready",
            content: {
              "application/json": {
                schema: {
                  type: "object",
                  properties: {
                    status: { type: "string", enum: ["ready"] },
                    checks: { type: "object" },
                  },
                },
              },
            },
          },
          "503": { description: "Service is not ready" },
        },
      },
    },
    "/metrics": {
      get: {
        operationId: "getMetricsPrometheus",
        summary: "Prometheus metrics (text format)",
        description: "Returns metrics in Prometheus text exposition format.",
        tags: ["metrics"],
        responses: {
          "200": { description: "Prometheus text metrics", content: { "text/plain": {} } },
        },
      },
    },
    "/metrics/json": {
      get: {
        operationId: "getMetricsJson",
        summary: "JSON metrics",
        description: "Returns metrics as a JSON object.",
        tags: ["metrics"],
        responses: {
          "200": { description: "JSON metrics object", content: { "application/json": {} } },
        },
      },
    },
    "/policy": {
      get: {
        operationId: "getPolicy",
        summary: "Current policy configuration",
        description:
          "Returns the active policy and its hash. Requires admin:policy permission.",
        tags: ["admin"],
        responses: {
          "200": { description: "Policy object", content: { "application/json": {} } },
          "404": { description: "Policy engine not configured" },
        },
      },
    },
    "/audit": {
      get: {
        operationId: "getAuditStatus",
        summary: "Audit chain status",
        description:
          "Returns audit log integrity verification status. Requires admin:audit_export permission.",
        tags: ["admin"],
        responses: {
          "200": { description: "Audit status", content: { "application/json": {} } },
        },
      },
    },
    "/tenants": {
      get: {
        operationId: "listTenants",
        summary: "List tenants",
        description: "Returns registered tenants. Requires admin:tenant permission.",
        tags: ["admin"],
        responses: {
          "200": { description: "Tenant list", content: { "application/json": {} } },
        },
      },
    },
    "/openapi": {
      get: {
        operationId: "getOpenApi",
        summary: "OpenAPI specification",
        description: "Returns this OpenAPI 3.0 schema document.",
        tags: ["meta"],
        responses: {
          "200": {
            description: "OpenAPI 3.0 JSON schema",
            content: { "application/json": {} },
          },
        },
      },
    },
    "/quotas": {
      get: {
        operationId: "getQuotas",
        summary: "Tenant quota status",
        description:
          "Returns per-tenant quota configuration and usage. Requires admin:tenant permission.",
        tags: ["admin"],
        responses: {
          "200": { description: "Quota status", content: { "application/json": {} } },
        },
      },
    },
  },
  components: {
    securitySchemes: {
      bearerAuth: {
        type: "http",
        scheme: "bearer",
        bearerFormat: "JWT or API key",
        description: "Bearer token (OIDC JWT or static API key)",
      },
    },
  },
  security: [{ bearerAuth: [] }],
};

/** GET /v1/openapi — serve the OpenAPI 3.0 spec. */
export function handleOpenApi(res: ServerResponse): void {
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(JSON.stringify(OPENAPI_SPEC, null, 2));
}

/** v1 router: dispatches to per-path handlers. */
export function routeV1(path: string, ctx: HandlerContext, res: ServerResponse): void {
  switch (path) {
    case "healthz":
      return handleHealthz(ctx, res);
    case "readyz":
      return handleReadyz(ctx, res);
    case "metrics":
      return handleMetricsPrometheus(ctx, res);
    case "metrics/json":
      return handleMetricsJson(ctx, res);
    case "policy":
      return handlePolicy(ctx, res);
    case "audit":
      return handleAuditVerify({} as IncomingMessage, res);
    case "tenants":
      return handleTenants(res);
    case "openapi":
      return handleOpenApi(res);
    case "quotas":
      return handleQuotas(res);
    default:
      res.writeHead(404, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          error: "not_found",
          available: [
            "/v1/healthz",
            "/v1/readyz",
            "/v1/metrics",
            "/v1/metrics/json",
            "/v1/openapi",
            "/v1/policy",
            "/v1/audit",
            "/v1/tenants",
            "/v1/quotas",
          ],
        }),
      );
  }
}

export function handleHealthz(ctx: HandlerContext, res: ServerResponse): void {
   
  const health = ctx.orchestrator.healthCheck();
  if (health.status === "shutting_down") {
    res.writeHead(503, { "Content-Type": "application/json" });
    res.end(
      JSON.stringify({ status: "unhealthy", stats: health.stats, providers: health.providers }),
    );
    return;
  }
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(
    JSON.stringify({ status: "healthy", stats: health.stats, providers: health.providers }),
  );
}

export function handleReadyz(ctx: HandlerContext, res: ServerResponse): void {
  if (ctx.ready === false) {
    res.writeHead(503, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ status: "not_ready" }));
    return;
  }
   
  const health = ctx.orchestrator.healthCheck();
  if (health.status === "shutting_down") {
    res.writeHead(503, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ status: "not_ready", reason: "shutting_down" }));
    return;
  }
  const checks: Record<string, { ok: boolean; detail?: string }> = {};
  const eventBus = health.eventBus;
  if (eventBus && typeof eventBus === "object") {
     
    const eb = eventBus as any;
    checks.eventBus = {
      ok: eb.running === true,
      detail: eb.running
        ? `inflight=${eb.inflight ?? 0}, buffer=${eb.bufferSize ?? 0}`
        : "not running",
    };
  } else {
    checks.eventBus = { ok: true, detail: "not configured" };
  }
  const memStatus = health.memory;
  if (memStatus === "connected") {
    checks.memoryStore = { ok: true };
  } else if (memStatus === "none") {
    checks.memoryStore = { ok: true, detail: "not configured" };
  } else {
    checks.memoryStore = { ok: false, detail: String(memStatus) };
  }
  const allOk = Object.values(checks).every((c) => c.ok);
  if (!allOk) {
    res.writeHead(503, { "Content-Type": "application/json" });
    res.end(
      JSON.stringify({
        status: "not_ready",
        checks,
        stats: health.stats,
        providers: health.providers,
      }),
    );
    return;
  }
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(
    JSON.stringify({
      status: "ready",
      checks,
      stats: health.stats,
      providers: health.providers,
    }),
  );
}

export function handleMetricsJson(ctx: HandlerContext, res: ServerResponse): void {
   
  const stats = ctx.orchestrator.getStats();
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(JSON.stringify(stats));
}

export function handleMetricsPrometheus(ctx: HandlerContext, res: ServerResponse): void {
   
  const stats = ctx.orchestrator.getStats();
   
  const health = ctx.orchestrator.healthCheck();
  const lines: string[] = [];
  const num = (v: unknown): number => (typeof v === "number" ? v : 0);
  const escapePromLabel = (v: string): string =>
    v.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n");
  const gauge = (name: string, help: string, value: number, labels?: Record<string, string>) => {
    lines.push(`# HELP ${name} ${help}`);
    lines.push(`# TYPE ${name} gauge`);
    const labelStr = labels
      ? `{${Object.entries(labels)
          .map(([k, v]) => `${k}="${escapePromLabel(v)}"`)
          .join(",")}}`
      : "";
    lines.push(`${name}${labelStr} ${value}`);
  };
  const counter = (
    name: string,
    help: string,
    value: number,
    labels?: Record<string, string>,
  ) => {
    lines.push(`# HELP ${name} ${help}`);
    lines.push(`# TYPE ${name} counter`);
    const labelStr = labels
      ? `{${Object.entries(labels)
          .map(([k, v]) => `${k}="${escapePromLabel(v)}"`)
          .join(",")}}`
      : "";
    lines.push(`${name}${labelStr} ${value}`);
  };
  gauge("kirkforge_up", "Is the KirkForge server up", health.status === "healthy" ? 1 : 0);
  counter(
    "kirkforge_delegations_total",
    "Total number of delegated tasks",
    num(stats.totalDelegations),
  );
  counter("kirkforge_tokens_total", "Total tokens consumed", num(stats.totalTokens));
   
  if (typeof (stats as any).totalErrors === "number")
     
    counter("kirkforge_errors_total", "Total errors", (stats as any).totalErrors);
   
  if (typeof (stats as any).activeTasks === "number")
     
    gauge("kirkforge_active_tasks", "Currently active tasks", (stats as any).activeTasks);
   
  if (typeof (stats as any).memoryEntries === "number")
     
    gauge("kirkforge_memory_store_entries", "Memory store entries", (stats as any).memoryEntries);
   
  if (typeof (stats as any).memorySizeBytes === "number")
     
    gauge(
      "kirkforge_memory_store_size_bytes",
      "Memory store size in bytes",
      (stats as any).memorySizeBytes,
    );
  const memUsage = process.memoryUsage();
  gauge("process_resident_memory_bytes", "Resident memory in bytes", memUsage.rss);
  gauge("process_heap_total_bytes", "Total heap in bytes", memUsage.heapTotal);
  gauge("process_heap_used_bytes", "Used heap in bytes", memUsage.heapUsed);
  gauge("process_uptime_seconds", "Process uptime in seconds", process.uptime());
  counter("kirkforge_auth_success_total", "Total successful auth events", ctx.authSuccessCount);
  counter("kirkforge_auth_failure_total", "Total failed auth events", ctx.authFailureCount);
  counter("kirkforge_policy_deny_total", "Total policy deny events", ctx.policyDenyCount);
  gauge(
    "kirkforge_http_requests_in_flight",
    "Currently processing HTTP requests",
    ctx.inFlight.size,
  );
  counter("kirkforge_http_requests_total", "Total HTTP requests processed", ctx.requestCount);
  if (ctx.rateLimitPerSecPerTenant > 0) {
    gauge(
      "kirkforge_tenant_rate_limit_buckets",
      "Active tenant rate limit buckets",
      ctx.tenantBuckets.size,
    );
  }
  lines.push("# EOF");
  res.writeHead(200, { "Content-Type": "text/plain; version=0.0.4; charset=utf-8" });
  res.end(lines.join("\n") + "\n");
}
