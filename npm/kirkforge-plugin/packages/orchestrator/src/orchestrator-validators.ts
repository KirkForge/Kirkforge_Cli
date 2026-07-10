import { exec, execFile } from "node:child_process";
import { promisify } from "node:util";
import { relative, resolve, isAbsolute } from "node:path";
import type {
  ValidatorRunConfig,
  LegacyValidatorRunConfig,
  StructuredValidatorConfig,
} from "./types.js";
import type { TaskValidationResult } from "@kirkforge/correction-core";
import { outputSummary } from "./workspace.js";
import { createIsolatedWorkspace } from "./orchestrator-workspace.js";
import type { OrchestratorInternals } from "./orchestrator-shared.js";

export const execAsync = promisify(exec);
export const execFileAsync = promisify(execFile);

export function resolveValidatorShellCommand(
  validator?: ValidatorRunConfig | LegacyValidatorRunConfig | StructuredValidatorConfig,
): string | undefined {
  if (!validator) return undefined;
  if ("shellCommand" in validator && validator.shellCommand) return validator.shellCommand;
  if (
    "command" in validator &&
    !(validator as StructuredValidatorConfig).args &&
    (validator as LegacyValidatorRunConfig).command
  )
    return (validator as LegacyValidatorRunConfig).command;
  return undefined;
}

export function resolveStructuredValidatorConfig(
  validator?: ValidatorRunConfig | LegacyValidatorRunConfig | StructuredValidatorConfig,
): StructuredValidatorConfig | undefined {
  if (!validator) return undefined;
  if (
    "command" in validator &&
    "args" in validator &&
    Array.isArray((validator as StructuredValidatorConfig).args)
  )
    return validator as StructuredValidatorConfig;
  return undefined;
}

/**
 * Run a structured (argv-form) task validator inside an isolated
 * workspace. Returns a structured TaskValidationResult. Rejects cwd
 * values that escape the isolated workspace.
 */
export async function runStructuredTaskValidator(
  s: OrchestratorInternals,
  config: StructuredValidatorConfig,
  emittedFiles?: Array<{ path: string; content?: string }>,
  baselineDir?: string,
): Promise<TaskValidationResult> {
  const started = Date.now();
  const isolatedBase = await createIsolatedWorkspace(
    s,
    emittedFiles,
    baselineDir,
  );
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
 * Run a raw shell task validator. Gated behind ALLOW_UNSAFE_VALIDATOR_SHELL=1
 * to prevent silent host compromise in enterprise deployments.
 */
export async function runTaskValidator(
  s: OrchestratorInternals,
  command: string,
  timeoutMs = 120000,
  emittedFiles?: Array<{ path: string; content?: string }>,
  baselineDir?: string,
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
  const isolatedCwd = await createIsolatedWorkspace(
    s,
    emittedFiles,
    baselineDir,
  );
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
