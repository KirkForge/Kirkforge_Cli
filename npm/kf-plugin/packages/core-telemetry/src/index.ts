import { trace, Span, SpanStatusCode, Tracer } from "@opentelemetry/api";
import { NodeSDK } from "@opentelemetry/sdk-node";
import { OTLPTraceExporter } from "@opentelemetry/exporter-trace-otlp-http";
import { OTLPMetricExporter } from "@opentelemetry/exporter-metrics-otlp-http";
import { PeriodicExportingMetricReader } from "@opentelemetry/sdk-metrics";
import { resourceFromAttributes } from "@opentelemetry/resources";
import { ATTR_SERVICE_NAME, ATTR_SERVICE_VERSION } from "@opentelemetry/semantic-conventions";
import { getNodeAutoInstrumentations } from "@opentelemetry/auto-instrumentations-node";
import type { Logger } from "@kirkforge/core-logging";

// ── Types ──────────────────────────────────────────────────────────────────

export interface TelemetryConfig {
  serviceName: string;
  serviceVersion?: string;
  otlpEndpoint?: string;
  enabled?: boolean;
  logger?: Logger;
  autoInstrument?: boolean;
}

export interface SpanOptions {
  attributes?: Record<string, string | number | boolean>;
}

// ── Module state ───────────────────────────────────────────────────────────

let _tracer: Tracer | null = null;
let _enabled = false;
let _sdk: NodeSDK | null = null;

// ── Init / shutdown ────────────────────────────────────────────────────────

export function initTelemetry(config: TelemetryConfig): void {
  if (config.enabled === false) {
    config.logger?.info("[telemetry] OpenTelemetry disabled by config");
    return;
  }

  try {
    _tracer = trace.getTracer(config.serviceName, config.serviceVersion ?? "1.0.0");
    _enabled = true;

    const otlpEndpoint =
      config.otlpEndpoint ?? process.env.OTEL_EXPORTER_OTLP_ENDPOINT ?? "http://localhost:4318";

    const resource = resourceFromAttributes({
      [ATTR_SERVICE_NAME]: config.serviceName,
      [ATTR_SERVICE_VERSION]: config.serviceVersion ?? "1.0.0",
    });

    const traceExporter = new OTLPTraceExporter({
      url: `${otlpEndpoint}/v1/traces`,
    });

    const metricReader = new PeriodicExportingMetricReader({
      exporter: new OTLPMetricExporter({
        url: `${otlpEndpoint}/v1/metrics`,
      }),
      exportIntervalMillis: 15000,
    });

    const instrumentations = config.autoInstrument !== false ? [getNodeAutoInstrumentations()] : [];

    _sdk = new NodeSDK({
      resource,
      traceExporter,
      metricReader,
      instrumentations,
    });

    _sdk.start();
    config.logger?.info(
      `[telemetry] OpenTelemetry SDK started, exporting traces+metrics to ${otlpEndpoint}`,
    );
  } catch (e) {
    config.logger?.warn(
      `[telemetry] OpenTelemetry init failed: ${e instanceof Error ? e.message : String(e)} — tracing disabled`,
    );
    _enabled = false;
    _tracer = null;
    _sdk = null;
  }
}

export async function shutdownTelemetry(): Promise<void> {
  if (_sdk) {
    try {
      await _sdk.shutdown();
    } catch {
      // Best-effort shutdown
    }
    _sdk = null;
  }
  _tracer = null;
  _enabled = false;
}

export function isTracingEnabled(): boolean {
  return _enabled && _sdk !== null;
}

// ── Tracer access ──────────────────────────────────────────────────────────

export function getTracer(): Tracer {
  if (_tracer) return _tracer;
  return trace.getTracer("kirkforge", "1.0.0");
}

// ── Span helpers ───────────────────────────────────────────────────────────

/**
 * Execute fn with an auto-closing span. Returns fn's result.
 */
export async function withSpan<T>(
  name: string,
  fn: (span: Span) => Promise<T>,
  opts?: SpanOptions,
): Promise<T> {
  if (!_enabled) return fn({} as Span);

  const tracer = getTracer();
  const span = tracer.startSpan(name);
  if (opts?.attributes) {
    for (const [k, v] of Object.entries(opts.attributes)) {
      span.setAttribute(k, v);
    }
  }
  try {
    const result = await fn(span);
    span.setStatus({ code: SpanStatusCode.OK });
    return result;
  } catch (e) {
    span.setStatus({
      code: SpanStatusCode.ERROR,
      message: e instanceof Error ? e.message : String(e),
    });
    span.recordException(e instanceof Error ? e : new Error(String(e)));
    throw e;
  } finally {
    span.end();
  }
}

/**
 * Synchronous version of withSpan.
 */
export function withSpanSync<T>(name: string, fn: (span: Span) => T, opts?: SpanOptions): T {
  if (!_enabled) return fn({} as Span);

  const tracer = getTracer();
  const span = tracer.startSpan(name);
  if (opts?.attributes) {
    for (const [k, v] of Object.entries(opts.attributes)) {
      span.setAttribute(k, v);
    }
  }
  try {
    const result = fn(span);
    span.setStatus({ code: SpanStatusCode.OK });
    return result;
  } catch (e) {
    span.setStatus({
      code: SpanStatusCode.ERROR,
      message: e instanceof Error ? e.message : String(e),
    });
    span.recordException(e instanceof Error ? e : new Error(String(e)));
    throw e;
  } finally {
    span.end();
  }
}

// ── Metrics helpers ────────────────────────────────────────────────────────

import { metrics, type Counter, type Histogram, type UpDownCounter } from "@opentelemetry/api";

let _delegationCounter: Counter | null = null;
let _delegationDuration: Histogram | null = null;
let _tokenCounter: Counter | null = null;
let _errorCounter: Counter | null = null;
let _activeTasks: UpDownCounter | null = null;

function ensureMetrics(): void {
  if (!_enabled) return;
  const meter = metrics.getMeter("kirkforge", "1.0.0");

  if (!_delegationCounter) {
    _delegationCounter = meter.createCounter("kirkforge.delegations.total", {
      description: "Total number of delegated tasks",
    });
  }
  if (!_delegationDuration) {
    _delegationDuration = meter.createHistogram("kirkforge.delegation.duration_ms", {
      description: "Delegation duration in milliseconds",
      unit: "ms",
    });
  }
  if (!_tokenCounter) {
    _tokenCounter = meter.createCounter("kirkforge.tokens.total", {
      description: "Total tokens consumed across all providers",
    });
  }
  if (!_errorCounter) {
    _errorCounter = meter.createCounter("kirkforge.errors.total", {
      description: "Total errors by category",
    });
  }
  if (!_activeTasks) {
    _activeTasks = meter.createUpDownCounter("kirkforge.tasks.active", {
      description: "Number of currently active tasks",
    });
  }
}

/** Record a delegation event for metrics. */
export function recordDelegation(attrs: {
  mode: string;
  provider: string;
  model: string;
  outcome: "pass" | "fail" | "error";
  durationMs: number;
  tokens: number;
}): void {
  if (!_enabled) return;
  ensureMetrics();
  _delegationCounter?.add(1, {
    mode: attrs.mode,
    provider: attrs.provider,
    model: attrs.model,
    outcome: attrs.outcome,
  });
  _delegationDuration?.record(attrs.durationMs, {
    mode: attrs.mode,
    outcome: attrs.outcome,
  });
  _tokenCounter?.add(attrs.tokens, {
    provider: attrs.provider,
    model: attrs.model,
  });
}

/** Record an error event for metrics. */
export function recordError(category: string, code: string): void {
  if (!_enabled) return;
  ensureMetrics();
  _errorCounter?.add(1, { category, code });
}

/** Track task lifecycle: increment on start, decrement on end. */
export function taskStarted(): void {
  if (!_enabled) return;
  ensureMetrics();
  _activeTasks?.add(1);
}

/** Record circuit breaker state transition for metrics. */
export function recordCircuitBreakerState(key: string, transition: string): void {
  if (!_enabled) return;
  try {
    const meter = metrics.getMeter("kirkforge", "1.0.0");
    const cbCounter = meter.createCounter("kirkforge.circuit_breaker.transitions", {
      description: "Circuit breaker state transitions",
    });
    cbCounter.add(1, { provider: key, transition });
  } catch {}
}

export function taskEnded(): void {
  if (!_enabled) return;
  ensureMetrics();
  _activeTasks?.add(-1);
}
