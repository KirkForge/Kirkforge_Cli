import { describe, it, expect } from "vitest";
import { PolicyEngine, PolicyDeniedError, type Policy } from "../src/index.js";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

describe("PolicyEngine — default deny", () => {
  it("denies all tools by default", () => {
    const engine = new PolicyEngine();
    const decision = engine.checkTool("eslint");
    expect(decision.allowed).toBe(false);
    expect(decision.reason).toContain("denied by default");
  });

  it("denies all models by default", () => {
    const engine = new PolicyEngine();
    const decision = engine.checkModel("gpt-4o");
    expect(decision.allowed).toBe(false);
  });

  it("denies all workspace paths by default", () => {
    const engine = new PolicyEngine();
    const decision = engine.checkWorkspace("/workspace/project");
    expect(decision.allowed).toBe(false);
  });

  it("denies shell execution by default", () => {
    const engine = new PolicyEngine();
    const decisions = engine.checkExecution({ command: "bash" });
    expect(decisions.some((d) => !d.allowed)).toBe(true);
  });

  it("denies network by default", () => {
    const engine = new PolicyEngine();
    const decisions = engine.checkExecution({ networkRequired: true });
    expect(decisions.some((d) => !d.allowed)).toBe(true);
  });
});

describe("PolicyEngine — with policy", () => {
  const policy: Policy = {
    version: 1,
    name: "test-policy",
    tools: {
      allowed: ["eslint", "tsc", "pyright"],
      denied: ["bash"],
      maxConcurrent: 2,
    },
    models: {
      allowed: ["gpt-4o", "claude-sonnet-4-20250514"],
      allowedProviders: ["openai", "anthropic"],
      maxTokensPerRequest: 8192,
    },
    workspaces: {
      allowedRoots: ["/workspace"],
      maxPathDepth: 10,
      allowSymlinks: false,
    },
    execution: {
      networkAllowed: false,
      maxRuntimeSeconds: 60,
      maxMemoryMb: 512,
      shellAllowed: false,
      allowedCommands: [],
    },
  };

  it("allows tools in the allowed list", () => {
    const engine = new PolicyEngine(policy);
    expect(engine.checkTool("eslint").allowed).toBe(true);
    expect(engine.checkTool("tsc").allowed).toBe(true);
    expect(engine.checkTool("pyright").allowed).toBe(true);
  });

  it("denies tools not in the allowed list", () => {
    const engine = new PolicyEngine(policy);
    expect(engine.checkTool("curl").allowed).toBe(false);
  });

  it("denies tools in the denied list even if in allowed", () => {
    const policyWithBash: Policy = {
      ...policy,
      tools: { allowed: ["eslint", "bash"], denied: ["bash"], maxConcurrent: 2 },
    };
    const engine = new PolicyEngine(policyWithBash);
    expect(engine.checkTool("bash").allowed).toBe(false);
  });

  it("allows models in the allowed list", () => {
    const engine = new PolicyEngine(policy);
    expect(engine.checkModel("gpt-4o").allowed).toBe(true);
    expect(engine.checkModel("claude-sonnet-4-20250514").allowed).toBe(true);
  });

  it("denies models not in the allowed list", () => {
    const engine = new PolicyEngine(policy);
    expect(engine.checkModel("gpt-3.5-turbo").allowed).toBe(false);
  });

  it("allows model with provider in the allowed list", () => {
    const engine = new PolicyEngine(policy);
    expect(engine.checkModel("gpt-4o", "openai").allowed).toBe(true);
    expect(engine.checkModel("claude-sonnet-4-20250514", "anthropic").allowed).toBe(true);
  });

  it("denies model with provider not in the allowed list", () => {
    const engine = new PolicyEngine(policy);
    expect(engine.checkModel("gpt-4o", "unknown").allowed).toBe(false);
  });

  it("allows workspace paths under allowed roots", () => {
    const engine = new PolicyEngine(policy);
    expect(engine.checkWorkspace("/workspace/my-project").allowed).toBe(true);
    expect(engine.checkWorkspace("/workspace").allowed).toBe(true);
  });

  it("denies workspace paths outside allowed roots", () => {
    const engine = new PolicyEngine(policy);
    expect(engine.checkWorkspace("/etc/passwd").allowed).toBe(false);
    expect(engine.checkWorkspace("/tmp/evil").allowed).toBe(false);
  });

  it("denies shell execution when shellAllowed is false", () => {
    const engine = new PolicyEngine(policy);
    const decisions = engine.checkExecution({ command: "bash" });
    expect(decisions.some((d) => !d.allowed)).toBe(true);
  });

  it("allows shell execution when shellAllowed and command is allowed", () => {
    const shellPolicy: Policy = {
      ...policy,
      execution: {
        ...policy.execution,
        shellAllowed: true,
        allowedCommands: ["node", "python3"],
      },
    };
    const engine = new PolicyEngine(shellPolicy);
    const allowed = engine.checkExecution({ command: "node" });
    expect(allowed.every((d) => d.allowed)).toBe(true);
    const allowed2 = engine.checkExecution({ command: "python3" });
    expect(allowed2.every((d) => d.allowed)).toBe(true);
    const denied = engine.checkExecution({ command: "bash" });
    expect(denied.some((d) => !d.allowed)).toBe(true);
  });

  it("returns policy hash", () => {
    const engine = new PolicyEngine(policy);
    expect(engine.getHash()).toBeTruthy();
    expect(engine.getHash().length).toBeGreaterThan(0);
  });
});

describe("PolicyEngine — tenant overrides", () => {
  const policy: Policy = {
    version: 1,
    name: "test-tenant-override",
    tools: {
      allowed: ["eslint", "tsc"],
      denied: [],
    },
    models: {
      allowed: ["gpt-4o"],
      allowedProviders: ["openai"],
    },
    workspaces: {
      allowedRoots: ["/workspace"],
    },
    execution: {
      networkAllowed: false,
    },
    tenantOverrides: {
      "tenant-alpha": {
        tools: { allowed: ["eslint", "tsc", "pyright"] },
      },
    },
  };

  it("forTenant returns base policy for unknown tenant", () => {
    const engine = new PolicyEngine(policy);
    const tenantPolicy = engine.forTenant("unknown");
    expect(tenantPolicy.tools.allowed).toEqual(["eslint", "tsc"]);
  });

  it("forTenant merges tenant overrides", () => {
    const engine = new PolicyEngine(policy);
    const tenantPolicy = engine.forTenant("tenant-alpha");
    expect(tenantPolicy.tools.allowed).toContain("pyright");
    expect(tenantPolicy.tools.allowed).toContain("eslint");
  });
});

describe("PolicyEngine — file loading", () => {
  it("loads policy from JSON file", () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-policy-test-"));
    const policyPath = join(tmpDir, "policy.json");
    const policy: Policy = {
      version: 1,
      name: "file-test",
      tools: { allowed: ["eslint"], denied: [] },
      models: { allowed: ["gpt-4o"], allowedProviders: ["openai"] },
      workspaces: { allowedRoots: ["/workspace"] },
      execution: { networkAllowed: false },
    };
    writeFileSync(policyPath, JSON.stringify(policy));

    try {
      const engine = new PolicyEngine();
      const result = engine.loadFromFile(policyPath);
      expect(result.ok).toBe(true);
      expect(engine.checkTool("eslint").allowed).toBe(true);
      expect(engine.checkTool("curl").allowed).toBe(false);
      expect(engine.getHash()).toBeTruthy();
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("fails to load from non-existent file", () => {
    const engine = new PolicyEngine();
    const result = engine.loadFromFile("/nonexistent/path/policy.json");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toBeTruthy();
    }
  });

  it("fails to load invalid policy", () => {
    const tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-policy-test-"));
    const policyPath = join(tmpDir, "policy.json");
    writeFileSync(policyPath, JSON.stringify({ invalid: true }));

    try {
      const engine = new PolicyEngine();
      const result = engine.loadFromFile(policyPath);
      expect(result.ok).toBe(false);
    } finally {
      rmSync(tmpDir, { recursive: true, force: true });
    }
  });
});

describe("PolicyDeniedError", () => {
  it("contains decision details", () => {
    const engine = new PolicyEngine();
    const decision = engine.checkTool("curl");
    const error = new PolicyDeniedError(decision);
    expect(error.code).toBe("POLICY_DENIED");
    expect(error.decision).toBe(decision);
    expect(error.message).toContain("curl");
  });
});
