import type { Command } from "commander";
import { createBootstrap } from "../bootstrap.js";

export function registerDecompose(program: Command): void {
  program
    .command("decompose")
    .description("Break a complex task into smaller, independently verifiable subtasks")
    .argument("<description>", "Task description to decompose")
    .option("--provider <key>", "Provider key from config (e.g. openai, local-ollama)")
    .option("--json", "JSON output")
    .option("--execute", "Execute the decomposed subtasks in dependency order")
    .action(async (description, opts) => {
      const {
        orchestrator,
        shutdown: _shutdown,
        policyEngine: _policyEngine,
        auditLogger: _auditLogger,
      } = await createBootstrap(opts);
      const result = await orchestrator.decomposeTask({ description });

      if (opts.json) {
        if (result.ok) {
          console.log(JSON.stringify(result.value, null, 2));
        } else {
          console.log(JSON.stringify({ error: result.error.message }, null, 2));
          process.exit(1);
        }
      } else {
        if (result.ok) {
          const d = result.value;
          console.log(`\nDecomposed "${d.rootTask}" into ${d.tasks.length} subtasks:`);
          console.log(`Rationale: ${d.rationale}`);
          console.log(`Estimated tokens: ~${d.totalEstimatedTokens}\n`);
          for (const t of d.tasks) {
            const deps = t.dependsOn.length > 0 ? ` (needs: ${t.dependsOn.join(", ")})` : "";
            console.log(`  [${t.id}] ${t.estimatedComplexity} | ${t.language}${deps}`);
            console.log(`    ${t.description}`);
            if (t.outputFiles.length > 0) console.log(`    → ${t.outputFiles.join(", ")}`);
            if (t.verificationHint) console.log(`    ✓ ${t.verificationHint}`);
          }
        } else {
          console.error(`Error: ${result.error.message}`);
          process.exit(1);
        }
      }

      if (opts.execute) {
        const taskId = "decomp-" + description.slice(0, 40).replace(/[^a-zA-Z0-9]/g, "-");
        console.log("\nExecuting decomposition...\n");
        const execResult = await orchestrator.executeDecomposition(taskId);
        if (execResult.ok) {
          const er = execResult.value;
          if (opts.json) {
            console.log(JSON.stringify(er, null, 2));
          } else {
            console.log(
              "Execution complete: " +
                er.succeededCount +
                "/" +
                er.totalSubtasks +
                " subtasks succeeded",
            );
            console.log(
              "Total tokens: " +
                er.totalTokens +
                " | Duration: " +
                (er.totalDurationMs / 1000).toFixed(1) +
                "s\n",
            );
            for (const r of er.results) {
              const status = r.ok ? "✓" : "✗";
              console.log(
                "  " +
                  status +
                  " [" +
                  r.nodeId +
                  "] " +
                  r.description.slice(0, 60) +
                  " (" +
                  (r.durationMs / 1000).toFixed(1) +
                  "s, " +
                  r.tokensUsed +
                  " tokens)",
              );
              if (r.error) console.log("      Error: " + r.error);
              if (r.files && r.files.length > 0) console.log("      Files: " + r.files.join(", "));
            }
          }
        } else {
          console.error("Execution failed: " + execResult.error.message);
          process.exit(1);
        }
      }
    });
}
