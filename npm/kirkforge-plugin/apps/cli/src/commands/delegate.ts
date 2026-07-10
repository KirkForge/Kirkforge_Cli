import type { Command } from "commander";
import { createBootstrap } from "../bootstrap.js";
import { ALL_MODES, exitError } from "../shared.js";

export function registerDelegate(program: Command): void {
  program
    .command("delegate")
    .description("Delegate a task to the orchestrator with automatic mode routing")
    .argument("<description>", "Task description")
    .option("--mode <mode>", `Delegation mode: ${ALL_MODES.join(", ")}`)
    .option("--provider <key>", "Provider key from config (e.g. openai, local-ollama)")
    .option("--context <text>", "Additional context for the task")
    .option("--file <paths...>", "Target files for the task")
    .option("--json", "JSON output")
    .action(async (description, opts) => {
      if (opts.mode && !ALL_MODES.includes(opts.mode)) {
        exitError(`--mode must be one of: ${ALL_MODES.join(", ")}`, opts.json);
      }
      const {
        orchestrator,
        shutdown: _shutdown,
        policyEngine: _policyEngine,
        auditLogger: _auditLogger,
      } = await createBootstrap(opts);
      const result = await orchestrator.delegate({
        description,
        modeOverride: opts.mode,
        context: opts.context,
        files: opts.file,
      });

      if (opts.json) {
        if (result.ok) {
          console.log(
            JSON.stringify(
              {
                mode: result.value.decision.mode,
                content: result.value.emission.content,
                tokens: result.value.emission.totalTokens,
                model: result.value.emission.model,
                packet: result.value.packet ?? null,
              },
              null,
              2,
            ),
          );
        } else {
          console.log(JSON.stringify({ error: result.error.message }, null, 2));
          process.exit(1);
        }
      } else {
        if (result.ok) {
          console.log(
            `\n[Mode: ${result.value.decision.mode}] [${result.value.emission.model}] [${result.value.emission.totalTokens} tokens]`,
          );
          console.log(result.value.emission.content);
          if (result.value.packet) {
            const p = result.value.packet;
            console.log(`\n--- Verification ---`);
            console.log(
              `  Lint:  ${p.verification.lint.errors} errors, ${p.verification.lint.warnings} warnings`,
            );
            console.log(`  Types: ${p.verification.types.errors} errors`);
            console.log(
              `  Security: ${p.verification.security.findings} findings (${p.verification.security.critical} critical)`,
            );
            console.log(`  Changes: ${p.changes.filesChanged} files`);
            console.log(`  Verdict: ${p.verification.overall}`);
          }
        } else {
          console.error(`Error: ${result.error.message}`);
          process.exit(1);
        }
      }
    });
}
