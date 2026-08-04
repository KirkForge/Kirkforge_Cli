import type { TaskNode } from "@kirkforge/core-types";
import {
  parseDecomposition as realParseDecomposition,
  topologicalSort as realTopologicalSort,
} from "../../src/orchestrator-decompose.js";

// ── Test helpers for _parseDecomposition and _topologicalSort ───────────
// These used to be private methods on a MockOrchestrator class that
// re-implemented the production logic inline. After step 8 of the
// godfile refactor extracted the real methods to orchestrator-decompose.ts,
// the mock is no longer needed — the tests now call the real code
// directly via a thin adapter.

export interface ParseResult {
  ok: boolean;
  value?: {
    rootTask: string;
    tasks: TaskNode[];
    totalEstimatedTokens: number;
    rationale: string;
  };
  error?: Error;
}

export interface TopologicalSortResult {
  ok: boolean;
  value?: TaskNode[];
  error?: Error;
}

/** Minimal stub of OrchestratorInternals — only `logger` is touched by the real code. */
const stubInternals = { logger: undefined } as Parameters<typeof realParseDecomposition>[0];

export function makeOrchestrator(): {
  _parseDecomposition(raw: string): ParseResult;
  _topologicalSort(nodes: TaskNode[]): TopologicalSortResult;
} {
  return {
    _parseDecomposition(raw: string): ParseResult {
      return realParseDecomposition(stubInternals, raw) as ParseResult;
    },
    _topologicalSort(nodes: TaskNode[]): TopologicalSortResult {
      return realTopologicalSort(nodes) as TopologicalSortResult;
    },
  };
}
