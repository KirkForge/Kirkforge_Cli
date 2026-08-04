import type { Command } from "commander";
import { readFileSync } from "node:fs";
import { ReducedStatePacketSchema } from "@kirkforge/core-schemas";
import { buildCorrectionPrompt } from "@kirkforge/plugin";

export function registerPrompt(program: Command): void {
  program
    .command("prompt")
    .description("Build a correction prompt from a verification packet")
    .requiredOption("--packet <path>", "Path to a ReducedStatePacket JSON file")
    .option("--language <lang>", "Task language for tool name resolution")
    .action((opts) => {
      let raw: string;
      try {
        raw = readFileSync(opts.packet, "utf-8");
      } catch {
        console.error(`Error: cannot read file: ${opts.packet}`);
        process.exit(1);
      }

      let parsed: unknown;
      try {
        parsed = JSON.parse(raw);
      } catch {
        console.error(`Error: invalid JSON in file: ${opts.packet}`);
        process.exit(1);
      }

      if (typeof parsed !== "object" || parsed === null || !parsed) {
        console.error("Error: packet JSON is not an object");
        process.exit(1);
      }

      const result = ReducedStatePacketSchema.safeParse(parsed);
      if (!result.success) {
        console.error("Error: packet shape is not a valid ReducedStatePacket");
        for (const issue of result.error.issues) {
          console.error(`  - ${issue.path.join(".")}: ${issue.message}`);
        }
        process.exit(1);
      }
      const packet = result.data;

      let prompt: string;
      try {
        prompt = buildCorrectionPrompt(packet, { language: opts.language });
      } catch {
        console.error("Error: failed to build correction prompt from packet");
        process.exit(1);
      }

      process.stdout.write(prompt);
      process.stdout.write("\n");
    });
}
