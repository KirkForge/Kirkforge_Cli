import type { Command } from "commander";
import { resolve } from "node:path";
import { existsSync, createReadStream } from "node:fs";
import { createInterface } from "node:readline";
import { chainHashOf, initialHash } from "@kirkforge/core-events";
import { exitError } from "../shared.js";

export function registerAuditVerify(program: Command): void {
  program
    .command("audit-verify")
    .description("Verify the integrity of an audit log chain (checks sequential hashes)")
    .requiredOption("--file <path>", "Path to audit JSONL file")
    .option("--json", "JSON output")
    .action(async (opts) => {
      const filePath = resolve(opts.file);
      if (!existsSync(filePath)) {
        exitError(`Audit file not found: ${filePath}`, opts.json);
      }

      // Re-derive each event's chain hash using the canonical helpers from
      // @kirkforge/core-events, which match what the audit sinks (file, syslog,
      // WORM) actually write. With the canonical helper the CLI correctly
      // detects tampering on any field the chain covers (action, outcome,
      // actor, tenant, reason, timestamp, sequence, metadata).
      const hmacKey = process.env["KIRKFORGE_AUDIT_KEY"];

      let prevHash: string = initialHash(hmacKey);
      let lineCount = 0;
      const errors: string[] = [];
      const actions: Record<string, number> = {};
      const actors: Record<string, number> = {};

      const fileStream = createReadStream(filePath, { encoding: "utf-8" });
      const rl = createInterface({ input: fileStream, crlfDelay: Infinity });

      for await (const line of rl) {
        const trimmed = line.trim();
        if (!trimmed) continue;
        lineCount++;

        let event: Record<string, unknown>;
        try {
          event = JSON.parse(trimmed);
        } catch {
          errors.push(`Line ${lineCount}: Invalid JSON`);
          continue;
        }

        const chainHash = event.chainHash as string | undefined;
        if (!chainHash) {
          errors.push(`Line ${lineCount}: Missing chainHash`);
          continue;
        }

        const expected = chainHashOf(
          prevHash,
          event as unknown as Parameters<typeof chainHashOf>[1],
          hmacKey,
        );

        if (chainHash !== expected) {
          errors.push(
            `Line ${lineCount}: Hash mismatch (expected ${expected}, got ${chainHash}). Chain is broken.`,
          );
          // Stop checking further — chain is broken
          break;
        }

        prevHash = chainHash;

        // Stats
        const a = (event.action as string | undefined) ?? "unknown";
        actions[a] = (actions[a] ?? 0) + 1;
        const act = (event.actorId as string | undefined) ?? "unknown";
        actors[act] = (actors[act] ?? 0) + 1;
      }

      if (opts.json) {
        console.log(
          JSON.stringify(
            {
              valid: errors.length === 0,
              lineCount,
              errors: errors.length > 0 ? errors : undefined,
              actions,
              actors,
            },
            null,
            2,
          ),
        );
      } else {
        if (errors.length === 0) {
          console.log(`✓ Audit chain integrity verified (${lineCount} events)`);
          console.log(`
Event summary:`);
          for (const [action, count] of Object.entries(actions)) {
            console.log(`  ${action}: ${count}`);
          }
          if (Object.keys(actors).length > 0) {
            console.log(`
Actors:`);
            for (const [actor, count] of Object.entries(actors)) {
              console.log(`  ${actor}: ${count}`);
            }
          }
        } else {
          console.error(`✗ Audit chain integrity FAILED`);
          for (const e of errors) {
            console.error(`  ${e}`);
          }
          process.exit(1);
        }
      }
    });
}
