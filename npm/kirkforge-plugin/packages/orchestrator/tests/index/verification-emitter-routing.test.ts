import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { createVerificationEmitters } from "../../src/emitter-factory.js";

describe("verification emitter routing", () => {
  it("uses Python verifiers for Python task profiles", () => {
    const emitters = createVerificationEmitters("/tmp", new EventBus(), ["solution.ts"], "python");
    expect(emitters.lint.constructor.name).toBe("LintEngine");
    expect(emitters.types.constructor.name).toBe("PyrightEmitter");
    // security is a dedicated SecurityEmitter (obfuscated dangerous-call scan),
    // not an alias of the lint engine.
    expect(emitters.security.constructor.name).toBe("SecurityEmitter");
    expect(emitters.security).not.toBe(emitters.lint);
  });

  it("falls back to Python verifiers for Python-only artifact extensions", () => {
    const emitters = createVerificationEmitters("/tmp", new EventBus(), ["solution.py"]);
    expect(emitters.lint.constructor.name).toBe("LintEngine");
  });

  it("keeps the TypeScript verifier stack for JS/TS artifacts", () => {
    const emitters = createVerificationEmitters("/tmp", new EventBus(), ["src/index.ts"]);
    expect(emitters.lint.constructor.name).toBe("LintEngine");
    expect(emitters.types.constructor.name).toBe("TscEmitter");
    expect(emitters.security.constructor.name).toBe("SecurityEmitter");
    expect(emitters.graph.constructor.name).toBe("GraphEmitter");
  });
});
