import type { LintRule } from "@kirkforge/tool-lint-core";

export const perfRules: LintRule[] = [
  {
    id: "no-sync-in-async",
    category: "perf",
    severity: "med",
    pattern: /\bfs\.\w+Sync\s*\(/g,
    message: "Synchronous fs call in async context; use async version instead",
  },
  {
    id: "prefer-array-methods",
    category: "perf",
    severity: "low",
    pattern: /\bfor\s*\(.*\.length\s*;[\s\S]*?\)\s*\{[\s\S]*?\.push\(/g,
    message: "Prefer .map() or .filter() over imperative loops with push",
  },
  {
    id: "no-unnecessary-spread",
    category: "perf",
    severity: "low",
    pattern: /\[\.\.\.\w+\]\.(forEach|map|filter|reduce)/g,
    message: "Unnecessary spread copy before iteration; use the original array",
  },
];
