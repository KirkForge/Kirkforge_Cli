// ── Types ──────────────────────────────────────────────────────────────────

export interface DecompositionSubtask {
  id: string;
  description: string;
  language: string;
  dependsOn: string[];
  estimatedTokens: number;
  schemaContract?: string;
}

export interface DecompositionResult {
  rootTask: string;
  tasks: DecompositionSubtask[];
  totalEstimatedTokens: number;
  rationale: string;
}

export interface SubtaskExecutionResult {
  nodeId: string;
  ok: boolean;
  description: string;
  language: string;
  durationMs: number;
  tokensUsed: number;
  verdict?: string;
  error?: string;
  files?: string[];
}

export interface DecompositionExecutionResult {
  rootTask: string;
  results: SubtaskExecutionResult[];
  totalSubtasks: number;
  succeededCount: number;
  failedCount: number;
  totalTokens: number;
  totalDurationMs: number;
}

// ── Decomposition parsing ──────────────────────────────────────────────────

export interface ParsedDecomposition {
  subtasks: DecompositionSubtask[];
  totalEstimatedTokens: number;
  rationale: string;
}

/**
 * Parse a decomposition response from a model into structured subtasks.
 * Handles JSON blocks, numbered lists, and markdown-style task breakdowns.
 */
export function parseDecompositionResponse(content: string, rootTask: string): ParsedDecomposition {
  // Try JSON extraction first
  const jsonMatch = content.match(/```(?:json)?\s*\n?([\s\S]*?)```/);
  if (jsonMatch) {
    try {
      const parsed = JSON.parse(jsonMatch[1]!.trim());
      if (Array.isArray(parsed.subtasks || parsed.tasks || parsed.steps)) {
        const tasks = (parsed.subtasks ?? parsed.tasks ?? parsed.steps).map(
          (t: Record<string, unknown>, i: number) => ({
            id: (t.id as string) ?? `sub-${i + 1}`,
            description: (t.description as string) ?? (t.task as string) ?? rootTask,
            language: (t.language as string) ?? "typescript",
            dependsOn: Array.isArray(t.dependsOn) ? (t.dependsOn as string[]) : [],
            estimatedTokens: (t.estimatedTokens as number) ?? 500,
            schemaContract: t.schemaContract as string | undefined,
          }),
        );
        return {
          subtasks: tasks,
          totalEstimatedTokens:
            (parsed.totalEstimatedTokens as number) ??
            tasks.reduce((s: number, t: DecompositionSubtask) => s + t.estimatedTokens, 0),
          rationale: (parsed.rationale as string) ?? "Decomposition from structured response",
        };
      }
    } catch {
      // Fall through to text parsing
    }
  }

  // Try inline JSON
  const inlineJsonMatch = content.match(/\{[\s\S]*"subtasks"[\s\S]*\}|\{[\s\S]*"tasks"[\s\S]*\}/);
  if (inlineJsonMatch) {
    try {
      const parsed = JSON.parse(inlineJsonMatch[0]!);
      if (Array.isArray(parsed.subtasks ?? parsed.tasks)) {
        const tasks = (parsed.subtasks ?? parsed.tasks).map(
          (t: Record<string, unknown>, i: number) => ({
            id: (t.id as string) ?? `sub-${i + 1}`,
            description: (t.description as string) ?? rootTask,
            language: (t.language as string) ?? "typescript",
            dependsOn: Array.isArray(t.dependsOn) ? (t.dependsOn as string[]) : [],
            estimatedTokens: (t.estimatedTokens as number) ?? 500,
          }),
        );
        return {
          subtasks: tasks,
          totalEstimatedTokens: tasks.reduce(
            (s: number, t: DecompositionSubtask) => s + t.estimatedTokens,
            0,
          ),
          rationale: (parsed.rationale as string) ?? "Decomposition from inline JSON",
        };
      }
    } catch {
      // Fall through to text parsing
    }
  }

  // Fallback: parse numbered/bulleted list
  const lines = content
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);
  const subtasks: DecompositionSubtask[] = [];
  let rationale = "Auto-decomposed from text response";

  for (const line of lines) {
    const numberedMatch = line.match(/^(\d+)[.)]\s+(.+)/);
    const bulletMatch = line.match(/^[-*]\s+(.+)/);
    const match = numberedMatch ?? bulletMatch;
    if (match) {
      const idx = numberedMatch ? parseInt(numberedMatch[1]!) - 1 : subtasks.length;
      const desc = (numberedMatch ? numberedMatch[2] : bulletMatch![1])!.trim();
      subtasks.push({
        id: `sub-${idx + 1}`,
        description: desc,
        language: "typescript",
        dependsOn: idx > 0 ? [`sub-${idx}`] : [],
        estimatedTokens: 500,
      });
    }
  }

  if (subtasks.length === 0) {
    // Single-task fallback
    subtasks.push({
      id: "sub-1",
      description: rootTask,
      language: "typescript",
      dependsOn: [],
      estimatedTokens: 1000,
    });
    rationale = "Could not decompose; treating as single task";
  }

  return {
    subtasks,
    totalEstimatedTokens: subtasks.reduce((s, t) => s + t.estimatedTokens, 0),
    rationale,
  };
}

// ── Topological sort ───────────────────────────────────────────────────────

/**
 * Sort subtasks by their dependency order for sequential execution.
 * Returns an ordered array where each task appears after all its dependencies.
 */
export function topologicalSort(tasks: DecompositionSubtask[]): DecompositionSubtask[] {
  const inDegree = new Map<string, number>();
  const adjacency = new Map<string, string[]>();

  for (const task of tasks) {
    inDegree.set(task.id, 0);
    adjacency.set(task.id, []);
  }

  for (const task of tasks) {
    for (const dep of task.dependsOn) {
      if (adjacency.has(dep)) {
        adjacency.get(dep)!.push(task.id);
        inDegree.set(task.id, (inDegree.get(task.id) ?? 0) + 1);
      }
    }
  }

  const queue: string[] = [];
  for (const [id, degree] of inDegree) {
    if (degree === 0) queue.push(id);
  }

  const sorted: DecompositionSubtask[] = [];
  while (queue.length > 0) {
    const id = queue.shift()!;
    const task = tasks.find((t) => t.id === id);
    if (task) sorted.push(task);
    for (const next of adjacency.get(id) ?? []) {
      inDegree.set(next, (inDegree.get(next) ?? 1) - 1);
      if (inDegree.get(next) === 0) queue.push(next);
    }
  }

  // Add any remaining tasks not reachable from dependencies
  for (const task of tasks) {
    if (!sorted.find((t) => t.id === task.id)) {
      sorted.push(task);
    }
  }

  return sorted;
}
