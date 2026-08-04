import type { Command } from "commander";
import { verifyWorkspace } from "@kirkforge/plugin";

export function registerVerifyWorkspace(program: Command): void {
  program
    .command("verify-workspace")
    .description("Run deterministic verification on a workspace and output a ReducedStatePacket")
    .requiredOption("--workspace <path>", "Path to the workspace directory")
    .option("--file <paths...>", "Specific files to verify")
    .option("--language <lang>", "Task language (typescript, javascript, python, etc.)")
    .option("--description <text>", "Task description for language profile detection")
    .option("--task-id <id>", "Task identifier for the verification run")
    .action(async (opts) => {
      const result = await verifyWorkspace({
        workspace: opts.workspace,
        files: opts.file,
        language: opts.language,
        description: opts.description,
        taskId: opts.taskId,
      });

      if (!result.ok) {
        console.error(`Error: ${result.error.message}`);
        process.exit(1);
      }

      console.log(JSON.stringify(result.value));
    });
}
