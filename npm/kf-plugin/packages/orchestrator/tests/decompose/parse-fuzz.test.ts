import { describe, it, expect } from "vitest";
import { makeOrchestrator } from "./_helpers.js";

describe("_parseDecomposition bracket heuristic fuzz", () => {
  const orch = makeOrchestrator();
  const validTask = JSON.stringify([
    {
      id: "x",
      description: "y",
      language: "text",
      dependsOn: [],
      estimatedComplexity: "simple",
      outputFiles: [],
      verificationHint: "",
    },
  ]);

  it("handles prose with brackets before JSON", () => {
    const input = "[note: this is important]\n" + validTask + "\n[end]";
    const result = orch._parseDecomposition(input);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.tasks).toHaveLength(1);
  });

  it("handles nested brackets in description strings", () => {
    const json = JSON.stringify([
      {
        id: "x",
        description: "Fix the [DEPRECATED] function",
        language: "text",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: [],
        verificationHint: "",
      },
    ]);
    const result = orch._parseDecomposition(json);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.tasks[0]!.description).toContain("[DEPRECATED]");
  });

  it("handles JSON wrapped in explanatory text with brackets", () => {
    const input =
      "Here is the plan [see footnote]:\n" + validTask + "\nThat covers everything [done].";
    const result = orch._parseDecomposition(input);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.tasks).toHaveLength(1);
  });

  it("handles outputFiles with bracket-like paths", () => {
    const json = JSON.stringify([
      {
        id: "x",
        description: "y",
        language: "text",
        dependsOn: [],
        estimatedComplexity: "simple",
        outputFiles: ["src/[id]/page.tsx"],
        verificationHint: "",
      },
    ]);
    const result = orch._parseDecomposition(json);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.tasks[0]!.outputFiles).toEqual(["src/[id]/page.tsx"]);
  });

  it("handles whitespace-heavy model output", () => {
    const input = "   \n\n  " + validTask + "  \n\n   ";
    const result = orch._parseDecomposition(input);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.value!.tasks).toHaveLength(1);
  });
});
