import { Orchestrator } from "../../src/index.js";
import type { OrchestratorConfig } from "../../src/index.js";
import type { TaskInput } from "../../src/types.js";

export function makePassPacket(taskId: string) {
  return {
    taskId,
    turn: 0,
    ts: new Date().toISOString(),
    verification: {
      lint: { errors: 0, warnings: 0 },
      types: { errors: 0 },
      security: { findings: 0, critical: 0, high: 0 },
      overall: "pass" as const,
    },
    changes: { filesChanged: 1, paths: ["solution.py"], insertions: 5, deletions: 0 },
    graph: { edgeCount: 0, newEdges: 0, brokenEdges: 0, cycles: 0 },
    contributingSignals: [],
  };
}

export class TestableOrchestrator extends Orchestrator {
  private _stubDelegate: ((task: TaskInput) => Promise<any>) | null = null;
  constructor(config: OrchestratorConfig) {
    super(config);
  }
  stubDelegate(fn: (task: TaskInput) => Promise<any>) {
    this._stubDelegate = fn;
  }
  override async delegate(task: TaskInput) {
    if (this._stubDelegate) return this._stubDelegate(task);
    return super.delegate(task);
  }
}
