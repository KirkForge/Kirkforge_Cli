import type { Command } from "commander";
import { createBootstrap } from "../bootstrap.js";

export function registerVerify(program: Command): void {
  program
    .command("verify")
    .description("Run deterministic verification emitters without calling a model")
    .option("--task <description>", "Task description used only for verifier language routing")
    .option("--json", "JSON output")
    .action(async (opts) => {
      const {
        orchestrator,
        shutdown: _shutdown,
        policyEngine: _policyEngine,
        auditLogger: _auditLogger,
      } = await createBootstrap(opts);
      const packet = await orchestrator.verify({ description: opts.task });

      if (opts.json) {
        console.log(JSON.stringify(packet, null, 2));
      } else {
        console.log(`\n--- Verification Report ---`);
        console.log(`  Lint errors:    ${packet.verification.lint.errors}`);
        console.log(`  Lint warnings:  ${packet.verification.lint.warnings}`);
        if (packet.verification.lint.suppressed) {
          console.log(`  Lint suppressed: ${packet.verification.lint.suppressed}`);
        }
        console.log(`  Type errors:    ${packet.verification.types.errors}`);
        console.log(
          `  Security:       ${packet.verification.security.findings} findings (${packet.verification.security.critical} critical, ${packet.verification.security.high} high)`,
        );
        console.log(`  Files changed:  ${packet.changes.filesChanged}`);
        console.log(
          `  Graph:          ${packet.graph.edgeCount} edges (${packet.graph.brokenEdges} broken, ${packet.graph.cycles} cycles)`,
        );
        console.log(`  Overall:        ${packet.verification.overall.toUpperCase()}`);
      }
    });
}
