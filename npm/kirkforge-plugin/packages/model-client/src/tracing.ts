import { withSpan, isTracingEnabled } from "@kirkforge/core-telemetry";
import type { Span } from "@opentelemetry/api";

/**
 * Wrap a model API call with tracing spans.
 */
export async function traceModelCall<T>(
  provider: string,
  model: string,
  fn: (span: Span) => Promise<T>,
): Promise<T> {
  if (!isTracingEnabled()) return fn({} as Span);

  return withSpan(`model.${provider}.chat`, async (span) => {
    span.setAttribute("gen_ai.system", provider);
    span.setAttribute("gen_ai.request.model", model);
    return fn(span);
  });
}

/**
 * Set response attributes on a model call span.
 */
export function setModelResponseAttributes(
  span: Span,
  attrs: {
    promptTokens?: number;
    completionTokens?: number;
    totalTokens?: number;
    reasoningTokens?: number;
    finishReason?: string;
  },
): void {
  if (!isTracingEnabled()) return;
  if (attrs.promptTokens != null)
    span.setAttribute("gen_ai.usage.input_tokens", attrs.promptTokens);
  if (attrs.completionTokens != null)
    span.setAttribute("gen_ai.usage.output_tokens", attrs.completionTokens);
  if (attrs.totalTokens != null) span.setAttribute("gen_ai.usage.total_tokens", attrs.totalTokens);
  if (attrs.reasoningTokens != null)
    span.setAttribute("gen_ai.usage.reasoning_tokens", attrs.reasoningTokens);
  if (attrs.finishReason) span.setAttribute("gen_ai.response.finish_reason", attrs.finishReason);
}
