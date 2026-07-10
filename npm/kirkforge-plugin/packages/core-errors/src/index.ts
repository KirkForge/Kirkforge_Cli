export class KirkForgeError extends Error {
  code: string;
  context?: Record<string, unknown>;

  constructor(code: string, message: string, context?: Record<string, unknown>) {
    super(message);
    this.name = "KirkForgeError";
    this.code = code;
    this.context = context;
  }
}

export class ValidationError extends KirkForgeError {
  constructor(message: string, context?: Record<string, unknown>) {
    super("VALIDATION_ERROR", message, context);
    this.name = "ValidationError";
  }
}

export class EventBusError extends KirkForgeError {
  constructor(message: string, context?: Record<string, unknown>) {
    super("EVENT_BUS_ERROR", message, context);
    this.name = "EventBusError";
  }
}

export class BufferOverflowError extends EventBusError {
  constructor(capacity: number, size: number) {
    super("Buffer overflow: cannot emit event", { capacity, size });
    this.name = "BufferOverflowError";
    this.code = "BUFFER_OVERFLOW";
  }
}

export class IdempotencyError extends EventBusError {
  constructor(eventId: string) {
    super("Duplicate event detected", { eventId });
    this.name = "IdempotencyError";
    this.code = "DUPLICATE_EVENT";
  }
}

export class ConfigError extends KirkForgeError {
  constructor(message: string, context?: Record<string, unknown>) {
    super("CONFIG_ERROR", message, context);
    this.name = "ConfigError";
  }
}

export class ToolError extends KirkForgeError {
  constructor(message: string, context?: Record<string, unknown>) {
    super("TOOL_ERROR", message, context);
    this.name = "ToolError";
  }
}

export class TimeoutError extends ToolError {
  constructor(tool: string, timeoutMs: number) {
    super(`Tool ${tool} timed out after ${timeoutMs}ms`, { tool, timeoutMs });
    this.name = "TimeoutError";
    this.code = "TOOL_TIMEOUT";
  }
}

export class CircuitOpenError extends KirkForgeError {
  constructor(circuit: string) {
    super("CIRCUIT_OPEN", `Circuit breaker ${circuit} is open`, { circuit });
    this.name = "CircuitOpenError";
  }
}

export class PipelineHaltedError extends KirkForgeError {
  constructor(reason: string) {
    super("PIPELINE_HALTED", "Pipeline halted", { reason });
    this.name = "PipelineHaltedError";
  }
}

export class HandlerError extends KirkForgeError {
  constructor(handlerName: string, cause: Error) {
    super("HANDLER_ERROR", `Handler ${handlerName} failed: ${cause.message}`, {
      handlerName,
      cause: cause.message,
    });
    this.name = "HandlerError";
  }
}

// ── New enterprise error classes ────────────────────────────────────────

export class AuthError extends KirkForgeError {
  constructor(
    code: "UNAUTHORIZED" | "FORBIDDEN" | "INVALID_TOKEN" | "METHOD_NOT_ALLOWED",
    message: string,
    context?: Record<string, unknown>,
  ) {
    super(code, message, context);
    this.name = "AuthError";
  }
}

export class NotFoundError extends KirkForgeError {
  constructor(
    code: "TASK_NOT_FOUND" | "TENANT_NOT_FOUND" | "RUN_NOT_FOUND" | "METHOD_NOT_FOUND",
    message: string,
    context?: Record<string, unknown>,
  ) {
    super(code, message, context);
    this.name = "NotFoundError";
  }
}

export class RateLimitError extends KirkForgeError {
  constructor(retryAfterSec: number) {
    super("RATE_LIMITED", "Too many requests", { retryAfterSec });
    this.name = "RateLimitError";
  }
}

export class ConcurrencyError extends KirkForgeError {
  constructor(
    code: "CONCURRENT_MODIFICATION" | "TASK_LOCKED",
    message: string,
    context?: Record<string, unknown>,
  ) {
    super(code, message, context);
    this.name = "ConcurrencyError";
  }
}

// ── Error catalog with HTTP status mappings ─────────────────────────────

export type ErrorCategory =
  | "validation"
  | "auth"
  | "permission"
  | "not_found"
  | "conflict"
  | "rate_limit"
  | "timeout"
  | "circuit_open"
  | "infra"
  | "internal"
  | "unavailable";

export const ERROR_CATALOG: Record<
  string,
  { status: number; category: ErrorCategory; description: string }
> = {
  VALIDATION_ERROR: {
    status: 400,
    category: "validation",
    description: "Request or input validation failed",
  },
  INVALID_CONFIG: {
    status: 400,
    category: "validation",
    description: "Configuration is invalid or inconsistent",
  },
  INVALID_WORKSPACE: {
    status: 400,
    category: "validation",
    description: "Workspace path is invalid or inaccessible",
  },
  INVALID_LANGUAGE: {
    status: 400,
    category: "validation",
    description: "Unsupported or unknown language",
  },
  SCHEMA_MISMATCH: {
    status: 422,
    category: "validation",
    description: "Output does not conform to expected schema",
  },
  PATH_TRAVERSAL: {
    status: 400,
    category: "validation",
    description: "Path contains unsafe traversal patterns",
  },
  UNAUTHORIZED: { status: 401, category: "auth", description: "Authentication required" },
  FORBIDDEN: { status: 403, category: "permission", description: "Insufficient permissions" },
  INVALID_TOKEN: {
    status: 401,
    category: "auth",
    description: "Authentication token is invalid or expired",
  },
  NOT_FOUND: { status: 404, category: "not_found", description: "Resource not found" },
  TASK_NOT_FOUND: { status: 404, category: "not_found", description: "Task not found" },
  TENANT_NOT_FOUND: { status: 404, category: "not_found", description: "Tenant not found" },
  RUN_NOT_FOUND: { status: 404, category: "not_found", description: "Run record not found" },
  CONCURRENT_MODIFICATION: {
    status: 409,
    category: "conflict",
    description: "Resource was modified concurrently",
  },
  DUPLICATE_EVENT: {
    status: 409,
    category: "conflict",
    description: "Idempotency: duplicate event detected",
  },
  TASK_LOCKED: {
    status: 423,
    category: "conflict",
    description: "Task is locked by another process",
  },
  PAYLOAD_TOO_LARGE: {
    status: 413,
    category: "validation",
    description: "Request payload exceeds size limit",
  },
  SERVICE_UNAVAILABLE: {
    status: 503,
    category: "unavailable",
    description: "Server is shutting down or unavailable",
  },
  RATE_LIMITED: {
    status: 429,
    category: "rate_limit",
    description: "Too many requests — slow down",
  },
  TOOL_TIMEOUT: {
    status: 504,
    category: "timeout",
    description: "Tool execution exceeded time limit",
  },
  MODEL_TIMEOUT: {
    status: 504,
    category: "timeout",
    description: "Model inference exceeded time limit",
  },
  VALIDATOR_TIMEOUT: {
    status: 504,
    category: "timeout",
    description: "Validator execution exceeded time limit",
  },
  CIRCUIT_OPEN: {
    status: 503,
    category: "circuit_open",
    description: "Circuit breaker is open — service unavailable",
  },
  EVENT_BUS_ERROR: { status: 500, category: "infra", description: "Event bus operation failed" },
  BUFFER_OVERFLOW: {
    status: 503,
    category: "infra",
    description: "Event buffer capacity exceeded",
  },
  MEMORY_ERROR: { status: 500, category: "infra", description: "Memory store operation failed" },
  SECRETS_ERROR: { status: 500, category: "infra", description: "Secrets resolution failed" },
  CONFIG_ERROR: { status: 500, category: "infra", description: "Configuration loading failed" },
  PROVIDER_ERROR: { status: 502, category: "infra", description: "Upstream model provider error" },
  PROVIDER_UNAVAILABLE: {
    status: 503,
    category: "unavailable",
    description: "Upstream model provider is unreachable",
  },
  PIPELINE_HALTED: { status: 500, category: "internal", description: "Task pipeline was halted" },
  HANDLER_ERROR: {
    status: 500,
    category: "internal",
    description: "Event handler execution failed",
  },
  INTERNAL_ERROR: { status: 500, category: "internal", description: "Unexpected internal error" },
  METHOD_NOT_ALLOWED: {
    status: 405,
    category: "validation",
    description: "HTTP method not allowed for this endpoint",
  },
};

export interface ErrorResponse {
  error: {
    code: string;
    message: string;
    category: ErrorCategory;
    status: number;
    details?: Record<string, unknown>;
    requestId?: string;
    timestamp?: string;
  };
}

export function toErrorResponse(error: Error | KirkForgeError, requestId?: string): ErrorResponse {
  const code = error instanceof KirkForgeError ? error.code : "INTERNAL_ERROR";
  const entry = ERROR_CATALOG[code] ?? ERROR_CATALOG["INTERNAL_ERROR"]!;
  const details: Record<string, unknown> = {};

  if (error instanceof KirkForgeError && error.context) {
    for (const [k, v] of Object.entries(error.context)) {
      if (v !== undefined) details[k] = v;
    }
  }

  return {
    error: {
      code,
      message: error.message,
      category: entry.category,
      status: entry.status,
      ...(Object.keys(details).length > 0 ? { details } : {}),
      ...(requestId ? { requestId } : {}),
      timestamp: new Date().toISOString(),
    },
  };
}

export function errorStatusCode(code: string): number {
  return ERROR_CATALOG[code]?.status ?? 500;
}

export function errorCategory(code: string): ErrorCategory {
  return ERROR_CATALOG[code]?.category ?? "internal";
}
