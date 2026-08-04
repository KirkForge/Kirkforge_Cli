import { z } from "zod";
import type { Result } from "@kirkforge/core-types";
import { ok, err } from "@kirkforge/core-types";
import { ConfigError } from "@kirkforge/core-errors";
import type { ModelProviderConfig, ModelConfig } from "@kirkforge/model-config";
import { ModelClient } from "@kirkforge/model-client";
import type { PromptTemplate, TaskBrief } from "@kirkforge/prompt-core";
import { compilePrompt, BUILTIN_TEMPLATES } from "@kirkforge/prompt-core";

export interface AgentEmission {
  agentId: string;
  content: string;
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  model: string;
  format: "hard-prompt" | "schema-contract" | "artifact" | "task-decompose";
  schemaContract?: Record<string, unknown>;
  finishReason?: string;
  reasoningTokens?: number;
  retried?: boolean;
}

export class Agent {
  private client: ModelClient;
  private template: PromptTemplate;

  constructor(
    public readonly agentId: string,
    providerConfig: ModelProviderConfig,
    template?: PromptTemplate,
  ) {
    this.client = new ModelClient({
      ...providerConfig,
      providerType: (providerConfig as Record<string, unknown>).provider as string,
    });
    this.template = template ?? BUILTIN_TEMPLATES["coder"]!;
  }

  static fromConfig(
    agentId: string,
    modelConfig: ModelConfig,
    providerKey?: string,
    template?: PromptTemplate,
  ): Result<Agent, ConfigError> {
    const key = providerKey ?? modelConfig.defaultProvider;
    const cfg = modelConfig.providers[key];
    if (!cfg) return err(new ConfigError(`Provider "${key}" not found`));
    return ok(new Agent(agentId, cfg, template));
  }

  async execute(brief: TaskBrief): Promise<Result<AgentEmission, Error>> {
    const compiled = compilePrompt(this.template, brief);
    if (!compiled.ok) return err(new Error(`Prompt compilation failed: ${compiled.error.message}`));

    const response = await this.client.complete(
      compiled.value.systemPrompt,
      compiled.value.userPrompt,
    );

    const emission: AgentEmission = {
      agentId: this.agentId,
      content: response.content,
      promptTokens: response.promptTokens,
      completionTokens: response.completionTokens,
      totalTokens: response.totalTokens,
      model: response.model,
      format: compiled.value.format,
      finishReason: response.finishReason,
      reasoningTokens: response.reasoningTokens,
    };

    const schema = this.template.responseSchema;
    const parsed = extractSchemaContract(response.content, schema);
    if (parsed) {
      emission.schemaContract = parsed;
    } else if (compiled.value.format === "schema-contract") {
      const retryPrompt =
        compiled.value.userPrompt +
        "\n\n---\nYour previous output could not be parsed as valid JSON. Output ONLY the JSON object, no markdown, no explanation.";
      const retryResponse = await this.client.complete(compiled.value.systemPrompt, retryPrompt);
      emission.content = retryResponse.content;
      emission.promptTokens += retryResponse.promptTokens;
      emission.completionTokens += retryResponse.completionTokens;
      emission.totalTokens += retryResponse.totalTokens;
      emission.retried = true;
      const retryParsed = extractSchemaContract(retryResponse.content, schema);
      if (retryParsed) emission.schemaContract = retryParsed;
    }

    return ok(emission);
  }
}

function extractSchemaContract(
  text: string,
  schema?: z.ZodType<any> | null,
): Record<string, unknown> | null {
  const s = schema ?? z.record(z.string(), z.unknown());
  const codeBlock = text.match(/```(?:\w+)?\s*\n?([\s\S]*?)```/);
  if (codeBlock) {
    const result = tryParse(codeBlock[1]!.trim(), s);
    if (result) return result;
  }
  const brace = findBalancedBraceBlock(text);
  if (brace) {
    const result = tryParse(brace, s);
    if (result) return result;
  }
  return null;
}

function findBalancedBraceBlock(text: string): string | null {
  let firstOpen = -1;
  let depth = 0;
  let inString: string | null = null;
  let escape = false;

  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (escape) {
      escape = false;
      continue;
    }
    if (ch === "\\") {
      escape = true;
      continue;
    }
    if (inString) {
      if (ch === inString) inString = null;
      continue;
    }
    if (ch === '"' || ch === "'") {
      inString = ch;
      continue;
    }
    if (ch === "{") {
      if (depth === 0) firstOpen = i;
      depth++;
    }
    if (ch === "}") {
      depth--;
      if (depth === 0 && firstOpen >= 0) return text.slice(firstOpen, i + 1);
    }
  }
  return null;
}

function tryParse(candidate: string, schema: z.ZodType<any>): Record<string, unknown> | null {
  try {
    const parsed = JSON.parse(candidate);
    const result = schema.safeParse(parsed);
    return result.success ? (result.data as Record<string, unknown>) : null;
  } catch {
    const inner = findBalancedBraceBlock(candidate);
    if (inner && inner !== candidate) {
      try {
        const parsed = JSON.parse(inner);
        const result = schema.safeParse(parsed);
        return result.success ? (result.data as Record<string, unknown>) : null;
      } catch {
        return null;
      }
    }
    return null;
  }
}
