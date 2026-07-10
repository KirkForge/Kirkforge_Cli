import { describe, it, expect, vi, beforeAll, afterAll } from "vitest";
import { InMemoryAdapter, MemoryStore } from "@kirkforge/memory-palace";
import { ok } from "@kirkforge/core-types";
import { makePassPacket, TestableOrchestrator } from "./_helpers.js";

describe("validator integration: real shell commands", () => {
  // Shell-based validator tests can be slow due to child process spawning
  vi.setConfig({ testTimeout: 30000 });
  const prevAllowUnsafe = process.env.ALLOW_UNSAFE_VALIDATOR_SHELL;
  beforeAll(() => {
    process.env.ALLOW_UNSAFE_VALIDATOR_SHELL = "1";
  });
  afterAll(() => {
    if (prevAllowUnsafe === undefined) delete process.env.ALLOW_UNSAFE_VALIDATOR_SHELL;
    else process.env.ALLOW_UNSAFE_VALIDATOR_SHELL = prevAllowUnsafe;
  });
  const modelConfig = {
    providers: {
      "test-provider": {
        provider: "ollama" as const,
        baseUrl: "http://localhost",
        defaultModel: "test-model",
      },
    },
    defaultProvider: "test-provider",
  };

  it("validator exit 0 -> finalVerdict pass, sourceOfTruth task-validator, taskPass true", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const orchestrator = new TestableOrchestrator({ modelConfig, memoryStore: store });

    orchestrator.stubDelegate(async () => {
      const packet = makePassPacket("v-pass");
      return ok({
        decision: { mode: "hard-prompt" },
        emission: {
          agentId: "a1",
          content: "```python\nprint('ok')\n```",
          promptTokens: 10,
          completionTokens: 20,
          totalTokens: 30,
          model: "test-model",
          format: "hard-prompt" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: "v-pass",
            domain: "files",
            kind: "files.written",
            source: "agent",
            ts: new Date().toISOString(),
            value: { files: ["solution.py"] },
          },
        ],
        packet,
      });
    });

    const result = await orchestrator.runCorrectionLoop(
      { taskId: "v-pass", description: "write a python script" },
      { maxCorrections: 0, validator: { shellCommand: "true", timeoutMs: 5000 } },
    );

    expect(result.finalVerdict).toBe("pass");
    expect(result.sourceOfTruth).toBe("task-validator");
    expect(result.taskValidation.status).toBe("pass");
    expect(result.taskValidation.validator).toBe("true");
    expect(result.taskOutcome).toBe("pass");
  });

  it("validator exit 1 -> finalVerdict fail, taskPass false, no accept", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const orchestrator = new TestableOrchestrator({ modelConfig, memoryStore: store });

    orchestrator.stubDelegate(async () => {
      const packet = makePassPacket("v-fail");
      return ok({
        decision: { mode: "hard-prompt" },
        emission: {
          agentId: "a1",
          content: "```python\nprint('ok')\n```",
          promptTokens: 10,
          completionTokens: 20,
          totalTokens: 30,
          model: "test-model",
          format: "hard-prompt" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: "v-fail",
            domain: "files",
            kind: "files.written",
            source: "agent",
            ts: new Date().toISOString(),
            value: { files: ["solution.py"] },
          },
        ],
        packet,
      });
    });

    const result = await orchestrator.runCorrectionLoop(
      { taskId: "v-fail", description: "write a python script" },
      { maxCorrections: 0, validator: { shellCommand: "false", timeoutMs: 5000 } },
    );

    expect(result.finalVerdict).toBe("fail");
    expect(result.sourceOfTruth).toBe("task-validator");
    expect(result.taskValidation.status).toBe("fail");
    expect(result.taskValidation.validator).toBe("false");
    expect(result.taskOutcome).toBe("fail");
    expect(result.finalAction).toBe("escalate");
  });

  it("validator timeout -> finalVerdict unknown, taskPass null", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const orchestrator = new TestableOrchestrator({ modelConfig, memoryStore: store });

    orchestrator.stubDelegate(async () => {
      const packet = makePassPacket("v-timeout");
      return ok({
        decision: { mode: "hard-prompt" },
        emission: {
          agentId: "a1",
          content: "```python\nprint('ok')\n```",
          promptTokens: 10,
          completionTokens: 20,
          totalTokens: 30,
          model: "test-model",
          format: "hard-prompt" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: "v-timeout",
            domain: "files",
            kind: "files.written",
            source: "agent",
            ts: new Date().toISOString(),
            value: { files: ["solution.py"] },
          },
        ],
        packet,
      });
    });

    const result = await orchestrator.runCorrectionLoop(
      { taskId: "v-timeout", description: "write a python script" },
      { maxCorrections: 0, validator: { shellCommand: "sleep 30", timeoutMs: 500 } },
    );

    expect(result.finalVerdict).toBe("unknown");
    expect(result.sourceOfTruth).toBe("task-validator");
    expect(result.taskValidation.status).toBe("error");
    expect(result.taskOutcome).toBe("unknown");
    expect(result.finalAction).toBe("escalate");
  }, 15000);

  it("validator pass with verifier fail still uses task-validator as sourceOfTruth", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const orchestrator = new TestableOrchestrator({ modelConfig, memoryStore: store });

    let _callCount = 0;
    orchestrator.stubDelegate(async () => {
      _callCount++;
      const packet = {
        taskId: "v-src",
        turn: 0,
        ts: new Date().toISOString(),
        verification: {
          lint: { errors: 1, warnings: 0 },
          types: { errors: 1 },
          security: { findings: 0, critical: 0, high: 0 },
          overall: "fail" as const,
        },
        changes: { filesChanged: 1, paths: ["solution.py"], insertions: 5, deletions: 0 },
        graph: { edgeCount: 0, newEdges: 0, brokenEdges: 0, cycles: 0 },
        contributingSignals: [],
      };
      return ok({
        decision: { mode: "hard-prompt" },
        emission: {
          agentId: "a1",
          content: "```python\nprint('ok')\n```",
          promptTokens: 10,
          completionTokens: 20,
          totalTokens: 30,
          model: "test-model",
          format: "hard-prompt" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: "v-src",
            domain: "files",
            kind: "files.written",
            source: "agent",
            ts: new Date().toISOString(),
            value: { files: ["solution.py"] },
          },
        ],
        packet,
      });
    });

    const result = await orchestrator.runCorrectionLoop(
      { taskId: "v-src", description: "write a python script" },
      { maxCorrections: 0, validator: { shellCommand: "true", timeoutMs: 5000 } },
    );

    expect(result.finalVerdict).toBe("pass");
    expect(result.sourceOfTruth).toBe("task-validator");
    expect(result.taskValidation.status).toBe("pass");
  });

  it("validator result is JSON-serializable (CLI compatibility)", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const orchestrator = new TestableOrchestrator({ modelConfig, memoryStore: store });

    orchestrator.stubDelegate(async () => {
      const packet = makePassPacket("v-json");
      return ok({
        decision: { mode: "hard-prompt" },
        emission: {
          agentId: "a1",
          content: "```python\nprint('ok')\n```",
          promptTokens: 10,
          completionTokens: 20,
          totalTokens: 30,
          model: "test-model",
          format: "hard-prompt" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: "v-json",
            domain: "files",
            kind: "files.written",
            source: "agent",
            ts: new Date().toISOString(),
            value: { files: ["solution.py"] },
          },
        ],
        packet,
      });
    });

    const result = await orchestrator.runCorrectionLoop(
      { taskId: "v-json", description: "write a python script" },
      {
        maxCorrections: 0,
        validator: { shellCommand: "echo 'all tests passed'", timeoutMs: 5000 },
      },
    );

    const json = JSON.stringify({
      finalAction: result.finalAction,
      finalVerdict: result.finalVerdict,
      sourceOfTruth: result.sourceOfTruth,
      taskValidation: result.taskValidation,
      taskOutcome: result.taskOutcome,
    });
    const parsed = JSON.parse(json);
    expect(parsed.finalVerdict).toBe("pass");
    expect(parsed.sourceOfTruth).toBe("task-validator");
    expect(parsed.taskValidation.status).toBe("pass");
    expect(parsed.taskOutcome).toBe("pass");
  });
});
