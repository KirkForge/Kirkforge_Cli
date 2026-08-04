import { cpSync, mkdirSync, rmSync, writeFileSync, copyFileSync } from "node:fs";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, relative, isAbsolute, dirname } from "node:path";
import { exec, execFile } from "node:child_process";
import { promisify } from "node:util";

const execAsync = promisify(exec);
const execFileAsync = promisify(execFile);

// ── Turn copy exclusions ──────────────────────────────────────────────────

const TURN_COPY_EXCLUDED = new Set([
  "node_modules",
  ".git",
  "dist",
  ".tsbuildinfo",
  "tsconfig.tsbuildinfo",
]);

/**
 * Check if a path should be excluded when copying turn/validator workspaces.
 */
export function shouldExcludeFromTurnCopy(src: string, baseLen: number): boolean {
  const rel = src.slice(baseLen + 1);
  if (!rel) return false;
  const segments = rel.split("/");
  if (segments.some((seg) => TURN_COPY_EXCLUDED.has(seg))) return true;
  const last = segments[segments.length - 1];
  if (last && TURN_COPY_EXCLUDED.has(last)) return true;
  return false;
}

// ── Isolated workspace management ──────────────────────────────────────────

export interface IsolatedWorkspaceConfig {
  /** The base cwd to copy from. */
  cwd: string;
  /** Logger for diagnostic messages. */
  logger?: { info: (msg: string) => void; error: (msg: string) => void };
}

/**
 * Manages isolated workspace directories for validator execution.
 * Each workspace is a copy of the project directory with emitted files
 * overlaid on top, ensuring validators don't modify the real project.
 */
export class WorkspaceManager {
  private isolatedDirs: string[] = [];
  private baselineDir: string | null = null;
  private baselineDirs: string[] = [];
  private cwd: string;
  private logger?: { info: (msg: string) => void; error: (msg: string) => void };

  constructor(config: IsolatedWorkspaceConfig) {
    this.cwd = config.cwd;
    this.logger = config.logger;
  }

  /**
   * Create an isolated workspace directory by copying the project cwd
   * and optionally overlaying emitted files.
   */
  async createIsolatedWorkspace(
    emittedFiles?: Array<{ path: string; content?: string }>,
    baselineDir?: string,
  ): Promise<string> {
    try {
      const tmpDir = mkdtempSync(join(tmpdir(), "kirkforge-validator-"));

      if (emittedFiles && emittedFiles.length > 0) {
        const baseline = baselineDir ?? this.cwd;
        cpSync(baseline, tmpDir, {
          recursive: true,
          dereference: false,
          filter: (src: string) => shouldExcludeFromTurnCopy(src, baseline.length),
        });

        for (const f of emittedFiles) {
          const src = resolve(baselineDir ?? this.cwd, f.path);
          const dst = resolve(tmpDir, f.path);
          try {
            mkdirSync(dirname(dst), { recursive: true });
            if (f.content !== undefined) {
              writeFileSync(dst, f.content, "utf-8");
            } else {
              try {
                copyFileSync(src, dst);
              } catch {
                /* file may not exist — skip */
              }
            }
          } catch {
            /* best effort per file */
          }
        }
      } else {
        const baseline = baselineDir ?? this.cwd;
        cpSync(baseline, tmpDir, {
          recursive: true,
          dereference: false,
          filter: (src: string) => shouldExcludeFromTurnCopy(src, baseline.length),
        });
      }

      this.isolatedDirs.push(tmpDir);
      return tmpDir;
    } catch (e) {
      this.logger?.error(
        `[workspace] Failed to create isolated workspace: ${e instanceof Error ? e.message : String(e)}`,
      );
      throw e instanceof Error ? e : new Error(String(e));
    }
  }

  /** Clean up all isolated workspace directories. */
  cleanupAll(): void {
    for (const dir of this.isolatedDirs) {
      try {
        rmSync(dir, { recursive: true, force: true });
      } catch {
        /* best effort */
      }
    }
    this.isolatedDirs = [];
  }

  /**
   * Ensure a baseline snapshot exists (copy of cwd for consistent validators).
   * Returns the snapshot directory path.
   */
  ensureBaselineSnapshot(): string {
    if (this.baselineDir) return this.baselineDir;
    const snapshotDir = mkdtempSync(join(tmpdir(), "kirkforge-baseline-"));
    cpSync(this.cwd, snapshotDir, {
      recursive: true,
      dereference: false,
      filter: (src: string) => {
        try {
          return shouldExcludeFromTurnCopy(src, this.cwd.length);
        } catch {
          return true;
        }
      },
    });
    this.baselineDir = snapshotDir;
    this.baselineDirs.push(snapshotDir);
    this.logger?.info(`[workspace] Baseline snapshot created at ${snapshotDir}`);
    return snapshotDir;
  }

  /** Clean up baseline snapshot directories. */
  cleanupBaselines(): void {
    for (const dir of this.baselineDirs) {
      try {
        rmSync(dir, { recursive: true, force: true });
      } catch {
        /* best effort */
      }
    }
    this.baselineDirs = [];
    this.baselineDir = null;
  }
}

// ── Task validator execution ───────────────────────────────────────────────

export interface TaskValidationResult {
  status: "pass" | "fail" | "error" | "skipped";
  validator: string;
  reason: string;
  durationMs: number;
  details: Record<string, unknown>;
}

/**
 * Run a structured validator command in an isolated workspace.
 * The command and args are passed separately (no shell expansion).
 */
export async function runStructuredValidator(
  config: { command: string; args: string[]; cwd?: string; timeoutMs?: number },
  isolatedBase: string,
): Promise<TaskValidationResult> {
  const started = Date.now();
  const cwd = config.cwd ?? isolatedBase;
  if (config.cwd) {
    const rel = relative(isolatedBase, resolve(config.cwd));
    if (rel === "" || rel.startsWith("..") || isAbsolute(rel)) {
      return {
        status: "error",
        validator: `${config.command} ${config.args.join(" ")}`,
        reason: `validator cwd (${config.cwd}) escapes isolated workspace (${isolatedBase})`,
        durationMs: Date.now() - started,
        details: {},
      };
    }
  }
  const timeoutMs = config.timeoutMs ?? 120000;
  try {
    const { stdout, stderr } = await execFileAsync(config.command, config.args, {
      cwd,
      timeout: timeoutMs,
      maxBuffer: 1024 * 1024 * 10,
    });
    const output = `${stdout}${stderr ? `\n${stderr}` : ""}`.trim();
    return {
      status: "pass",
      validator: `${config.command} ${config.args.join(" ")}`,
      reason: outputSummary(output) || "validator exited 0",
      durationMs: Date.now() - started,
      details: { exitCode: 0, stdout: stdout.slice(-8000), stderr: stderr.slice(-8000) },
    };
  } catch (cause) {
    const errObj = cause as {
      code?: unknown;
      signal?: unknown;
      stdout?: string;
      stderr?: string;
      killed?: boolean;
      message?: string;
    };
    const stdout = errObj.stdout ?? "";
    const stderr = errObj.stderr ?? "";
    const output = `${stdout}${stderr ? `\n${stderr}` : ""}`.trim();
    const timedOut = errObj.killed === true || errObj.signal === "SIGTERM";
    return {
      status: timedOut ? "error" : "fail",
      validator: `${config.command} ${config.args.join(" ")}`,
      reason:
        outputSummary(output) ||
        errObj.message ||
        (timedOut ? "validator timed out" : "validator exited non-zero"),
      durationMs: Date.now() - started,
      details: {
        exitCode: errObj.code ?? null,
        signal: errObj.signal ?? null,
        stdout: stdout.slice(-8000),
        stderr: stderr.slice(-8000),
      },
    };
  }
}

/**
 * Run a shell validator command in an isolated workspace.
 * Requires ALLOW_UNSAFE_VALIDATOR_SHELL=1 environment variable.
 */
export async function runShellValidator(
  command: string,
  isolatedCwd: string,
  timeoutMs: number = 120000,
): Promise<TaskValidationResult> {
  if (process.env.ALLOW_UNSAFE_VALIDATOR_SHELL !== "1") {
    return {
      status: "error",
      validator: command,
      reason:
        "validator-shell is disabled: set ALLOW_UNSAFE_VALIDATOR_SHELL=1 to enable raw shell validators",
      durationMs: 0,
      details: {},
    };
  }
  const started = Date.now();
  try {
    const { stdout, stderr } = await execAsync(command, {
      cwd: isolatedCwd,
      timeout: timeoutMs,
      maxBuffer: 1024 * 1024 * 10,
    });
    const output = `${stdout}${stderr ? `\n${stderr}` : ""}`.trim();
    return {
      status: "pass",
      validator: command,
      reason: outputSummary(output) || "validator exited 0",
      durationMs: Date.now() - started,
      details: { exitCode: 0, stdout: stdout.slice(-8000), stderr: stderr.slice(-8000) },
    };
  } catch (cause) {
    const errObj = cause as {
      code?: unknown;
      signal?: unknown;
      stdout?: string;
      stderr?: string;
      killed?: boolean;
      message?: string;
    };
    const stdout = errObj.stdout ?? "";
    const stderr = errObj.stderr ?? "";
    const output = `${stdout}${stderr ? `\n${stderr}` : ""}`.trim();
    const timedOut = errObj.killed === true || errObj.signal === "SIGTERM";
    return {
      status: timedOut ? "error" : "fail",
      validator: command,
      reason:
        outputSummary(output) ||
        errObj.message ||
        (timedOut ? "validator timed out" : "validator exited non-zero"),
      durationMs: Date.now() - started,
      details: {
        exitCode: errObj.code ?? null,
        signal: errObj.signal ?? null,
        stdout: stdout.slice(-8000),
        stderr: stderr.slice(-8000),
      },
    };
  }
}

// ── Utility ──────────────────────────────────────────────────────────────────

export function outputSummary(output: string): string | undefined {
  const firstLines = output
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .slice(0, 8)
    .join("\n");
  return firstLines ? firstLines.slice(0, 2000) : undefined;
}
