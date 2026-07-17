import type { Result } from "@kirkforge/core-types";
import { ok, err } from "@kirkforge/core-types";
import { EventBus } from "@kirkforge/core-events";
import {
  StateReducer,
  createVerificationEmitters,
  detectTaskProfile,
  profileForLanguage,
} from "@kirkforge/orchestrator";
import type { ReducedStatePacket, TaskLanguage, VerifierPolicy } from "@kirkforge/correction-core";
import { buildCorrectionPrompt as correctionBuildCorrectionPrompt } from "@kirkforge/correction-core";
import type {
  MemoryStore,
  RoutingBias,
  TaskObservationInput as PalaceTaskObservationInput,
} from "@kirkforge/memory-palace";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { safeRelativePath } from "@kirkforge/orchestrator/path-safety.js";

const execFileAsync = promisify(execFile);

const MAX_PARENT_TRAVERSAL = 6;

/**
 * Search for a bundled tool in `node_modules/.bin` directories starting from
 * the plugin module location and, when invoked via the CLI wrapper, from the
 * CLI entry point. This lets `doctor()` report workspace-bundled tools (tsc,
 * pyright, eslint) even when the plugin host passes a curated PATH that does
 * not include the workspace's `node_modules/.bin`.
 */
function resolveBundledBinary(name: string): string | undefined {
  const moduleFile = fileURLToPath(import.meta.url);
  const cliFile = process.argv[1];
  for (const startFile of [moduleFile, cliFile]) {
    if (!startFile) continue;
    let dir = dirname(startFile);
    for (let i = 0; i < MAX_PARENT_TRAVERSAL && dir !== dirname(dir); i++) {
      const candidate = join(dir, "node_modules", ".bin", name);
      if (existsSync(candidate)) return candidate;
      dir = dirname(dir);
    }
  }
  return undefined;
}

export type { ReducedStatePacket } from "@kirkforge/correction-core";
export type { TaskLanguage } from "@kirkforge/correction-core";
export type { RoutingBias } from "@kirkforge/memory-palace";
export { toolNames } from "@kirkforge/correction-core";

export interface VerifyWorkspaceInput {
  workspace: string;
  files?: string[];
  language?: string;
  description?: string;
  taskId?: string;
}

export interface CorrectionContext {
  taskDescription?: string;
  language?: string;
  maxTokens?: number;
}

export interface TaskObservation {
  taskId: string;
  description: string;
  language: string;
  mode: string;
  model: string;
  outcome: "pass" | "fail" | "escalate" | "error";
  durationMs: number;
  tokens?: number;
  taskFamily?: string;
  verifierOverall?: string;
}

export interface ToolCapabilityReport {
  eslint: ToolCapability;
  tsc: ToolCapability;
  ruff: ToolCapability;
  pyright: ToolCapability;
  bandit: ToolCapability;
  secdev: ToolCapability;
  languages: string[];
}

export interface ToolCapability {
  available: boolean;
  version?: string;
  source: "external" | "internal";
  required?: boolean;
  note?: string;
}

export interface PluginCoreConfig {
  memoryStore?: MemoryStore;
  cwd?: string;
}

function normalizeLanguage(language?: string): TaskLanguage {
  if (!language) return "typescript";
  const normalized = language.toLowerCase();
  const valid = [
    "typescript",
    "javascript",
    "python",
    "shell",
    "cpp",
    "c",
    "rust",
    "go",
    "sql",
    "text",
  ];
  if (valid.includes(normalized)) {
    return normalized as TaskLanguage;
  }
  // Unknown language — throw so callers can't silently reroute
  throw new Error(`Unknown language: "${language}". Valid: ${valid.join(", ")}`);
}

export function buildCorrectionPrompt(
  packet: ReducedStatePacket,
  context?: CorrectionContext,
): string {
  let language: TaskLanguage | undefined;
  try {
    language = normalizeLanguage(context?.language);
  } catch {
    // Unknown language — pass undefined so correction-core falls back to generic tool names
  }
  return correctionBuildCorrectionPrompt(packet, language);
}

function defaultTaskId(): string {
  return `task-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

export async function verifyWorkspace(
  input: VerifyWorkspaceInput,
): Promise<Result<ReducedStatePacket, Error>> {
  const cwd = input.workspace;
  if (!existsSync(cwd)) {
    return err(new Error(`Workspace directory does not exist: ${cwd}`));
  }
  const taskId = input.taskId ?? defaultTaskId();

  let profile;
  try {
    profile = input.language
      ? profileForLanguage(normalizeLanguage(input.language))
      : detectTaskProfile(input.description ?? "verify workspace");
  } catch (e) {
    return err(
      new Error(
        `Unsupported language: ${input.language}. ${e instanceof Error ? e.message : String(e)}`,
      ),
    );
  }
  const effectiveLanguage = profile.language;
  const policy: VerifierPolicy = profile.verifierPolicy;

  if (input.files) {
    const originalCount = input.files.length;
    const sanitized = input.files
      .map((f) => safeRelativePath(cwd, f))
      .filter((f): f is string => f !== null);
    if (sanitized.length < originalCount) {
      return err(
        new Error(
          `verifyWorkspace: ${originalCount - sanitized.length} file(s) rejected by path safety check (outside workspace or unsafe)`,
        ),
      );
    }
    input = { ...input, files: sanitized };
  }

  let eventBus: EventBus | undefined;
  try {
    eventBus = new EventBus({ bufferCapacity: 1000 });
    const reducer = new StateReducer(eventBus);
    const emitters = createVerificationEmitters(cwd, eventBus, input.files, effectiveLanguage);

    await Promise.allSettled([
      emitters.lint.emit(taskId),
      emitters.types.emit(taskId),
      emitters.security.emit(taskId),
      emitters.changes.emit(taskId),
      emitters.graph.emit(taskId),
      emitters.imports.emit(taskId),
    ]);

    const packet = reducer.reduce(taskId, 0, policy);
    return ok(packet);
  } catch (cause) {
    return err(new Error("plugin: verification failed", { cause }));
  } finally {
    await eventBus?.gracefulShutdown();
  }
}

export async function recordObservation(
  observation: TaskObservation,
  memoryStore?: MemoryStore,
): Promise<Result<void, Error>> {
  if (!memoryStore) {
    return err(
      new Error(
        "recordObservation requires a MemoryStore instance. Pass one via PluginCoreConfig.memoryStore.",
      ),
    );
  }

  const palaceOutcome: "pass" | "fail" | "error" =
    observation.outcome === "pass" ? "pass" : observation.outcome === "fail" ? "fail" : "error";

  const palaceInput: PalaceTaskObservationInput = {
    taskId: observation.taskId,
    description: observation.description,
    language: observation.language,
    taskFamily: observation.taskFamily,
    mode: observation.mode,
    model: observation.model,
    outcome: palaceOutcome,
    tokens: observation.tokens ?? 0,
    durationMs: observation.durationMs,
    verifierOverall: observation.verifierOverall,
  };

  return memoryStore.writeTaskObservation(palaceInput);
}

export async function recallRoutingBias(
  taskDescription: string,
  workerModel?: string,
  memoryStore?: MemoryStore,
): Promise<Result<RoutingBias | null, Error>> {
  if (!memoryStore) {
    return err(
      new Error(
        "recallRoutingBias requires a MemoryStore instance. Pass one via PluginCoreConfig.memoryStore.",
      ),
    );
  }

  const result = await memoryStore.recall(taskDescription, workerModel);
  if (!result.ok) return result;
  const recommendation = result.value;
  if (!recommendation) return ok(null);
  return ok(recommendation.routingBias ?? null);
}

const INTERNAL_TOOLS = new Set(["secdev"]);

async function probeTool(name: string, args: string[] = ["--version"]): Promise<ToolCapability> {
  const source: "external" | "internal" = INTERNAL_TOOLS.has(name) ? "internal" : "external";
  const bundledPath = resolveBundledBinary(name);
  try {
    const { stdout } = await execFileAsync(bundledPath ?? name, args, { timeout: 5000 });
    const firstLine = stdout.trim().split("\n")[0];
    const version = firstLine ? firstLine.trim() : undefined;
    return { available: true, version, source };
  } catch {
    return { available: false, source };
  }
}

export async function doctor(): Promise<ToolCapabilityReport> {
  const [eslint, tsc, ruff, pyright, bandit] = await Promise.all([
    probeTool("eslint", ["--version"]),
    probeTool("tsc", ["--version"]),
    probeTool("ruff", ["--version"]),
    probeTool("pyright", ["--version"]),
    probeTool("bandit", ["--version"]),
  ]);

  const secdev: ToolCapability = {
    available: true,
    source: "internal",
    note: "Regex-based security scanner (not a substitute for shellcheck/pylint/bandit on non-JS/TS). Advisory for C/C++/Go/Rust/SQL.",
  };

  const hasTsTool = eslint.available || tsc.available;
  const hasPyTool = ruff.available || pyright.available;
  const languages: string[] = [];
  if (hasTsTool) languages.push("typescript", "javascript");
  if (hasPyTool) languages.push("python");
  languages.push(
    "shell (advisory only)",
    "cpp (validator required)",
    "c (validator required)",
    "rust (validator required)",
    "go (validator required)",
    "sql (validator required)",
  );
  if (languages.length === 0) languages.push("unknown");

  return { eslint, tsc, ruff, pyright, bandit, secdev, languages };
}

export function createPluginCore(config?: PluginCoreConfig) {
  const memoryStore = config?.memoryStore;
  return {
    verifyWorkspace,
    buildCorrectionPrompt,
    recordObservation: (observation: TaskObservation) =>
      recordObservation(observation, memoryStore),
    recallRoutingBias: (taskDescription: string, workerModel?: string) =>
      recallRoutingBias(taskDescription, workerModel, memoryStore),
    doctor,
  };
}

// Auth-audit bridge: wires RBAC decisions to the audit logger
export { createAuthAuditHook } from "./auth-audit-bridge.js";

// Tenant context: multi-tenant scoping for plugin operations
export {
  createTenantContext,
  createTenantAuditLogger,
  type TenantContext,
  type CreateTenantContextConfig,
} from "./tenant-context.js";

// Auth middleware: OIDC/API key authentication and RBAC for HTTP/MCP handlers
export {
  AuthMiddleware,
  AuthMiddlewareError,
  createAuthMiddleware,
  parseGroupRoleMapping,
  type AuthMiddlewareConfig,
  type AuthenticatedRequest,
} from "./auth-middleware.js";
