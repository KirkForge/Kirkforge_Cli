import { describe, it, expect } from "vitest";
import { classifyTask } from "../../src/classifier.js";

describe("classifyTask", () => {
  it("audit → schema-contract", () => {
    expect(classifyTask({ description: "audit the security report" }).mode).toBe("schema-contract");
  });
  it("file creation → artifact", () => {
    expect(classifyTask({ description: "generate a component file" }).mode).toBe("artifact");
  });
  it("artifact overrides schema-contract for code-gen with mixed keywords", () => {
    expect(classifyTask({ description: "write a TypeScript server with validation" }).mode).toBe(
      "artifact",
    );
  });
  it("defaults to hard-prompt", () => {
    expect(classifyTask({ description: "hello world" }).mode).toBe("hard-prompt");
  });
  it("user override respected", () => {
    const d = classifyTask({ description: "build", modeOverride: "artifact" });
    expect(d.mode).toBe("artifact");
    expect(d.autoRouted).toBe(false);
  });
});
