import { ok, err } from "@kirkforge/core-types";
import { Agent } from "@kirkforge/agent-core";
import { BUILTIN_TEMPLATES } from "@kirkforge/prompt-core";
import { detectTaskProfile } from "./task-profile.js";
import {
  extractWrittenFiles,
  type TaskInput,
  type DecompositionResult,
  type SubtaskExecutionResult,
  type DecompositionExecutionResult,
} from "./types.js";
import type { TaskNode } from "@kirkforge/core-types";
import { resolveProvider } from "./orchestrator-provider.js";
import { auditPolicyDeny } from "./orchestrator-telemetry.js";
import type { OrchestratorInternals } from "./orchestrator-shared.js";
import type { Actor } from "@kirkforge/core-rbac";
import { KirkForgeError } from "@kirkforge/core-errors";

/**
 * Break a task into subtasks via a single-shot model call. Applies
 * policy enforcement (model + tool) before dispatch and persists
 * successful decompositions to memory for future recall.
 */
export async function decomposeTask(
  s: OrchestratorInternals,
  task: TaskInput,
): Promise<import("@kirkforge/core-types").Result<DecompositionResult, Error>> {
  if (s.shuttingDown) return err(new Error("Orchestrator is shutting down"));

  if (s.policyEngine) {
    const providerConfig = resolveProvider(s, null);
    const modelDecision = s.policyEngine.checkModel(providerConfig.defaultModel);
    if (!modelDecision.allowed) {
      auditPolicyDeny(s, "model.deny", modelDecision.reason, modelDecision.policyHash, task.actor);
      return err(
        new KirkForgeError("POLICY_DENIED", modelDecision.reason, {
          rule: modelDecision.rule,
          policyHash: modelDecision.policyHash,
        }),
      );
    }
    const profile = detectTaskProfile(task.description);
    const toolDecision = s.policyEngine.checkTool(profile.language ?? "unknown");
    if (!toolDecision.allowed) {
      auditPolicyDeny(s, "tool.deny", toolDecision.reason, toolDecision.policyHash, task.actor);
      return err(
        new KirkForgeError("POLICY_DENIED", toolDecision.reason, {
          rule: toolDecision.rule,
          policyHash: toolDecision.policyHash,
        }),
      );
    }
  }

  const effectiveTaskId =
    task.taskId ?? `decomp-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  const profile = detectTaskProfile(task.description);
  const agent = Agent.fromConfig(
    "decomposer-" + effectiveTaskId,
    s.modelConfig,
    s.decomposeProvider,
    BUILTIN_TEMPLATES["task-decompose"],
  );
  if (!agent.ok) return err(agent.error);

  const brief = {
    description: task.description,
    variables: {
      language: profile.language,
      defaultFile: profile.defaultFile,
    },
  };

  const result = await agent.value.execute(brief);
  if (!result.ok) return err(result.error);

  const emission = result.value;
  const parsed = parseDecomposition(s, emission.content);
  if (!parsed.ok) {
    const retryResult = await agent.value.execute({
      description:
        task.description +
        "\n\n---\nYour previous output could not be parsed as valid JSON. Output ONLY a JSON array, no markdown, no explanation.",
      variables: {
        language: profile.language,
        defaultFile: profile.defaultFile,
      },
    });
    if (!retryResult.ok) return err(retryResult.error);
    const retryParsed = parseDecomposition(s, retryResult.value.content);
    if (!retryParsed.ok)
      return err(new Error("Decomposition failed after retry: " + retryParsed.error.message));
    const rdr = retryParsed.value;
    rdr.rootTask = task.description;
    if (s.memoryStore) {
      s.memoryStore
        .writeDecomposition(effectiveTaskId, task.description, rdr.tasks, profile.language)
        .catch(() => {});
    }
    return ok(rdr);
  }

  const dr = parsed.value;
  dr.rootTask = task.description;
  if (s.memoryStore) {
    s.memoryStore
      .writeDecomposition(effectiveTaskId, task.description, dr.tasks, profile.language)
      .catch(() => {
        // Persistence failure is non-fatal
      });
  }
  return ok(dr);
}

/**
 * Parse a model-emitted decomposition string into a typed
 * DecompositionResult. Strips markdown fences, applies a bracket-heuristic
 * to find the JSON array, validates each node, and topologically sorts.
 */
export function parseDecomposition(
  s: OrchestratorInternals,
  raw: string,
): import("@kirkforge/core-types").Result<DecompositionResult, Error> {
  let jsonStr = raw.trim();
  const codeBlock = jsonStr.match(/```(?:json)?\s*\n?([\s\S]*?)```/);
  if (codeBlock) jsonStr = codeBlock[1]!.trim();
  // Robust bracket heuristic: [{ is unambiguous JSON array start
  const bracketPair = jsonStr.indexOf("[{");
  if (bracketPair > 0) jsonStr = jsonStr.slice(bracketPair);
  else {
    const braceStart = jsonStr.indexOf("[");
    if (braceStart > 0) jsonStr = jsonStr.slice(braceStart);
  }
  const braceEnd = jsonStr.lastIndexOf("}]");
  if (braceEnd > 0) jsonStr = jsonStr.slice(0, braceEnd + 2);
  else {
    const bEnd = jsonStr.lastIndexOf("]");
    if (bEnd > 0 && bEnd < jsonStr.length - 1) jsonStr = jsonStr.slice(0, bEnd + 1);
  }

  let tasks: unknown[];
  try {
    const parsed = JSON.parse(jsonStr);
    if (!Array.isArray(parsed))
      return err(new Error("Decomposition output must be a JSON array"));
    tasks = parsed;
  } catch (e) {
    return err(new Error("Failed to parse decomposition JSON: " + (e as Error).message));
  }

  if (tasks.length === 0) return err(new Error("Decomposition produced zero subtasks"));

  // Validate against the canonical Zod schema
  const decomposeSchema = BUILTIN_TEMPLATES["task-decompose"]?.responseSchema;
  if (decomposeSchema) {
    const zodResult = decomposeSchema.safeParse(tasks);
    if (!zodResult.success) {
      s.logger?.warn(
        "[orchestrator] Decomposition failed Zod validation: " +
          zodResult.error.issues.map((i) => `${i.path.join(".")}: ${i.message}`).join("; "),
      );
    }
  }

  const validComplexities = new Set(["trivial", "simple", "moderate", "complex"]);
  const nodes: TaskNode[] = [];
  const ids = new Set<string>();

  for (let i = 0; i < tasks.length; i++) {
    const t = tasks[i] as Record<string, unknown>;
    const id = String(t.id ?? `task-${i + 1}`);
    if (ids.has(id)) return err(new Error(`Duplicate task id: ${id}`));
    ids.add(id);

    const complexity = String(t.estimatedComplexity ?? "moderate");
    if (!validComplexities.has(complexity))
      return err(new Error(`Invalid complexity "${complexity}" in task ${id}`));

    nodes.push({
      id,
      description: String(t.description ?? "").slice(0, 500),
      language: String(t.language ?? "text"),
      dependsOn: Array.isArray(t.dependsOn) ? (t.dependsOn as unknown[]).map(String) : [],
      estimatedComplexity: complexity as TaskNode["estimatedComplexity"],
      outputFiles: Array.isArray(t.outputFiles)
        ? (t.outputFiles as unknown[]).map(String).slice(0, 20)
        : [],
      verificationHint: String(t.verificationHint ?? "").slice(0, 200),
    });
  }

  for (const node of nodes) {
    if (node.dependsOn.includes(node.id))
      return err(new Error(`Task ${node.id} cannot depend on itself`));
    for (const dep of node.dependsOn) {
      if (!ids.has(dep)) return err(new Error(`Task ${node.id} depends on unknown task: ${dep}`));
    }
  }

  if (nodes.length > 24)
    return err(new Error(`Decomposition produced ${nodes.length} subtasks; maximum is 24`));

  const validLanguages = new Set([
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
  ]);
  for (const node of nodes) {
    if (!validLanguages.has(node.language)) {
      (node as unknown as Record<string, unknown>).language = "text";
    }
  }

  const sorted = topologicalSort(nodes);
  if (!sorted.ok) return err(sorted.error);

  const tokenEstimate =
    sorted.value.length * 400 + sorted.value.reduce((sum, n) => sum + n.description.length, 0);

  return ok({
    rootTask: "", // filled in by decomposeTask
    tasks: sorted.value,
    totalEstimatedTokens: tokenEstimate,
    rationale: `Decomposed into ${sorted.value.length} subtasks (${sorted.value.filter((n) => n.dependsOn.length > 0).length} with dependencies)`,
  });
}

/**
 * Kahn's algorithm topological sort with cycle detection. Returns
 * the input order for independent tasks.
 */
export function topologicalSort(
  nodes: TaskNode[],
): import("@kirkforge/core-types").Result<TaskNode[], Error> {
  const byId = new Map<string, TaskNode>();
  for (const n of nodes) byId.set(n.id, n);

  const inDegree = new Map<string, number>();
  const adj = new Map<string, string[]>();
  for (const n of nodes) {
    inDegree.set(n.id, 0);
    adj.set(n.id, []);
  }
  for (const n of nodes) {
    for (const dep of n.dependsOn) {
      adj.get(dep)?.push(n.id);
      inDegree.set(n.id, (inDegree.get(n.id) ?? 0) + 1);
    }
  }

  const queue: string[] = [];
  for (const [id, deg] of inDegree) {
    if (deg === 0) queue.push(id);
  }

  const sorted: TaskNode[] = [];
  while (queue.length > 0) {
    const id = queue.shift()!;
    sorted.push(byId.get(id)!);
    for (const next of adj.get(id) ?? []) {
      const newDeg = (inDegree.get(next) ?? 1) - 1;
      inDegree.set(next, newDeg);
      if (newDeg === 0) queue.push(next);
    }
  }

  if (sorted.length !== nodes.length) return err(new Error("Cycle detected in task dependencies"));
  return ok(sorted);
}

/**
 * Execute a previously-stored decomposition in dependency order. Each
 * subtask delegates through the full pipeline. Failed dependencies
 * cause dependent subtasks to be skipped (recorded as failed, not run).
 */
export async function executeDecomposition(
  s: OrchestratorInternals & {
    delegate: (t: TaskInput) => Promise<import("./types.js").OrchestratorResult>;
  },
  taskId: string,
  actor?: Actor,
): Promise<import("@kirkforge/core-types").Result<DecompositionExecutionResult, Error>> {
  if (s.shuttingDown) return err(new Error("Orchestrator is shutting down"));
  if (!s.memoryStore)
    return err(new Error("Memory store required for decomposition execution"));

  const recalled = await s.memoryStore.recallDecomposition(taskId);
  if (!recalled.ok) return err(recalled.error);
  if (!recalled.value || recalled.value.tasks.length === 0) {
    return err(new Error("No decomposition found for taskId: " + taskId));
  }

  if (s.policyEngine) {
    const { resolveProvider } = await import("./orchestrator-provider.js");
    const providerConfig = resolveProvider(s, null);
    const modelDecision = s.policyEngine.checkModel(providerConfig.defaultModel);
    if (!modelDecision.allowed) {
      auditPolicyDeny(s, "model.deny", modelDecision.reason, modelDecision.policyHash, actor);
      return err(
        new KirkForgeError("POLICY_DENIED", modelDecision.reason, {
          rule: modelDecision.rule,
          policyHash: modelDecision.policyHash,
        }),
      );
    }
    const languages = new Set<string>(recalled.value.tasks.map((t) => t.language ?? "unknown"));
    for (const lang of languages) {
      const toolDecision = s.policyEngine.checkTool(lang);
      if (!toolDecision.allowed) {
        auditPolicyDeny(
          s,
          "tool.deny",
          `Tool policy denies language "${lang}" in decomposition: ${toolDecision.reason}`,
          toolDecision.policyHash,
          actor,
        );
        return err(
          new KirkForgeError(
            "POLICY_DENIED",
            `Tool policy denies language "${lang}" in decomposition: ${toolDecision.reason}`,
            { rule: toolDecision.rule, policyHash: toolDecision.policyHash, language: lang },
          ),
        );
      }
    }
  }

  const tasks = recalled.value.tasks;
  const sorted = topologicalSort(tasks);
  if (!sorted.ok)
    return err(new Error("Stored decomposition has invalid dependency graph: " + sorted.error.message));
  const ordered = sorted.value;
  const completed = new Map<string, SubtaskExecutionResult>();
  const results: SubtaskExecutionResult[] = [];
  let totalTokens = 0;
  const startedAt = Date.now();

  for (const node of ordered) {
    for (const depId of node.dependsOn) {
      const depResult = completed.get(depId);
      if (!depResult)
        return err(
          new Error(
            "Dependency " + depId + " for task " + node.id + " was not found in execution plan",
          ),
        );
      if (!depResult.ok) {
        results.push({
          nodeId: node.id,
          ok: false,
          description: node.description,
          language: node.language,
          durationMs: 0,
          tokensUsed: 0,
          error: "Skipped: dependency " + depId + " failed",
        });
        completed.set(node.id, results[results.length - 1]!);
        continue;
      }
    }

    if (completed.has(node.id)) continue;

    const subtaskStartedAt = Date.now();
    const SUBTASK_TIMEOUT_MS = 5 * 60 * 1000;
    s.logger?.info(
      "[orchestrator] Executing subtask " + node.id + ": " + node.description.slice(0, 60),
    );

    let result: Awaited<ReturnType<typeof s.delegate>>;
    try {
      result = await Promise.race([
        s.delegate({
          taskId: taskId + "--" + node.id,
          description: node.description,
          suppressMemory: false,
          actor,
        }),
        new Promise<never>((_, reject) =>
          setTimeout(
            () =>
              reject(
                new Error("Subtask " + node.id + " timed out after " + SUBTASK_TIMEOUT_MS + "ms"),
              ),
            SUBTASK_TIMEOUT_MS,
          ),
        ),
      ]);
    } catch (e) {
      result = { ok: false as const, error: e instanceof Error ? e : new Error(String(e)) };
    }

    if (!result.ok) {
      s.logger?.warn(
        "[orchestrator] Subtask " +
          node.id +
          " failed on first attempt, retrying once: " +
          result.error.message,
      );
      try {
        result = await Promise.race([
          s.delegate({
            taskId: taskId + "--" + node.id + "-r",
            description: node.description,
            suppressMemory: false,
          }),
          new Promise<never>((_, reject) =>
            setTimeout(
              () =>
                reject(
                  new Error(
                    "Subtask " + node.id + " retry timed out after " + SUBTASK_TIMEOUT_MS + "ms",
                  ),
                ),
              SUBTASK_TIMEOUT_MS,
            ),
          ),
        ]);
      } catch (e) {
        result = { ok: false as const, error: e instanceof Error ? e : new Error(String(e)) };
      }
    }

    if (!result.ok) {
      const sr: SubtaskExecutionResult = {
        nodeId: node.id,
        ok: false,
        description: node.description,
        language: node.language,
        durationMs: Date.now() - subtaskStartedAt,
        tokensUsed: 0,
        error: result.error.message,
      };
      results.push(sr);
      completed.set(node.id, sr);
      continue;
    }

    const emission = result.value.emission;
    const packet = result.value.packet;
    const verdict = packet?.verification?.overall ?? "unknown";
    const files = extractWrittenFiles(result.value);

    totalTokens += emission.totalTokens;

    const sr: SubtaskExecutionResult = {
      nodeId: node.id,
      ok: verdict === "pass" || verdict === "warn",
      description: node.description,
      language: node.language,
      durationMs: Date.now() - subtaskStartedAt,
      tokensUsed: emission.totalTokens,
      verdict,
      files: files.length > 0 ? files : undefined,
    };
    results.push(sr);
    completed.set(node.id, sr);
  }

  const succeededCount = results.filter((r) => r.ok).length;
  const executionResult: DecompositionExecutionResult = {
    rootTask: recalled.value.description,
    results,
    totalSubtasks: ordered.length,
    succeededCount,
    failedCount: ordered.length - succeededCount,
    totalTokens,
    totalDurationMs: Date.now() - startedAt,
  };

  return ok(executionResult);
}
