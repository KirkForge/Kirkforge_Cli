import { describe, it, expect } from "vitest";
import { EventBus } from "@kirkforge/core-events";
import { SecurityEmitter } from "../src/security-emitter.js";
import { mkdtempSync, writeFileSync, mkdirSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

// WO 10.8 / ADR-028 §5: the bridge emitter wraps the orchestrator's
// SecurityEmitter and outputs NDJSON verdicts the Rust
// TsOrchestratorBridgeVerifier can parse. This test verifies the
// event-to-verdict translation by replaying the event-bus shape the
// bridge emitter uses.

describe("TsOrchestratorBridgeEmitter (WO 10.8)", () => {
  it("translates security emitter findings to NDJSON bridge verdicts", async () => {
    // Create a temp file with an obfuscated eval pattern the
    // SecurityEmitter detects.
    const dir = mkdtempSync(join(tmpdir(), "kf-bridge-"));
    const subdir = join(dir, "src");
    mkdirSync(subdir);
    const file = join(subdir, "evil.ts");
    writeFileSync(file, "const f = window['eval']('1+1');\n");

    const eventBus = new EventBus();
    const verdicts: any[] = [];

    // The bridge emitter listens for verify.security events and
    // translates them to the NDJSON wire format. The SecurityEmitter
    // puts findings in `value.details` (an array of Finding objects).
    eventBus.on("verify.security", async (event: any) => {
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

    const emitter = new SecurityEmitter({ cwd: dir, eventBus, files: ["src/evil.ts"] });
    await emitter.emit("bridge-test");

    expect(verdicts.length).toBeGreaterThan(0);
    const v = verdicts[0]!;
    expect(v.verifier).toBe("security");
    expect(v.severity).toBe("error");
    expect(v.rule).toContain("bracket-eval");
    expect(v.message.length).toBeGreaterThan(0);
    // The verdict must be JSON-serializable (the bridge writes NDJSON).
    expect(() => JSON.stringify(v)).not.toThrow();
  });

  it("produces zero verdicts for a clean file", async () => {
    const dir = mkdtempSync(join(tmpdir(), "kf-bridge-clean-"));
    const subdir = join(dir, "src");
    mkdirSync(subdir);
    const file = join(subdir, "clean.ts");
    writeFileSync(file, "const x = 1 + 2;\nexport { x };\n");

    const eventBus = new EventBus();
    const verdicts: any[] = [];
    eventBus.on("verify.security", async (event: any) => {
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

    const emitter = new SecurityEmitter({ cwd: dir, eventBus, files: ["src/clean.ts"] });
    await emitter.emit("bridge-clean");

    expect(verdicts.length).toBe(0);
  });
});

function mapSeverity(s: string): "error" | "warning" | "info" {
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