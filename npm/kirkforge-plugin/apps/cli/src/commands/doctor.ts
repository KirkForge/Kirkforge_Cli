import type { Command } from "commander";
import { doctor } from "@kirkforge/plugin";

export function registerDoctor(program: Command): void {
  program
    .command("doctor")
    .description("Probe local verification tools and report capabilities")
    .option("--pretty", "Human-readable output instead of JSON")
    .action(async (opts) => {
      const report = await doctor();

      if (opts.pretty) {
        console.log("\n--- Tool Capability Report ---");
        const tools: [
          string,
          { available: boolean; version?: string; source?: string; note?: string },
        ][] = [
          ["ESLint", report.eslint],
          ["TypeScript (tsc)", report.tsc],
          ["Ruff", report.ruff],
          ["Pyright", report.pyright],
          ["Bandit", report.bandit],
          ["SecDev", report.secdev],
        ];
        for (const [name, cap] of tools) {
          const src = cap.source === "internal" ? " [internal]" : "";
          const status = cap.available
            ? `available (${cap.version ?? "bundled"})${src}`
            : `not found${src}`;
          const note = cap.note ? ` -- ${cap.note}` : "";
          console.log(`  ${name}: ${status}${note}`);
        }
        console.log(`  Languages: ${report.languages.join(", ")}`);
      } else {
        console.log(JSON.stringify(report));
      }
    });
}
