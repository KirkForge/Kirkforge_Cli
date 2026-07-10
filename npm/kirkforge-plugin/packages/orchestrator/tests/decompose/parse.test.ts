import { describe, it, expect } from "vitest";
import { makeOrchestrator } from "./_helpers.js";

describe("_parseDecomposition", () => {
  const orch = makeOrchestrator();

  it("parses a valid task array", () => {
    const json = JSON.stringify([
      {
        id: "setup-project",
        description: "Initialize the project",
        language: "typescript",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: ["package.json"],
        verificationHint: "npm test passes",
      },
      {
        id: "add-auth",
        description: "Add authentication",
        language: "typescript",
        dependsOn: ["setup-project"],
        estimatedComplexity: "moderate",
        outputFiles: ["src/auth.ts"],
        verificationHint: "login endpoint works",
      },
    ]);
    const result = orch._parseDecomposition(json);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.tasks).toHaveLength(2);
    expect(result.value!.tasks[0]!.id).toBe("setup-project");
    expect(result.value!.tasks[1]!.id).toBe("add-auth");
  });

  it("rejects non-array output", () => {
    const result = orch._parseDecomposition('{"not": "an array"}');
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("must be a JSON array");
  });

  it("rejects empty array", () => {
    const result = orch._parseDecomposition("[]");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("zero subtasks");
  });

  it("rejects invalid JSON", () => {
    const result = orch._parseDecomposition("not json at all {{{");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("Failed to parse");
  });

  it("handles JSON inside markdown fences", () => {
    const json = JSON.stringify([
      {
        id: "task-1",
        description: "Do something",
        language: "python",
        dependsOn: [],
        estimatedComplexity: "trivial",
        outputFiles: [],
        verificationHint: "",
      },
    ]);
    const result = orch._parseDecomposition("```json\n" + json + "\n```");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.tasks).toHaveLength(1);
  });

  it("handles JSON with surrounding prose", () => {
    const json = JSON.stringify([
      {
        id: "x",
        description: "y",
        language: "shell",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ]);
    const result = orch._parseDecomposition("Here is the breakdown:\n" + json + "\nThat's all.");
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.tasks).toHaveLength(1);
  });

  it("rejects duplicate task ids", () => {
    const json = JSON.stringify([
      {
        id: "dup",
        description: "First",
        language: "typescript",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
      {
        id: "dup",
        description: "Second",
        language: "typescript",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ]);
    const result = orch._parseDecomposition(json);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("Duplicate");
  });

  it("rejects unknown dependency references", () => {
    const json = JSON.stringify([
      {
        id: "a",
        description: "Task A",
        language: "typescript",
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

  it("rejects self-dependency", () => {
    const json = JSON.stringify([
      {
        id: "setup",
        description: "Setup",
        language: "typescript",
        dependsOn: ["setup"],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ]);
    const result = orch._parseDecomposition(json);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("cannot depend on itself");
  });

  it("rejects invalid complexity values", () => {
    const json = JSON.stringify([
      {
        id: "a",
        description: "x",
        language: "rust",
        dependsOn: [],
        estimatedComplexity: "impossible",
        outputFiles: [],
        verificationHint: "",
      },
    ]);
    const result = orch._parseDecomposition(json);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("Invalid complexity");
  });

  it("rejects excessive number of subtasks", () => {
    const tasks = Array.from({ length: 25 }, (_, i) => ({
      id: `task-${i}`,
      description: `Task ${i}`,
      language: "text",
      dependsOn: [] as string[],
      estimatedComplexity: "trivial",
      outputFiles: [] as string[],
      verificationHint: "",
    }));
    const result = orch._parseDecomposition(JSON.stringify(tasks));
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error!.message).toContain("maximum is 24");
  });

  it("fills defaults for missing fields", () => {
    const json = JSON.stringify([{}]);
    const result = orch._parseDecomposition(json);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    const t = result.value!.tasks[0]!;
    expect(t.id).toBe("task-1");
    expect(t.description).toBe("");
    expect(t.language).toBe("text");
    expect(t.dependsOn).toEqual([]);
    expect(t.estimatedComplexity).toBe("moderate");
    expect(t.outputFiles).toEqual([]);
    expect(t.verificationHint).toBe("");
  });
});
