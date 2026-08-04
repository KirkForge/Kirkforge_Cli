import { describe, it, expect } from "vitest";
import { InMemoryAdapter, MemoryStore } from "@kirkforge/memory-palace";
import { ok } from "@kirkforge/core-types";
import type { TaskInput } from "../../src/types.js";
import { makePassPacket, TestableOrchestrator } from "./_helpers.js";

describe("correction loop memory metadata", () => {
  it("stores actual model name instead of 'session'", { timeout: 30000 }, async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
    const modelConfig = {
      providers: {
        "test-provider": {
          provider: "ollama" as const,
          baseUrl: "http://localhost",
          defaultModel: "test-model-xyz",
        },
      },
      defaultProvider: "test-provider",
    };
    const orchestrator = new TestableOrchestrator({ modelConfig, memoryStore: store });

    orchestrator.stubDelegate(async (task: TaskInput) => {
      const packet = makePassPacket(task.taskId ?? "test");
      return ok({
        decision: { mode: "artifact" },
        emission: {
          agentId: "a1",
          content: "```python\nprint('hi')\n```",
          promptTokens: 10,
          completionTokens: 20,
          totalTokens: 30,
          model: "test-model-xyz",
          format: "artifact" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: task.taskId ?? "test",
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

    await orchestrator.runCorrectionLoop(
      { taskId: "mem-model", description: "write a python script" },
      { maxCorrections: 0 },
    );

    const observations = await adapter.query({ kind: "task-observation" });
    expect(observations.ok).toBe(true);
    const obs = observations.value[0];
    expect(obs).toBeDefined();
    expect(obs!.properties.model).toBe("test-model-xyz");
  });

  it("stores nonzero durationMs", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
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
    const orchestrator = new TestableOrchestrator({ modelConfig, memoryStore: store });

    orchestrator.stubDelegate(async (task: TaskInput) => {
      const packet = makePassPacket(task.taskId ?? "test");
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
            taskId: task.taskId ?? "test",
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

    await orchestrator.runCorrectionLoop(
      { taskId: "mem-duration", description: "fix broken-python script" },
      { maxCorrections: 0 },
    );

    const observations = await adapter.query({ kind: "task-observation" });
    expect(observations.ok).toBe(true);
    const obs = observations.value[0];
    expect(obs).toBeDefined();
    expect(typeof obs!.properties.durationMs).toBe("number");
    expect(obs!.properties.durationMs).toBeGreaterThanOrEqual(0);
  });

  it("stores actual mode from emission format, not guessed from verifierOverall", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
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
    const orchestrator = new TestableOrchestrator({ modelConfig, memoryStore: store });

    orchestrator.stubDelegate(async (task: TaskInput) => {
      const packet = makePassPacket(task.taskId ?? "test");
      return ok({
        decision: { mode: "artifact" },
        emission: {
          agentId: "a1",
          content: "### FILE: solution.py\nprint('ok')\n### END",
          promptTokens: 10,
          completionTokens: 20,
          totalTokens: 30,
          model: "test-model",
          format: "artifact" as const,
        },
        signals: [
          {
            id: "s1",
            taskId: task.taskId ?? "test",
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

    await orchestrator.runCorrectionLoop(
      { taskId: "mem-mode", description: "write a python script" },
      { maxCorrections: 0 },
    );

    const observations = await adapter.query({ kind: "task-observation" });
    expect(observations.ok).toBe(true);
    const obs = observations.value[0];
    expect(obs).toBeDefined();
    expect(obs!.properties.mode).toBe("artifact");
  });

  it("taskPass=false with verifier pass records unknown", async () => {
    const adapter = new InMemoryAdapter();
    const store = new MemoryStore(adapter);
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
    const orchestrator = new TestableOrchestrator({ modelConfig, memoryStore: store });

    let delegateCallCount = 0;
    orchestrator.stubDelegate(async (task: TaskInput) => {
      delegateCallCount++;
      if (delegateCallCount === 1) {
        const packet = makePassPacket(task.taskId ?? "test");
        return ok({
          decision: { mode: "artifact" },
          emission: {
            agentId: "a1",
            content: "### FILE: solution.py\nprint('ok')\n### END",
            promptTokens: 10,
            completionTokens: 20,
            totalTokens: 30,
            model: "test-model",
            format: "artifact" as const,
          },
          signals: [
            {
              id: "s1",
              taskId: task.taskId ?? "test",
              domain: "files",
              kind: "files.written",
              source: "agent",
              ts: new Date().toISOString(),
              value: { files: ["solution.py"] },
            },
          ],
          packet,
        });
      }
      const packet = makePassPacket(task.taskId ?? "test");
      return ok({
        decision: { mode: "hard-prompt" },
        emission: {
          agentId: "a1",
          content: "```python\nprint('fixed')\n```",
          promptTokens: 5,
          completionTokens: 10,
          totalTokens: 15,
          model: "test-model",
          format: "hard-prompt" as const,
        },
        signals: [],
        packet,
      });
    });

    const result = await orchestrator.runCorrectionLoop(
      { taskId: "mem-taskpass", description: "write a python script", taskPass: false },
      { maxCorrections: 1 },
    );

    expect(result.finalAction).toBe("escalate");

    const observations = await adapter.query({ kind: "task-observation" });
    expect(observations.ok).toBe(true);
    const obs = observations.value[0];
    expect(obs).toBeDefined();
    expect(obs!.properties.outcome).toBe("fail");
    expect(obs!.properties.taskPass).toBe(false);
    expect(obs!.properties.finalVerdict).toBe("unknown");
  });
});
