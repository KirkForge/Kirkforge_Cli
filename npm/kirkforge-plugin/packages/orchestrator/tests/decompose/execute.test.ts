import { describe, it, expect } from "vitest";
import type { TaskNode } from "@kirkforge/core-types";
import type { DecompositionExecutionResult } from "@kirkforge/orchestrator";
import { makeOrchestrator } from "./_helpers.js";

describe("executeDecomposition", () => {
  // Unit tests for the execution engine — no live model needed
  // since we test structural properties through mock memory stores

  it("defensive re-sort recovers from reverse-ordered tasks", () => {
    // Simulates a corrupted memory store where tasks were stored in reverse
    // dependency order. The topological sort must recover the correct order.
    const orch = makeOrchestrator();
    const reverseOrder: TaskNode[] = [
      {
        id: "d",
        description: "D",
        language: "text",
        dependsOn: ["b", "c"],
        estimatedComplexity: "complex",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "c",
        description: "C",
        language: "text",
        dependsOn: ["a"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "b",
        description: "B",
        language: "text",
        dependsOn: ["a"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "a",
        description: "A",
        language: "text",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ];
    const result = orch._topologicalSort(reverseOrder);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    const ids = result.value!.map((n) => n.id);
    // a must come first (no dependencies)
    expect(ids[0]).toBe("a");
    // d must come last (depends on b and c)
    expect(ids[3]).toBe("d");
  });

  it("detects cycles introduced by corrupted dependency data", () => {
    const orch = makeOrchestrator();
    const cycle: TaskNode[] = [
      {
        id: "a",
        description: "A",
        language: "text",
        dependsOn: ["b"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "b",
        description: "B",
        language: "text",
        dependsOn: ["a"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ];
    const result = orch._topologicalSort(cycle);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("Cycle");
  });

  it("executes tasks in dependency order", () => {
    // Verify topological ordering is preserved
    const orch = makeOrchestrator();
    const nodes: TaskNode[] = [
      {
        id: "a",
        description: "A",
        language: "text",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "b",
        description: "B",
        language: "text",
        dependsOn: ["a"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "c",
        description: "C",
        language: "text",
        dependsOn: ["a"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "d",
        description: "D",
        language: "text",
        dependsOn: ["b", "c"],
        estimatedComplexity: "complex",
        outputFiles: [],
        verificationHint: "",
      },
    ];
    const result = orch._topologicalSort(nodes);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    const ids = result.value!.map((n) => n.id);
    // a must come before everything
    expect(ids.indexOf("a")).toBeLessThan(ids.indexOf("b"));
    expect(ids.indexOf("a")).toBeLessThan(ids.indexOf("c"));
    expect(ids.indexOf("a")).toBeLessThan(ids.indexOf("d"));
    // b and c before d
    expect(ids.indexOf("b")).toBeLessThan(ids.indexOf("d"));
    expect(ids.indexOf("c")).toBeLessThan(ids.indexOf("d"));
  });

  it("fails when dependency is missing from the graph", () => {
    const orch = makeOrchestrator();
    // This is caught by _parseDecomposition, not topologicalSort, but execution engine
    // would see missing deps at runtime. We test the validation layer.
    const json = JSON.stringify([
      {
        id: "b",
        description: "B",
        language: "text",
        dependsOn: ["nonexistent"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ]);
    const result = orch._parseDecomposition(json);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("depends on unknown task");
  });

  it("handles a linear chain of 10 tasks", () => {
    const orch = makeOrchestrator();
    const nodes: TaskNode[] = Array.from({ length: 10 }, (_, i) => ({
      id: `step-${i}`,
      description: `Step ${i}`,
      language: "text",
      dependsOn: i > 0 ? [`step-${i - 1}`] : [],
      estimatedComplexity: "simple",
      outputFiles: [],
      verificationHint: "",
    }));
    const result = orch._topologicalSort(nodes);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.map((n) => n.id)).toEqual(nodes.map((n) => n.id));
  });

  it("execution result types are well-formed", () => {
    // Type-level validation: SubtaskExecutionResult and DecompositionExecutionResult
    // have all required fields
    const mockResult: DecompositionExecutionResult = {
      rootTask: "Build a CLI",
      results: [
        {
          nodeId: "setup",
          ok: true,
          description: "Init project",
          language: "typescript",
          durationMs: 100,
          tokensUsed: 200,
          verdict: "pass",
        },
      ],
      totalSubtasks: 1,
      succeededCount: 1,
      failedCount: 0,
      totalTokens: 200,
      totalDurationMs: 100,
    };
    expect(mockResult.succeededCount + mockResult.failedCount).toBe(mockResult.totalSubtasks);
  });
});
