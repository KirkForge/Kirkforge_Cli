import { describe, it, expect } from "vitest";
import type { TaskNode } from "@kirkforge/core-types";
import { makeOrchestrator } from "./_helpers.js";

describe("_topologicalSort", () => {
  const orch = makeOrchestrator();

  it("preserves independent tasks in input order", () => {
    const nodes: TaskNode[] = [
      {
        id: "b",
        description: "",
        language: "text",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "a",
        description: "",
        language: "text",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ];
    const result = orch._topologicalSort(nodes);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.map((n) => n.id)).toEqual(["b", "a"]);
  });

  it("orders dependencies correctly", () => {
    const nodes: TaskNode[] = [
      {
        id: "c",
        description: "",
        language: "text",
        dependsOn: ["b"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "a",
        description: "",
        language: "text",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "b",
        description: "",
        language: "text",
        dependsOn: ["a"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ];
    const result = orch._topologicalSort(nodes);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.map((n) => n.id)).toEqual(["a", "b", "c"]);
  });

  it("detects cycles", () => {
    const nodes: TaskNode[] = [
      {
        id: "a",
        description: "",
        language: "text",
        dependsOn: ["b"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "b",
        description: "",
        language: "text",
        dependsOn: ["a"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ];
    const result = orch._topologicalSort(nodes);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("Cycle");
  });

  it("handles a 5-node diamond dependency graph", () => {
    const nodes: TaskNode[] = [
      {
        id: "setup",
        description: "",
        language: "typescript",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "backend",
        description: "",
        language: "typescript",
        dependsOn: ["setup"],
        estimatedComplexity: "moderate",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "frontend",
        description: "",
        language: "typescript",
        dependsOn: ["setup"],
        estimatedComplexity: "moderate",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "integration",
        description: "",
        language: "typescript",
        dependsOn: ["backend", "frontend"],
        estimatedComplexity: "complex",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "deploy",
        description: "",
        language: "shell",
        dependsOn: ["integration"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ];
    const result = orch._topologicalSort(nodes);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    const ids = result.value!.map((n) => n.id);
    expect(ids.indexOf("setup")).toBe(0);
    expect(ids.indexOf("backend")).toBeLessThan(ids.indexOf("integration"));
    expect(ids.indexOf("frontend")).toBeLessThan(ids.indexOf("integration"));
    expect(ids.indexOf("integration")).toBeLessThan(ids.indexOf("deploy"));
  });
});
