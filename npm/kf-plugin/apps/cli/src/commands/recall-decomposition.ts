import type { Command } from "commander";
import { resolve } from "node:path";
import { FileAdapter, MemoryStore } from "@kirkforge/memory-palace";

export function registerRecallDecomposition(program: Command): void {
  program
    .command("recall-decomposition")
    .description("Recall a previously stored task decomposition")
    .argument("<task-id-or-description>", "Task ID or description substring to search for")
    .option("--json", "JSON output")
    .action(async (query, opts) => {
      const adapter = new FileAdapter(resolve(process.cwd(), ".kirkforge-memory.json"));
      const memoryStore = new MemoryStore(adapter);
      const result = await memoryStore.recallDecomposition(query);

      if (opts.json) {
        if (result.ok && result.value) {
          console.log(JSON.stringify(result.value, null, 2));
        } else if (result.ok) {
          console.log(JSON.stringify({ found: false, query }, null, 2));
        } else {
          console.log(JSON.stringify({ error: result.error.message }, null, 2));
          process.exit(1);
        }
      } else {
        if (result.ok && result.value) {
          const d = result.value;
          console.log(`\nDecomposition for "${d.description}" (stored ${d.timestamp}):`);
          console.log(`${d.tasks.length} subtasks:\n`);
          for (const t of d.tasks) {
            const deps = t.dependsOn.length > 0 ? ` (needs: ${t.dependsOn.join(", ")})` : "";
            console.log(`  [${t.id}] ${t.estimatedComplexity} | ${t.language}${deps}`);
            console.log(`    ${t.description}`);
            if (t.outputFiles.length > 0) console.log(`    → ${t.outputFiles.join(", ")}`);
            if (t.verificationHint) console.log(`    ✓ ${t.verificationHint}`);
          }
        } else if (result.ok) {
          console.log(`No decomposition found for "${query}"`);
        } else {
          console.error(`Error: ${result.error.message}`);
          process.exit(1);
        }
      }
    });
}
