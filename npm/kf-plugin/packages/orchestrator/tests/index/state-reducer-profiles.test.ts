import { describe, it, expect } from "vitest";
import { detectTaskProfile } from "../../src/task-profile.js";
import type { EmissionSchema } from "../../src/task-profile.js";

describe("StateReducer: task profile verifierPolicy coverage", () => {
  it("task profile has verifierPolicy for each language", () => {
    const languages = [
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
    ] as const;
    for (const lang of languages) {
      const profile = detectTaskProfile(`write a ${lang} program`);
      expect(profile.verifierPolicy).toBeDefined();
      expect(profile.verifierPolicy.required).toBeDefined();
      expect(profile.verifierPolicy.advisory).toBeDefined();
      expect(Array.isArray(profile.verifierPolicy.required)).toBe(true);
      expect(Array.isArray(profile.verifierPolicy.advisory)).toBe(true);
    }
  });

  it("typescript profile requires lint, types, security; advisory graph", () => {
    const profile = detectTaskProfile("write a typescript server endpoint");
    expect(profile.verifierPolicy.required).toEqual(["lint", "types", "security"]);
    expect(profile.verifierPolicy.advisory).toEqual(["graph"]);
  });

  it("python profile requires lint, types; advisory security, graph", () => {
    const profile = detectTaskProfile("write a python pandas script");
    expect(profile.verifierPolicy.required).toEqual(["lint", "types"]);
    expect(profile.verifierPolicy.advisory).toEqual(["security", "graph"]);
  });

  it("detectTaskProfile returns EmissionSchema-conforming object for python", () => {
    const schema: EmissionSchema = detectTaskProfile("write a python pandas script");
    expect(schema.language).toBe("python");
    expect(schema.defaultFile).toBe("solution.py");
    expect(schema.forbiddenExtensions).toContain(".ts");
    expect(schema.verifierPolicy.required).toContain("lint");
    expect(schema.verifierPolicy.required).toContain("types");
    expect(schema.verifierPolicy.advisory).toContain("security");
    expect(schema.fenceLanguages).toContain("python");
    expect(typeof schema.checkCommand).toBe("string");
    expect(typeof schema.promptHint).toBe("string");
    expect(Array.isArray(schema.allowedExtensions)).toBe(true);
    expect(Array.isArray(schema.forbiddenExtensions)).toBe(true);
  });
});
