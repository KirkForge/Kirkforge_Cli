import type { ChatMessage, ModelResponse, ModelClientOptions } from "./types.js";
import { ModelClientError } from "./model-client-error.js";
import { ChatCompletionSchema, handleHttpError, parseChatCompletionResponse } from "./shared.js";

export async function chatCompletion(
  messages: ChatMessage[],
  options: ModelClientOptions,
  headers: Record<string, string> = {},
): Promise<ModelResponse> {
  const url = `${options.baseUrl}/chat/completions`;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs);
  const body: Record<string, unknown> = {
    model: options.defaultModel,
    messages,
    stream: false,
    max_tokens: options.maxTokens ?? 4096,
  };
  if (options.temperature !== undefined) body.temperature = options.temperature;
  if (options.topP !== undefined) body.top_p = options.topP;
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...headers },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    if (!res.ok) await handleHttpError(res);
    return parseChatCompletionResponse(await res.json(), ChatCompletionSchema, options, [
      "reasoning",
    ]);
  } catch (e) {
    if (e instanceof ModelClientError) throw e;
    if (e instanceof DOMException && e.name === "AbortError")
      throw ModelClientError.timeout(options.timeoutMs);
    throw ModelClientError.network(e instanceof Error ? e.message : String(e));
  } finally {
    clearTimeout(timer);
  }
}

import { z } from "zod";

const AnthropicResponseSchema = z
  .object({
    model: z.string().optional(),
    content: z
      .array(z.object({ type: z.string(), text: z.string().optional() }).passthrough())
      .optional(),
    usage: z
      .object({ input_tokens: z.number().optional(), output_tokens: z.number().optional() })
      .passthrough()
      .optional(),
    stop_reason: z.string().optional(),
  })
  .passthrough();

export async function anthropicCompletion(
  messages: ChatMessage[],
  options: ModelClientOptions,
): Promise<ModelResponse> {
  const url = options.baseUrl + "/messages";
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs);
  const systemMessages = messages.filter((m) => m.role === "system");
  const userMessages = messages.filter((m) => m.role !== "system");
  const maxTokens = options.maxTokens ?? 4096;
  const body: Record<string, unknown> = {
    model: options.defaultModel,
    max_tokens: maxTokens,
    messages: userMessages.map((m) => ({ role: m.role, content: m.content })),
  };
  if (systemMessages.length > 0) body.system = systemMessages.map((m) => m.content).join("\n\n");
  if (options.temperature !== undefined) body.temperature = options.temperature;
  if (options.topP !== undefined) body.top_p = options.topP;
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "x-api-key": options.apiKey ?? "",
        "anthropic-version": "2023-06-01",
      },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    if (!res.ok) await handleHttpError(res);
    const raw = await res.json();
    const parsed = AnthropicResponseSchema.safeParse(raw);
    const data: Record<string, unknown> = parsed.success
      ? (parsed.data as Record<string, unknown>)
      : (raw as Record<string, unknown>);
    const contentArr = data.content as Array<Record<string, unknown>> | undefined;
    const usageData = data.usage as Record<string, unknown> | undefined;
    const promptTokens = (usageData?.input_tokens as number) ?? 0;
    const completionTokens = (usageData?.output_tokens as number) ?? 0;
    return {
      content: (contentArr?.[0]?.type === "text" ? contentArr[0].text : "") as string,
      model: (data.model as string) ?? options.defaultModel,
      promptTokens,
      completionTokens,
      totalTokens: promptTokens + completionTokens,
      finishReason: data.stop_reason as string | undefined,
    };
  } catch (e) {
    if (e instanceof ModelClientError) throw e;
    if (e instanceof DOMException && e.name === "AbortError")
      throw ModelClientError.timeout(options.timeoutMs);
    throw ModelClientError.network(e instanceof Error ? e.message : String(e));
  } finally {
    clearTimeout(timer);
  }
}
