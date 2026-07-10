import { execFile } from "node:child_process";
import { relative, resolve, isAbsolute } from "node:path";
import { ok, err } from "@kirkforge/core-types";
import type { Result } from "@kirkforge/core-types";
import type { EventBus } from "@kirkforge/core-events";
import { walkFiles } from "@kirkforge/core-logging";

interface PyrightOpts {
  cwd: string;
  eventBus?: EventBus;
  files?: string[];
  command?: string;
}

export interface PythonTypesReport {
  taskId: string;
  errors: number;
  durationMs: number;
  details: Array<{ file: string; line: number; code: string; message: string }>;
}

type ExecResult = { stdout: string; stderr: string };

function runTool(cmd: string, args: string[], cwd: string): Promise<ExecResult> {
  return new Promise((resolve, reject) => {
    execFile(
      cmd,
      args,
      { cwd, timeout: 60000, maxBuffer: 10 * 1024 * 1024 },
      (err, stdout, stderr) => {
        const out = stdout?.toString?.() ?? "";
        const errOut = stderr?.toString?.() ?? "";
        if (err && !out && !errOut) reject(err);
        else resolve({ stdout: out, stderr: errOut });
      },
    );
  });
}

function sanitizeFilePath(cwd: string, f: string): string | null {
  const resolved = resolve(cwd, f);
  const rel = relative(cwd, resolved);
  if (rel.startsWith("..") || rel === "" || isAbsolute(rel)) return null;
  return rel;
}

async function discoverPythonFiles(cwd: string, files?: string[]): Promise<string[]> {
  if (files && files.length > 0) {
    return files
      .map((f) => sanitizeFilePath(cwd, f))
      .filter((f): f is string => f !== null)
      .filter((f) => f.endsWith(".py"));
  }
  return walkFiles(cwd, (entry) => entry.endsWith(".py"));
}

function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function isMissingTool(e: unknown, cmd: string): boolean {
  const msg = errorMessage(e);
  return (
    /\b(ENOENT|not found|spawn\b.*\bENOENT)\b/i.test(msg) || msg.includes(`spawn ${cmd} ENOENT`)
  );
}

export class PyrightEmitter {
  constructor(private opts: PyrightOpts) {}

  async emit(taskId: string): Promise<Result<PythonTypesReport, Error>> {
    const start = Date.now();
    const { cwd, eventBus } = this.opts;
    const targets = await discoverPythonFiles(cwd, this.opts.files);

    // No Python files: legitimately nothing to verify. "skipped" is correct.
    if (targets.length === 0) {
      const report: PythonTypesReport = { taskId, errors: 0, durationMs: 0, details: [] };
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

    try {
      const { stdout, stderr } = await runTool(
        this.opts.command ?? "pyright",
        ["--outputjson", ...targets],
        cwd,
      );
      let parsed: {
        summary?: { errorCount?: number };
        diagnostics?: Array<{
          file?: { path?: string };
          range?: { start?: { line?: number } };
          code?: string;
          message?: string;
        }>;
      };
      try {
        parsed = stdout.trim() ? JSON.parse(stdout) : {};
      } catch {
        throw new Error(
          (stderr || stdout || "pyright returned unparseable output").trim().slice(0, 500),
        );
      }

      const errors = parsed.summary?.errorCount ?? 0;
      const details = (parsed.diagnostics ?? []).map((d) => ({
        file: relative(cwd, resolve(cwd, d.file?.path ?? "<pyright>")),
        line: (d.range?.start?.line ?? 0) + 1,
        code: d.code ?? "pyright",
        message: d.message ?? "pyright error",
      }));
      const report: PythonTypesReport = { taskId, errors, durationMs: Date.now() - start, details };
      await eventBus?.emit({
        kind: "verify.types",
        schemaVersion: "v3",
        sequence: 0,
        streamId: taskId,
        taskId,
        value: {
          status: errors > 0 ? "fail" : "pass",
          errors,
          durationMs: report.durationMs,
          details,
        },
        timestamp: new Date().toISOString(),
      });
      return ok(report);
    } catch (e) {
      const message = errorMessage(e);
      if (isMissingTool(e, this.opts.command ?? "pyright")) {
        // FAIL-CLOSED: a missing pyright binary means the verifier did NOT run.
        // Reporting 0 errors here would let an environment without pyright
        // installed pass type-checking on every Python task. Emit status:"error"
        // (counted by the reducer as a verifier failure) and return err(...) so
        // callers know the verifier did not actually run.
        const detailMessage = `pyright binary not found in PATH; type-check did not run. ${message}`;
        const report: PythonTypesReport = {
          taskId,
          errors: 1,
          durationMs: 0,
          details: [{ file: "<pyright>", line: 0, code: "VERIFIER_MISSING_BINARY", message: detailMessage }],
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
      const report: PythonTypesReport = {
        taskId,
        errors: 1,
        durationMs: Date.now() - start,
        details: [{ file: "<pyright>", line: 0, code: "VERIFIER_ERROR", message }],
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
