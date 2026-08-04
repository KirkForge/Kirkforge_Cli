import type { Command } from "commander";
import { recallRoutingBias } from "@kirkforge/plugin";
import { resolveMemoryStore } from "../shared.js";

export function registerRecall(program: Command): void {
  program
    .command("recall")
    .description("Recall routing bias from past task observations")
    .option("--workspace <path>", "Workspace path (enables tenant-scoped memory)")
    .option("--memory <path>", "Path to the memory store file")
    .option("--sqlite", "Use SQLite adapter instead of file-based")
    .requiredOption("--description <text>", "Task description to match")
    .option("--model <model>", "Worker model to filter by")
    .action(async (opts) => {
      if (!opts.workspace && !opts.memory) {
        console.error("Error: either --workspace or --memory is required");
        process.exit(1);
      }
      const { store } = await resolveMemoryStore(opts);
      const result = await recallRoutingBias(opts.description, opts.model, store);

      if (!result.ok) {
        console.error(`Error: ${result.error.message}`);
        process.exit(1);
      }

      console.log(JSON.stringify({ ok: true, bias: result.value }));
    });
}
