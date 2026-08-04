import { describe, it, expect } from "vitest";
import { compilePrompt, BUILTIN_TEMPLATES } from "../src/index.js";

describe("compilePrompt", () => {
  it("compiles hard-prompt template with variables", () => {
    const result = compilePrompt(BUILTIN_TEMPLATES["hard-prompt"]!, {
      description: "build a server",
      variables: {
        language: "typescript",
        defaultFile: "output.ts",
        languageHint: "Emit TypeScript.",
        checkCommand: "npx tsc --noEmit",
      },
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.systemPrompt).toContain("Emit TypeScript");
      expect(result.value.userPrompt).toContain("build a server");
      expect(result.value.format).toBe("hard-prompt");
    }
  });

  it("compiles artifact template", () => {
    const result = compilePrompt(BUILTIN_TEMPLATES["artifact"]!, {
      description: "write server.ts",
      variables: {
        language: "typescript",
        defaultFile: "server.ts",
        languageHint: "Prefer .ts paths.",
        checkCommand: "npx tsc --noEmit",
        files: "",
      },
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.userPrompt).toContain("write server.ts");
      expect(result.value.format).toBe("artifact");
    }
  });

  it("compiles schema-contract template", () => {
    const result = compilePrompt(BUILTIN_TEMPLATES["schema-contract"]!, {
      description: "audit the codebase",
      variables: {
        language: "typescript",
        defaultFile: "audit.json",
        languageHint: "JSON output.",
        checkCommand: "",
      },
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.format).toBe("schema-contract");
      expect(result.value.systemPrompt).toContain("valid JSON");
    }
  });

  it("interpolates {{task}} as description", () => {
    const result = compilePrompt(BUILTIN_TEMPLATES["hard-prompt"]!, {
      description: "hello world",
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.userPrompt).toContain("hello world");
    }
  });
});

describe("BUILTIN_TEMPLATES", () => {
  it("has hard-prompt, schema-contract, artifact, and coder", () => {
    expect(BUILTIN_TEMPLATES["hard-prompt"]).toBeDefined();
    expect(BUILTIN_TEMPLATES["schema-contract"]).toBeDefined();
    expect(BUILTIN_TEMPLATES["artifact"]).toBeDefined();
    expect(BUILTIN_TEMPLATES["coder"]).toBeDefined();
  });
});
