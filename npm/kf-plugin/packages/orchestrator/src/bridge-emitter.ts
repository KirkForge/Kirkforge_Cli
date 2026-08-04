// WO 10.8 / ADR-028 §5: NDJSON bridge emitter for the Rust VerifierBus.
//
// The Rust `TsOrchestratorBridgeVerifier` (src/session/verifier/bus.rs)
// shells out to this script and reads NDJSON verdicts from stdout. Each
// line is a JSON object:
//
//   {"verifier":"security","severity":"error","file":"src/foo.ts",
//    "line":42,"message":"eval() call detected","rule":"no-eval"}
//
// This module runs the orchestrator's `SecurityEmitter` on the changed
// files (passed as argv or via `KF_CHANGED_FILES`), collects the
// findings, and writes them as NDJSON to stdout. It is a thin wrapper
// over the existing emitter — the bridge format is a strict subset of
// the orchestrator's internal event shape.

import { SecurityEmitter } from "./security-emitter.js";
import { EventBus } from "@kirkforge/core-events";
import { readFileSync } from "node:fs";

interface BridgeVerdict {
  verifier: string;
  severity: "error" | "warning" | "info";
  file: string;
  line: number;
  message: string;
  rule: string;
}

// Collect KVB security events from the event bus and translate them to
// the NDJSON bridge format. Returns the verdicts.
async function collectSecurityVerdicts(
  cwd: string,
  files: string[],
): Promise<BridgeVerdict[]> {
  const verdicts: BridgeVerdict[] = [];
  const eventBus = new EventBus();

  eventBus.on("verify.security", async (event: any) => {
    // The SecurityEmitter writes findings to `value.details` (an array
    // of Finding objects). Translate each to the NDJSON bridge format.
    const findings: any[] = event?.value?.details ?? [];
    for (const f of findings) {
      verdicts.push({
        verifier: "security",
        severity: mapSeverity(f.severity),
        file: f.file ?? "",
        line: f.line ?? 0,
        message: f.message ?? "",
        rule: f.rule ?? "",
      });
    }
    return { ok: true, value: undefined };
  });

  const emitter = new SecurityEmitter({ cwd, eventBus, files });
  await emitter.emit("bridge");
  return verdicts;
}

function mapSeverity(
  s: string,
): "error" | "warning" | "info" {
  switch (s) {
    case "critical":
    case "high":
      return "error";
    case "medium":
      return "warning";
    case "low":
      return "info";
    default:
      return "warning";
  }
}

function parseArgs(): { cwd: string; files: string[] } {
  const cwd = process.cwd();
  // Files come from argv (after the script path) or KF_CHANGED_FILES.
  const argvFiles = process.argv.slice(2).filter((a) => !a.startsWith("-"));
  let files = argvFiles;
  if (files.length === 0) {
    const envFiles = process.env.KF_CHANGED_FILES ?? "";
    files = envFiles.split("\n").map((s) => s.trim()).filter((s) => s.length > 0);
  }
  return { cwd, files };
}

// Entry point: run the security emitter, write NDJSON to stdout, exit 0.
async function main() {
  const { cwd, files } = parseArgs();
  const verdicts = await collectSecurityVerdicts(cwd, files);
  for (const v of verdicts) {
    process.stdout.write(JSON.stringify(v) + "\n");
  }
}

main().catch((e) => {
  process.stderr.write(`bridge emitter failed: ${e}\n`);
  process.exit(1);
});