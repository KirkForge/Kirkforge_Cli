import { z } from "zod";
import type { ModelResponse, ModelClientOptions } from "./types.js";
import { ModelClientError } from "./model-client-error.js";

export const ChatCompletionSchema = z
  .object({
    model: z.string().optional(),
    choices: z
      .array(
        z
          .object({
            message: z.object({ content: z.string().optional() }).passthrough().optional(),
            finish_reason: z.string().optional(),
          })
          .passthrough(),
      )
      .optional(),
    usage: z
      .object({
        prompt_tokens: z.number().optional(),
        completion_tokens: z.number().optional(),
        total_tokens: z.number().optional(),
      })
      .passthrough()
      .optional(),
  })
  .passthrough();

export async function handleHttpError(res: Response): Promise<never> {
  const text = await res.text().catch(() => "Unknown error");
  if (res.status === 401 || res.status === 403) throw ModelClientError.auth(text);
  if (res.status === 429) {
    const retryAfter = parseRetryAfter(res.headers.get("Retry-After"));
    throw ModelClientError.rateLimit(text, retryAfter);
  }
  throw ModelClientError.api(res.status, text);
}

function parseRetryAfter(header: string | null): number | undefined {
  if (!header) return undefined;
  const seconds = parseInt(header, 10);
  if (!isNaN(seconds)) return seconds * 1000;
  const date = Date.parse(header);
  if (!isNaN(date)) return Math.max(0, date - Date.now());
  return undefined;
}

export function parseChatCompletionResponse(
  raw: unknown,
  schema: z.ZodTypeAny,
  options: ModelClientOptions,
  extraContentKeys: string[] = [],
): ModelResponse {
  const parsed = schema.safeParse(raw);
  const data: Record<string, unknown> = parsed.success
    ? (parsed.data as Record<string, unknown>)
    : (raw as Record<string, unknown>);
  const choices = data.choices as Array<Record<string, unknown>> | undefined;
  const usageData = data.usage as Record<string, unknown> | undefined;
  const choice = choices?.[0];
  const msg = (choice?.message ?? {}) as Record<string, unknown>;
  let content = (msg.content as string) || "";
  if (!content) {
    for (const key of extraContentKeys) {
      if (msg[key] as string) {
        content = msg[key] as string;
        break;
      }
    }
  }
  const promptTokens = (usageData?.prompt_tokens as number) ?? 0;
  const completionTokens = (usageData?.completion_tokens as number) ?? 0;
  const reportedTotal = usageData?.total_tokens as number | undefined;
  const totalTokens =
    typeof reportedTotal === "number" ? reportedTotal : promptTokens + completionTokens;
  return {
    content,
    model: (data.model as string) ?? options.defaultModel,
    promptTokens,
    completionTokens,
    totalTokens,
    finishReason: choice?.finish_reason as string | undefined,
  };
}
