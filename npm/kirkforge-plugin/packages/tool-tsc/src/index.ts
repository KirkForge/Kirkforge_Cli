import { ok, err } from "@kirkforge/core-types";
import type { Result } from "@kirkforge/core-types";
import type { EventBus } from "@kirkforge/core-events";
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";

export interface TscReport {
  taskId: string;
  errors: number;
  durationMs: number;
  details: Array<{ file: string; line: number; code: string; message: string }>;
}

export class TscEmitter {
  constructor(
    private opts: { cwd: string; eventBus?: EventBus; tsconfigPath?: string; files?: string[] },
  ) {}

  async emit(taskId: string): Promise<Result<TscReport, Error>> {
    const start = Date.now();
    const { cwd, eventBus } = this.opts;
    const tsconfigPath = resolve(cwd, this.opts.tsconfigPath ?? "tsconfig.json");

    // No tsconfig.json: legitimately nothing to verify in this project. "skipped"
    // is correct because there are no TypeScript files to type-check. The reducer
    // (StateReducer.reduce) handles the case where the slot is policy.required.
    if (!existsSync(tsconfigPath)) {
      const report: TscReport = { taskId, errors: 0, durationMs: 0, details: [] };
      await eventBus?.emit({
        kind: "verify.types",
        schemaVersion: "v3",
        sequence: 0,
        streamId: taskId,
        taskId,
        value: { status: "skipped", errors: 0, durationMs: 0, details: [] },
        timestamp: new Date().toISOString(),
      });
      return ok(report);
    }

    const execAsync = (cmd: string, args: string[]) =>
      new Promise<{ stdout: string; stderr: string }>((resolve, reject) => {
        execFile(
          cmd,
          args,
          { cwd, timeout: 30000, maxBuffer: 10 * 1024 * 1024 },
          (err, stdout, stderr) => {
            if (err && !stdout && !stderr) reject(err);
            else
              resolve({ stdout: stdout?.toString?.() ?? "", stderr: stderr?.toString?.() ?? "" });
          },
        );
      });

    try {
      const { stdout, stderr } = await execAsync("npx", [
        "tsc",
        "--noEmit",
        "--project",
        tsconfigPath,
      ]);
      const output = stdout + stderr;
      const details: TscReport["details"] = [];
      for (const line of output.split("\n")) {
        const m = line.match(/(.+?)\((\d+),\d+\):\s*error\s+TS(\d+):\s*(.+)/);
        if (m) {
          details.push({
            file: m[1]!.replace(cwd, "").replace(/^\//, ""),
            line: parseInt(m[2]!),
            code: `TS${m[3]}`,
            message: m[4]!,
          });
        }
      }

      const outputHadErrors = /\berror\s+TS\d+:/.test(output);
      if (outputHadErrors && details.length === 0) {
        details.push({
          file: "<tsc>",
          line: 0,
          code: "TS_UNKNOWN",
          message: output.trim().slice(0, 500) || "tsc failed with unparsed output",
        });
      }
      const report: TscReport = {
        taskId,
        errors: details.length,
        durationMs: Date.now() - start,
        details,
      };
      const status = details.length > 0 ? "fail" : "pass";
      await eventBus?.emit({
        kind: "verify.types",
        schemaVersion: "v3",
        sequence: 0,
        streamId: taskId,
        taskId,
        value: { status, errors: details.length, durationMs: report.durationMs, details },
        timestamp: new Date().toISOString(),
      });
      return ok(report);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      if (/ENOENT/.test(message)) {
        // FAIL-CLOSED: a missing `tsc` binary means the verifier did NOT run.
        // Reporting 0 errors here would let an environment without tsc installed
        // pass type-checking on every task — undermining the core "deterministic
        // verification" claim. Emit status:"error" (counted by the reducer as
        // a verifier failure) and return err(...) so callers know the verifier
        // did not actually run.
        const detailMessage = `tsc binary not found in PATH; type-check did not run. ${message}`;
        const report: TscReport = {
          taskId,
          errors: 1,
          durationMs: 0,
          details: [{ file: "<tsc>", line: 0, code: "VERIFIER_MISSING_BINARY", message: detailMessage }],
        };
        await eventBus?.emit({
          kind: "verify.types",
          schemaVersion: "v3",
          sequence: 0,
          streamId: taskId,
          taskId,
          value: {
            status: "error",
            error: detailMessage,
            errors: 1,
            durationMs: 0,
            details: report.details,
          },
          timestamp: new Date().toISOString(),
        });
        return err(new Error(detailMessage));
      }
      const report: TscReport = {
        taskId,
        errors: 1,
        durationMs: Date.now() - start,
        details: [{ file: "<tsc>", line: 0, code: "VERIFIER_ERROR", message }],
      };
      await eventBus?.emit({
        kind: "verify.types",
        schemaVersion: "v3",
        sequence: 0,
        streamId: taskId,
        taskId,
        value: {
          status: "error",
          error: message,
          errors: 1,
          durationMs: report.durationMs,
          details: report.details,
        },
        timestamp: new Date().toISOString(),
      });
      return ok(report);
    }
  }
}
