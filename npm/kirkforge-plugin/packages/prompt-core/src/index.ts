import { z } from "zod";
import type { Result } from "@kirkforge/core-types";
import { ok } from "@kirkforge/core-types";

export type PromptFormat = "hard-prompt" | "schema-contract" | "artifact" | "task-decompose";

export interface PromptTemplate {
  id: string;
  name: string;
  format: PromptFormat;
  systemPrompt: string;
  userPromptTemplate: string;
  responseSchema?: z.ZodType<any> | null;
}

export interface TaskBrief {
  description: string;
  variables?: Record<string, string>;
}

export interface CompiledPrompt {
  format: PromptFormat;
  systemPrompt: string;
  userPrompt: string;
}

export function compilePrompt(
  template: PromptTemplate,
  brief: TaskBrief,
): Result<CompiledPrompt, Error> {
  let userPrompt = template.userPromptTemplate;
  const vars = brief.variables ?? {};
  vars.task = brief.description;
  vars.emissionRules = vars.emissionRules ?? "";
  vars.forbiddenRules = vars.forbiddenRules ?? "";

  for (const [key, value] of Object.entries(vars)) {
    userPrompt = userPrompt.replaceAll(`{{${key}}}`, value);
  }

  let systemPrompt = template.systemPrompt;
  for (const [key, value] of Object.entries(vars)) {
    systemPrompt = systemPrompt.replaceAll(`{{${key}}}`, value);
  }

  return ok({ format: template.format, systemPrompt, userPrompt });
}

export const HARD_PROMPT_TEMPLATE: PromptTemplate = {
  id: "hard-prompt",
  name: "Free-Text Delegation",
  format: "hard-prompt",
  systemPrompt: "Complete the requested coding task. Keep output concise. {{languageHint}}",
  userPromptTemplate: "{{task}}\n\nTarget language: {{language}}. Default file: {{defaultFile}}.",
};

export function buildContractTemplate(language: string, hint: string): PromptTemplate {
  return {
    id: `contract-${language}`,
    name: `Contract (${language})`,
    format: "schema-contract",
    systemPrompt:
      `You are a ${language} code verifier. Output only valid JSON with ${language}-specific findings. ` +
      `No explanation, no surrounding prose. ` +
      `Focus on ${language} best practices, idioms, and conventions. ${hint}`,
    userPromptTemplate:
      `{{task}}\n\n` +
      `Analyze the ${language} codebase. Output a JSON object with your ${language}-specific findings. ` +
      `Keys must match the expected schema. Report ${language}-specific issues.`,
    responseSchema: z.record(z.string(), z.unknown()),
  };
}

export const DEFAULT_CONTRACT_TEMPLATE: PromptTemplate = buildContractTemplate(
  "typescript",
  "Emit TypeScript.",
);

export const ARTIFACT_TEMPLATE: PromptTemplate = {
  id: "artifact",
  name: "File Emission",
  format: "artifact",
  systemPrompt:
    "Generate files. Output ONLY JSONL lines. No prose, no explanation, no preamble, no markdown fences. " +
    "Use JSONL protocol — one JSON object per line:\n" +
    '{"type":"file_write","path":"relative/path.ext","sha256":"<sha256hex>","content_b64":"<base64>"}\n\n' +
    "Compute sha256 of the exact file content (utf-8), then base64-encode the content. " +
    "Every line must be valid JSON with type, path, sha256, and content_b64 fields. " +
    "No other text allowed. {{languageHint}}" +
    "{{emissionRules}}",
  userPromptTemplate:
    "{{task}}\n\n" +
    "Target language: {{language}}. Default file: {{defaultFile}}.\n" +
    "Emit only allowed file types for {{language}}.\n" +
    "{{forbiddenRules}}" +
    "Use only relative paths inside the sandbox. No absolute paths. No path traversal (../).\n" +
    "Do not wrap file content in ``` fences.",
  responseSchema: null,
};

export function getContractTemplate(language: string, hint: string): PromptTemplate {
  return buildContractTemplate(language, hint);
}

export const DECOMPOSE_TEMPLATE: PromptTemplate = {
  id: "task-decompose",
  name: "Task Decomposition",
  format: "task-decompose",
  systemPrompt:
    "You are a task decomposition engine. Break complex coding tasks into the smallest independently verifiable subtasks. " +
    "Output ONLY a valid JSON array of task objects. No prose, no explanation, no markdown fences. " +
    "Each task object must have: id (kebab-case string), description (one sentence), language (one of: typescript, javascript, python, shell, cpp, c, rust, go, sql, text), " +
    "dependsOn (array of prerequisite task ids, empty if none), estimatedComplexity (one of: trivial, simple, moderate, complex), " +
    'outputFiles (array of expected output file paths like ["src/server.ts"]), ' +
    "verificationHint (short sentence on how to verify this subtask works). " +
    "Order tasks by dependency. A task cannot depend on a task that comes after it. " +
    "Each subtask must be small enough that a single LLM call can complete it. " +
    "Prefer 3-7 subtasks for most requests. Do not exceed 12.",
  userPromptTemplate:
    "Break this task into the smallest independently verifiable subtasks: {{task}}\n\n" +
    "Target language: {{language}}.\n\n" +
    "Return ONLY a JSON array. Each element must match the task schema exactly.",
  responseSchema: z.array(
    z.object({
      id: z.string().min(1),
      description: z.string(),
      language: z.string(),
      dependsOn: z.array(z.string()),
      estimatedComplexity: z.enum(["trivial", "simple", "moderate", "complex"]),
      outputFiles: z.array(z.string()),
      verificationHint: z.string(),
    }),
  ),
};

export const BUILTIN_TEMPLATES: Record<string, PromptTemplate> = {
  "task-decompose": DECOMPOSE_TEMPLATE,
  "hard-prompt": HARD_PROMPT_TEMPLATE,
  "schema-contract": DEFAULT_CONTRACT_TEMPLATE,
  artifact: ARTIFACT_TEMPLATE,
  coder: HARD_PROMPT_TEMPLATE,
};
