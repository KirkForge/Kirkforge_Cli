import { describe, it, expect } from "vitest";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  doctor,
  verifyWorkspace,
  buildCorrectionPrompt,
  recordObservation,
  recallRoutingBias,
  createPluginCore,
  toolNames,
} from "../src/index.js";
import type { ReducedStatePacket, TaskLanguage } from "../src/index.js";
import { MemoryStore, InMemoryAdapter } from "@kirkforge/memory-palace";

function makeFailPacket(): ReducedStatePacket {
  return {
    taskId: "test-task",
    turn: 0,
    ts: new Date().toISOString(),
    verification: {
      lint: { errors: 3, warnings: 1 },
      types: { errors: 2 },
      security: { findings: 2, critical: 0, high: 1 },
      overall: "fail",
    },
    changes: {
      filesChanged: 2,
      paths: ["src/index.ts", "src/utils.ts"],
      insertions: 10,
      deletions: 5,
    },
    graph: { edgeCount: 5, newEdges: 0, brokenEdges: 0, cycles: 0 },
    contributingSignals: [],
  };
}

describe("doctor()", () => {
  it("returns a ToolCapabilityReport with tools array and languages array", async () => {
    const report = await doctor();
    expect(report).toBeDefined();
    expect(typeof report).toBe("object");

    const toolKeys = [
      "eslint",
      "tsc",
      "ruff",
      "pyright",
      "bandit",
      "secdev",
    ] as const;
    for (const key of toolKeys) {
      expect(report).toHaveProperty(key);
      expect(typeof report[key].available).toBe("boolean");
    }

    expect(Array.isArray(report.languages)).toBe(true);
    expect(report.languages.length).toBeGreaterThan(0);
  }, 30000);

  it("does not expose model, provider, or auth fields", async () => {
    const report = await doctor();
    const json = JSON.stringify(report);
    const parsed = JSON.parse(json);
    expect(parsed).not.toHaveProperty("model");
    expect(parsed).not.toHaveProperty("provider");
    expect(parsed).not.toHaveProperty("apiKey");
    expect(parsed).not.toHaveProperty("auth");
  }, 30000);

  it("internal tools have source internal and available true", async () => {
    const report = await doctor();
    expect(report.secdev.available).toBe(true);
    expect(report.secdev.source).toBe("internal");
  }, 30000);

  it("external tools have source external", async () => {
    const report = await doctor();
    expect(report.eslint.source).toBe("external");
    expect(report.tsc.source).toBe("external");
    expect(report.ruff.source).toBe("external");
    expect(report.pyright.source).toBe("external");
    expect(report.bandit.source).toBe("external");
  }, 30000);

  it("every tool entry has a source field", async () => {
    const report = await doctor();
    const toolEntries = [
      report.eslint,
      report.tsc,
      report.ruff,
      report.pyright,
      report.bandit,
      report.secdev,
    ];
    for (const entry of toolEntries) {
      expect(entry.source).toBeDefined();
      expect(["internal", "external"]).toContain(entry.source);
    }
  }, 30000);
});

describe("verifyWorkspace()", () => {
  it("returns err for nonexistent workspace directory", async () => {
    const tmp = mkdtempSync(join(tmpdir(), "kirkforge-vw-test-"));
    try {
      const result = await verifyWorkspace({
        workspace: join(tmp, "nonexistent-path-xyz"),
        language: "typescript",
        taskId: "vw-test-1",
      });
      expect(result.ok).toBe(false);
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("returns ok(packet) for empty workspace directory", async () => {
    const tmp = mkdtempSync(join(tmpdir(), "kirkforge-vw-test-"));
    try {
      const result = await verifyWorkspace({
        workspace: tmp,
        language: "typescript",
        taskId: "vw-test-2",
      });
      expect(result.ok).toBe(true);
      if (result.ok) {
        expect(result.value.verification.overall).toBe("fail");
      }
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("never returns err for a verifiable workspace (verifier failure is data)", async () => {
    const tmp = mkdtempSync(join(tmpdir(), "kirkforge-vw-test-"));
    try {
      const result = await verifyWorkspace({
        workspace: tmp,
        language: "python",
        taskId: "vw-test-3",
      });
      expect(result.ok).toBe(true);
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("uses Python emitters/policy when description implies Python and no language is given", async () => {
    const tmp = mkdtempSync(join(tmpdir(), "kirkforge-vw-test-"));
    try {
      const result = await verifyWorkspace({
        workspace: tmp,
        description: "write a python function that scrapes a web page",
        taskId: "vw-test-4",
      });
      expect(result.ok).toBe(true);
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("sanitizes file paths that escape workspace", async () => {
    const tmp = mkdtempSync(join(tmpdir(), "kirkforge-vw-test-"));
    try {
      mkdirSync(join(tmp, "src"), { recursive: true });
      writeFileSync(join(tmp, "src", "app.ts"), "const x = 1;");
      const result = await verifyWorkspace({
        workspace: tmp,
        files: ["../../etc/passwd", "src/app.ts", "../outside.ts"],
        language: "typescript",
        taskId: "vw-test-5",
      });
      expect(result.ok).toBe(false);
      expect(result.error).toBeDefined();
      expect(result.error!.message).toContain("rejected by path safety");
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });
});

describe("buildCorrectionPrompt()", () => {
  it("returns a non-empty string for a failing packet", () => {
    const packet = makeFailPacket();
    const prompt = buildCorrectionPrompt(packet, { language: "typescript" });
    expect(typeof prompt).toBe("string");
    expect(prompt.length).toBeGreaterThan(0);
    expect(prompt).toContain("lint");
    expect(prompt).toContain("type");
  });

  it("includes artifact enforcement info when present", () => {
    const packet: ReducedStatePacket = {
      ...makeFailPacket(),
      artifactEnforcement: {
        blocked: 1,
        blockedPaths: [{ path: ".env", reason: "dotfile not allowed" }],
        status: "fail",
      },
    };
    const prompt = buildCorrectionPrompt(packet);
    expect(prompt).toContain(".env");
  });

  it("includes Python tooling for Python language", () => {
    const packet = makeFailPacket();
    const prompt = buildCorrectionPrompt(packet, { language: "python" });
    expect(prompt).toContain("KirkForge Python lint engine");
    expect(prompt).toContain("pyright");
    expect(prompt).toContain("safety rules");
  });

  it("mentions broken edges when present", () => {
    const packet: ReducedStatePacket = {
      ...makeFailPacket(),
      graph: { edgeCount: 3, newEdges: 1, brokenEdges: 1, cycles: 0 },
    };
    const prompt = buildCorrectionPrompt(packet, { language: "typescript" });
    expect(prompt).toContain("broken");
  });

  it("mentions missing required verifiers", () => {
    const packet: ReducedStatePacket = {
      ...makeFailPacket(),
      verifierPolicy: {
        required: ["lint", "types", "security"],
        advisory: ["graph"],
        missingRequired: ["types"],
        skippedRequired: [],
      },
    };
    const prompt = buildCorrectionPrompt(packet, { language: "typescript" });
    expect(prompt).toContain("missing");
  });
});

describe("recordObservation() + recallRoutingBias()", () => {
  it("records a pass observation", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const result = await recordObservation(
      {
        taskId: "test-1",
        description: "write a hello world",
        language: "typescript",
        mode: "hard-prompt",
        model: "gpt-4",
        outcome: "pass",
        durationMs: 5000,
      },
      store,
    );
    expect(result.ok).toBe(true);
  });

  it("records a fail observation", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const result = await recordObservation(
      {
        taskId: "test-2",
        description: "fix a broken function",
        language: "python",
        mode: "artifact",
        model: "claude-sonnet",
        outcome: "fail",
        durationMs: 10000,
      },
      store,
    );
    expect(result.ok).toBe(true);
  });

  it("escalate is mapped to error internally (distinct from fail)", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const result = await recordObservation(
      {
        taskId: "test-escalate-mapping",
        description: "escalated task",
        language: "typescript",
        mode: "hard-prompt",
        model: "gpt-4",
        outcome: "escalate",
        durationMs: 3000,
      },
      store,
    );
    expect(result.ok).toBe(true);
    const observations = await adapter.query({ kind: "task-observation", limit: 10 });
    expect(observations.ok).toBe(true);
    const obs = observations.value[0]!;
    expect(obs.properties.outcome).toBe("error");
  });

  it("recallRoutingBias returns null on empty memory, never throws", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    const result = await recallRoutingBias("unseen task description", undefined, store);
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value).toBeNull();
    }
  });

  it("recallRoutingBias returns RoutingBias after observations are recorded", async () => {
    const store = new MemoryStore(new InMemoryAdapter());

    for (let i = 0; i < 5; i++) {
      await recordObservation(
        {
          taskId: `recall-test-${i}`,
          description: "implement user authentication",
          language: "typescript",
          mode: "hard-prompt",
          model: "claude-sonnet",
          outcome: i < 4 ? "pass" : "fail",
          durationMs: 5000 + i * 1000,
        },
        store,
      );
    }

    const result = await recallRoutingBias("implement user authentication", "claude-sonnet", store);
    expect(result.ok).toBe(true);
    if (result.ok && result.value) {
      expect(result.value).toHaveProperty("prefer");
      expect(result.value).toHaveProperty("avoid");
      expect(result.value).toHaveProperty("confidence");
      expect(result.value).toHaveProperty("evidence");
      expect(Array.isArray(result.value.prefer)).toBe(true);
      expect(Array.isArray(result.value.avoid)).toBe(true);
      expect(typeof result.value.confidence).toBe("number");
    }
  });
});

describe("createPluginCore()", () => {
  it("returns bound methods", () => {
    const store = new MemoryStore(new InMemoryAdapter());
    const core = createPluginCore({ memoryStore: store });
    expect(typeof core.verifyWorkspace).toBe("function");
    expect(typeof core.buildCorrectionPrompt).toBe("function");
    expect(typeof core.recordObservation).toBe("function");
    expect(typeof core.recallRoutingBias).toBe("function");
    expect(typeof core.doctor).toBe("function");
  });

  it("bound recordObservation uses provided MemoryStore", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    const core = createPluginCore({ memoryStore: store });

    const result = await core.recordObservation({
      taskId: "bound-test-1",
      description: "bound observation",
      language: "typescript",
      mode: "hard-prompt",
      model: "gpt-4",
      outcome: "pass",
      durationMs: 2000,
    });
    expect(result.ok).toBe(true);
  });

  it("bound recallRoutingBias uses provided MemoryStore", async () => {
    const store = new MemoryStore(new InMemoryAdapter());
    const core = createPluginCore({ memoryStore: store });

    const result = await core.recallRoutingBias("bound recall test");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value).toBeNull();
    }
  });

  it("bound recordObservation returns err without MemoryStore", async () => {
    const core = createPluginCore();
    const result = await core.recordObservation({
      taskId: "no-store",
      description: "no store",
      language: "typescript",
      mode: "hard-prompt",
      model: "gpt-4",
      outcome: "pass",
      durationMs: 1000,
    });
    expect(result.ok).toBe(false);
  });

  it("verifyWorkspace works as bound method", async () => {
    const core = createPluginCore();
    const tmp = mkdtempSync(join(tmpdir(), "kirkforge-core-vw-"));
    try {
      const result = await core.verifyWorkspace({
        workspace: tmp,
        language: "typescript",
        taskId: "core-vw-1",
      });
      expect(result.ok).toBe(true);
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  });

  it("buildCorrectionPrompt works as bound method", () => {
    const core = createPluginCore();
    const packet = makeFailPacket();
    const prompt = core.buildCorrectionPrompt(packet, { language: "python" });
    expect(prompt).toContain("KirkForge Python lint engine");
    expect(prompt).toContain("pyright");
    expect(prompt).toContain("safety rules");
  });
});

describe("exported types and values", () => {
  it("toolNames returns correct mapping for typescript", () => {
    const ts = toolNames("typescript");
    expect(ts).toEqual({
      lint: "KirkForge TypeScript lint engine",
      types: "tsc",
      security: "KirkForge TypeScript lint engine (safety rules)",
    });
  });

  it("toolNames returns correct mapping for python", () => {
    const py = toolNames("python");
    expect(py).toEqual({
      lint: "KirkForge Python lint engine",
      types: "pyright",
      security: "KirkForge Python lint engine (safety rules)",
    });
  });

  it("toolNames returns default for unknown language", () => {
    const def = toolNames("cobol" as TaskLanguage);
    expect(def).toEqual({ lint: "lint", types: "type-check", security: "security scanner" });
  });
});
