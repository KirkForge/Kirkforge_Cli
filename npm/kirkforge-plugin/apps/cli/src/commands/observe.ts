import type { Command } from "commander";
import { recordObservation } from "@kirkforge/plugin";
import { ALL_MODES, resolveMemoryStore } from "../shared.js";

export function registerObserve(program: Command): void {
  program
    .command("observe")
    .description("Record a task observation to memory")
    .option("--workspace <path>", "Workspace path (enables tenant-scoped memory)")
    .option("--memory <path>", "Path to the memory store file")
    .option("--sqlite", "Use SQLite adapter instead of file-based")
    .requiredOption("--task-id <id>", "Task identifier")
    .requiredOption("--description <text>", "Task description")
    .requiredOption("--language <lang>", "Task language")
    .requiredOption("--mode <mode>", "Delegation mode")
    .requiredOption("--model <model>", "Worker model used")
    .requiredOption("--outcome <result>", "Task outcome: pass, fail, or escalate")
    .requiredOption("--duration-ms <n>", "Duration in milliseconds")
    .option("--tokens <n>", "Token count")
    .action(async (opts) => {
      if (!opts.workspace && !opts.memory) {
        console.error("Error: either --workspace or --memory is required");
        process.exit(1);
      }
      if (!ALL_MODES.includes(opts.mode)) {
        console.error(`Error: --mode must be one of: ${ALL_MODES.join(", ")}`);
        process.exit(1);
      }
      const validOutcomes = ["pass", "fail", "escalate"];
      if (!validOutcomes.includes(opts.outcome)) {
        console.error(`Error: --outcome must be one of: ${validOutcomes.join(", ")}`);
        process.exit(1);
      }

      const durationMs = parseInt(opts.durationMs, 10);
      if (Number.isNaN(durationMs) || durationMs < 0) {
        console.error("Error: --duration-ms must be a non-negative integer");
        process.exit(1);
      }

      const tokens = opts.tokens ? parseInt(opts.tokens, 10) : undefined;
      if (tokens !== undefined && (Number.isNaN(tokens) || tokens < 0)) {
        console.error("Error: --tokens must be a non-negative integer");
        process.exit(1);
      }

      const { store, adapter } = await resolveMemoryStore(opts);

      const result = await recordObservation(
        {
          taskId: opts.taskId,
          description: opts.description,
          language: opts.language,
          mode: opts.mode,
          model: opts.model,
          outcome: opts.outcome,
          durationMs,
          tokens,
        },
        store,
      );

      if (!result.ok) {
        console.error(`Error: ${result.error.message}`);
        process.exit(1);
      }

      await adapter.persist();

      console.log(JSON.stringify({ ok: true, taskId: opts.taskId, outcome: opts.outcome }));
    });
}
