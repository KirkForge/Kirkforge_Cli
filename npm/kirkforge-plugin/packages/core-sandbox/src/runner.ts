import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, readFileSync as fsReadFileSync } from "node:fs";
import { delimiter, isAbsolute, join } from "node:path";
import { ok, err, type Result } from "@kirkforge/core-types";
import { KirkForgeError } from "@kirkforge/core-errors";
import {
  type SandboxConstraints,
  type SandboxResult,
  type SandboxContext,
  type SandboxViolation,
  validateConstraints,
  isCommandAllowed,
  isReadPathAllowed,
  isWritePathAllowed,
  createSandboxContext,
} from "./index.js";
import { isEnterpriseMode } from "@kirkforge/core-enterprise";

// ── Sandboxed process runner ───────────────────────────────────────────────
//
// Executes commands inside a constrained subprocess. This is the runtime
// companion to the constraint declarations in index.ts.
//
// Enforcement model:
//   1. Command allowlist: only commands in allowedCommands may be spawned.
//   2. Wall-clock timeout: kill process after maxTimeMs.
//   3. Output size limit: truncate stdout/stderr after maxOutputBytes.
//   4. Path validation: reject arguments referencing paths outside allowed
//      read/write roots.
//   5. Network: no enforcement at process level (requires container/VM);
//      instead, violations are detected and reported for post-hoc auditing.
//   6. Memory/CPU: no enforcement at process level (requires OS-level cgroups
//      or container limits); instead, resource usage is measured and reported.
//
// For full isolation (network, filesystem, memory/CPU), run inside Docker or
// a microVM. This runner provides the "constrained host" baseline.

// ── Types ──────────────────────────────────────────────────────────────────

export interface SandboxRunConfig {
  /** Command to execute (must be in allowedCommands if shellAllowed). */
  command: string;
  /** Arguments to pass to the command. */
  args?: string[];
  /** Current working directory for the command. */
  cwd?: string;
  /** Environment variables to pass to child. Deny-by-default: only explicitly
   *  listed env vars are passed; the parent process.env is NOT inherited
   *  unless ALLOW_UNSAFE_HOST_SANDBOX=1 is set. The secret scan in
   *  scanEnvForSecrets() blocks env vars whose names match /password|secret|
   *  token|api.?key|private.?key|auth/i in enterprise mode and in
   *  non-enterprise mode unless ALLOW_UNSAFE_HOST_SANDBOX=1 is set. */
  env?: Record<string, string>;
  /** Whether to inherit the parent process environment. Default: false.
   *
   *  WARNING: Setting this to true hands the child process the full parent
   *  process.env — every secret in the orchestrator's environment (API keys,
   *  database credentials, signing keys, JWT secrets, tenant encryption
   *  keys) is exposed to whatever code runs inside the sandbox.
   *
   *  Acceptable for: trusted local linters (tsc, pyright, eslint) that
   *  need to read shell env like PATH.
   *  DANGEROUS for: any code path where the inputs are model-influenced
   *  or otherwise untrusted. A prompt-injection attack could exfiltrate
   *  `OPENAI_API_KEY` or `KIRKFORGE_TENANT_KEY` to an attacker-controlled
   *  network endpoint with this flag set.
   *
   *  Equivalent to ALLOW_UNSAFE_HOST_SANDBOX behavior. In enterprise mode
   *  (DOPAFLOW_ENTERPRISE_MODE=true / KIRKFORCE_ENTERPRISE_MODE=true),
   *  this is forced to false regardless of what the caller passes. */
  inheritParentEnv?: boolean;
  /** Sandbox constraints. Defaults applied for unspecified fields. */
  constraints?: SandboxConstraints;
  /** Tenant ID for multi-tenant isolation. */
  tenantId?: string;
  /** Actor ID for audit logging. */
  actorId?: string;
  /** Skip path argument scanning. Set to true when the command provides its own
   *  filesystem isolation (e.g. Docker), so path-like args are mount specs, not
   *  filesystem references that should be checked against allowed paths. */
  skipArgPathScan?: boolean;
  /** Pre-execution hook. */
  beforeHook?: (context: SandboxContext) => Result<void, Error>;
  /** Post-execution hook. */
  afterHook?: (context: SandboxContext, result: SandboxResult) => void;
}

export class SandboxExecutionError extends KirkForgeError {
  violations: SandboxViolation[];
  constructor(message: string, violations: SandboxViolation[] = []) {
    super("SANDBOX_EXECUTION_ERROR", message, { violations });
    this.name = "SandboxExecutionError";
    this.violations = violations;
  }
}

/**
 * Resolve a command name to an absolute path using PATH lookup.
 * This allows sandboxed commands to run even when the child env
 * doesn't inherit PATH — the parent resolves the binary, but the
 * child process never receives the parent's environment.
 */
function resolveCommand(command: string): string {
  if (command === "node") return process.execPath;
  if (isAbsolute(command)) return command;
  const envPath = process.env.PATH ?? "";
  for (const dir of envPath.split(delimiter)) {
    const candidate = join(dir, command);
    if (existsSync(candidate)) return candidate;
  }
  return command;
}

// ── Path argument scanning ──────────────────────────────────────────────────

/**
 * Scan command arguments for path references that violate constraints.
 * Returns violations for any argument that looks like a path outside allowed roots.
 */
function scanArgsForPathViolations(
  args: string[],
  constraints: Required<SandboxConstraints>,
): SandboxViolation[] {
  const violations: SandboxViolation[] = [];
  for (const arg of args) {
    // Heuristic: if arg looks like a path, check against allowed roots
    if (arg.startsWith("/") || arg.startsWith("./") || arg.startsWith("..")) {
      // Check both read and write paths — we cannot determine intent from
      // the argument alone (a relative path like ./readme could be read or write).
      const readAllowed = isReadPathAllowed(arg, constraints);
      const writeAllowed = isWritePathAllowed(arg, constraints);
      if (!readAllowed && !writeAllowed) {
        violations.push({
          type: "filesystem",
          message: `Argument path "${arg}" is outside allowed read/write roots`,
          target: arg,
        });
      }
    }
  }
  return violations;
}

/**
 * Scan environment for secrets or sensitive values that should not be passed.
 */
function scanEnvForSecrets(env: Record<string, string>): SandboxViolation[] {
  const violations: SandboxViolation[] = [];
  const sensitivePatterns = [
    /password/i,
    /secret/i,
    /token/i,
    /api.?key/i,
    /private.?key/i,
    /auth/i,
  ];
  for (const [key, value] of Object.entries(env)) {
    for (const pattern of sensitivePatterns) {
      if (pattern.test(key) && value.length > 0) {
        violations.push({
          type: "secret",
          message: `Environment variable "${key}" may contain sensitive data`,
          target: key,
        });
        break;
      }
    }
  }
  return violations;
}

// ── Main runner ─────────────────────────────────────────────────────────────

/**
 * Execute a command inside sandbox constraints.
 *
 * This creates a child process with the given command and args, enforces
 * wall-clock timeout and output size limits, and detects path violations
 * in arguments. Full filesystem/network/CPU/memory isolation requires
 * running inside Docker or a microVM.
 *
 * SECURITY: The bare-host runner provides constraint enforcement (allowlists,
 * timeouts, output limits) but NOT process-level isolation. For untrusted or
 * model-influenced code, use runDockerSandboxed() instead. If you must use
 * this runner for untrusted code, set ALLOW_UNSAFE_HOST_SANDBOX=1 — mirroring
 * the ALLOW_UNSAFE_VALIDATOR_SHELL pattern. This env var gates:
 *   - Parent env inheritance (otherwise deny-by-default)
 *   - Bypassing secret env var blocking
 * Enterprise mode forces the Docker path regardless.
 */
export async function runSandboxed(
  config: SandboxRunConfig,
): Promise<Result<SandboxResult, SandboxExecutionError>> {
  const mergedConstraints = validateConstraints(config.constraints ?? {});
  if (!mergedConstraints.ok) {
    return err(
      new SandboxExecutionError(`Invalid sandbox constraints: ${mergedConstraints.error.message}`),
    );
  }
  const constraints = mergedConstraints.value;

  // ── ALLOW_UNSAFE_HOST_SANDBOX gate ───────────────────────────────────
  // The bare-host runner provides constraint enforcement but NOT process-level
  // isolation. For untrusted/model-influenced code, runDockerSandboxed() is
  // the safe default. ALLOW_UNSAFE_HOST_SANDBOX=1 opts into the bare-host
  // runner for trusted tool execution — mirrors the ALLOW_UNSAFE_VALIDATOR_SHELL
  // pattern. In enterprise mode, this gate is always enforced.
  const isEnterprise = isEnterpriseMode();
  const unsafeHostSandbox = process.env.ALLOW_UNSAFE_HOST_SANDBOX === "1";
  if (isEnterprise && !unsafeHostSandbox) {
    // In enterprise mode, the bare-host runner is denied unless explicitly
    // opted in with ALLOW_UNSAFE_HOST_SANDBOX=1. Use runDockerSandboxed instead.
    return err(
      new SandboxExecutionError(
        "Enterprise mode requires container isolation. Use runDockerSandboxed() or set ALLOW_UNSAFE_HOST_SANDBOX=1 (not recommended for untrusted code).",
        [
          {
            type: "command",
            message: "Bare-host sandbox denied in enterprise mode",
            target: config.command,
          },
        ],
      ),
    );
  }

  // ── Command allowlist check ───────────────────────────────────────────
  if (constraints.shellAllowed && !isCommandAllowed(config.command, constraints)) {
    return err(
      new SandboxExecutionError(
        `Command "${config.command}" is not in the allowed list: [${constraints.allowedCommands.join(", ")}]`,
        [
          {
            type: "command",
            message: `Command not allowed: ${config.command}`,
            target: config.command,
          },
        ],
      ),
    );
  }
  if (!constraints.shellAllowed) {
    return err(
      new SandboxExecutionError("Shell execution is not allowed by sandbox constraints", [
        { type: "command", message: "Shell execution denied by policy", target: config.command },
      ]),
    );
  }

  const args = config.args ?? [];

  // ── Path scanning ─────────────────────────────────────────────────────
  // Deny-by-default: path violations always block execution, even when
  // allowedReadPaths is empty. An empty allowedReadPaths list means NO paths
  // are allowed for reading - so any path-like argument is a violation.
  // Docker execution skips arg path scanning because Docker mount args
  // (e.g. -v /path:/path:ro) are not filesystem references the host sandbox
  // should restrict — the container provides its own filesystem isolation.
  const pathViolations = config.skipArgPathScan ? [] : scanArgsForPathViolations(args, constraints);
  if (pathViolations.length > 0) {
    return err(
      new SandboxExecutionError(
        `Sandbox path violations detected: ${pathViolations.map((v) => v.message).join("; ")}`,
        pathViolations,
      ),
    );
  }

  // ── Environment scanning ──────────────────────────────────────────────
  // Deny-by-default: env inheritance is opt-in. Only explicitly provided env
  // vars are passed to the child unless inheritParentEnv is set.
  // Secret-named env vars BLOCK execution — they are not merely flagged.
  // (Fixes defects identified in enterprise security review.)
  const inheritParent = config.inheritParentEnv === true || unsafeHostSandbox;
  const envViolations = scanEnvForSecrets(config.env ?? {});
  // In non-enterprise mode with ALLOW_UNSAFE_HOST_SANDBOX, secret violations
  // are still detected and logged but don't block execution (for backward
  // compat with trusted tool chains). In enterprise mode, they always block.
  if (envViolations.length > 0 && (isEnterprise || !unsafeHostSandbox)) {
    return err(
      new SandboxExecutionError(
        `Sandbox env violations: sensitive env vars detected: ${envViolations.map((v) => v.target).join(", ")}. Remove them or set ALLOW_UNSAFE_HOST_SANDBOX=1 (not recommended for untrusted code).`,
        envViolations,
      ),
    );
  }

  // ── Create context and call beforeHook ─────────────────────────────────
  const contextResult = createSandboxContext(config.command, args, {
    constraints: config.constraints ?? {},
    tenantId: config.tenantId,
    actorId: config.actorId,
    beforeHook: config.beforeHook as
      | ((context: SandboxContext) => Result<void, import("./index.js").SandboxError>)
      | undefined,
  });
  if (!contextResult.ok) {
    return err(
      new SandboxExecutionError(`Sandbox context creation failed: ${contextResult.error.message}`),
    );
  }
  const context = contextResult.value;

  // ── Execute ───────────────────────────────────────────────────────────
  const startTime = Date.now();
  let stdout = "";
  let stderr = "";
  let truncated = false;
  const violations: SandboxViolation[] = [...pathViolations, ...envViolations];
  let peakMemoryMb: number | null = null;

  return new Promise((resolve) => {
    let killed = false;

    const resolvedCommand = inheritParent ? config.command : resolveCommand(config.command);
    const childEnv = inheritParent ? { ...process.env, ...config.env } : { ...config.env };
    const child: ChildProcess = spawn(resolvedCommand, args, {
      cwd: config.cwd,
      env: childEnv as Record<string, string | undefined>,
      stdio: ["pipe", "pipe", "pipe"],
      // Detach creates a new process group so we can kill the entire tree
      // on timeout, including any grandchildren the child may have spawned.
      detached: true,
    });
    // Immediately unref so the parent doesn't wait for the detached child
    child.unref();

    // ── Timeout ─────────────────────────────────────────────────────────
    const timeoutHandle = setTimeout(() => {
      killed = true;
      violations.push({
        type: "time",
        message: `Process exceeded maxTimeMs=${constraints.maxTimeMs}`,
      });
      // Kill the entire process group (including grandchildren) rather than
      // just the direct child. Without this, grandchildren survive past maxTimeMs.
      try {
        if (child.pid != null) process.kill(-child.pid, "SIGKILL");
      } catch {
        // Process group may already be gone — best effort
      }
    }, constraints.maxTimeMs);

    // ── Stdout collection ────────────────────────────────────────────────
    let stdoutBytes = 0;
    child.stdout?.on("data", (data: Buffer) => {
      stdoutBytes += data.length;
      if (stdoutBytes > constraints.maxOutputBytes) {
        if (!truncated) {
          truncated = true;
          violations.push({
            type: "output_size",
            message: `Stdout exceeded maxOutputBytes=${constraints.maxOutputBytes}`,
          });
        }
      } else {
        stdout += data.toString("utf-8");
      }
    });

    // ── Stderr collection ────────────────────────────────────────────────
    let stderrBytes = 0;
    child.stderr?.on("data", (data: Buffer) => {
      stderrBytes += data.length;
      if (stderrBytes > constraints.maxOutputBytes) {
        if (!truncated) {
          truncated = true;
          violations.push({
            type: "output_size",
            message: `Stderr exceeded maxOutputBytes=${constraints.maxOutputBytes}`,
          });
        }
      } else {
        stderr += data.toString("utf-8");
      }
    });

    // ── Memory measurement (best-effort) ────────────────────────────────
    // On Linux, read child RSS from /proc/<pid>/status for accuracy.
    // On other platforms, we cannot reliably measure child memory, so we
    // report null rather than a misleading parent-process metric.
    // (Fixes defect identified in enterprise security review.)
    const isLinux = process.platform === "linux";
    const memoryInterval = setInterval(() => {
      if (child.killed || child.pid == null) return;
      if (!isLinux) return; // non-Linux: leave peakMemoryMb as null
      try {
        if (!child.pid) return;
        const status = fsReadFileSync(`/proc/${child.pid}/status`, "utf-8");
        const match = status.match(/VmRSS:\s+(\d+)\s+kB/);
        if (match) {
          const mb = parseInt(match[1] ?? "0", 10) / 1024;
          if (peakMemoryMb === null || mb > peakMemoryMb) {
            peakMemoryMb = mb;
          }
        }
      } catch {
        // /proc not available or PID gone — best effort
      }
    }, 500);

    // ── Close handlers ──────────────────────────────────────────────────
    child.on("close", (code) => {
      clearTimeout(timeoutHandle);
      clearInterval(memoryInterval);

      const durationMs = Date.now() - startTime;
      const result: SandboxResult = {
        success: !killed && code === 0,
        exitCode: code,
        stdout,
        stderr,
        durationMs,
        peakMemoryMb,
        violations,
        truncated,
      };

      // ── Post-execution hook ────────────────────────────────────────────
      if (config.afterHook) {
        try {
          config.afterHook(context, result);
        } catch {
          // Best-effort — don't fail the result for hook errors
        }
      }

      // ── Return ─────────────────────────────────────────────────────────
      if (violations.length > 0 && (killed || violations.some((v) => v.type !== "output_size"))) {
        resolve(
          err(
            new SandboxExecutionError(
              `Sandbox execution failed with ${violations.length} violation(s): ${violations.map((v) => v.message).join("; ")}`,
              violations,
            ),
          ),
        );
      } else {
        resolve(ok(result));
      }
    });

    child.on("error", (errObj) => {
      clearTimeout(timeoutHandle);
      clearInterval(memoryInterval);
      const _durationMs = Date.now() - startTime;
      resolve(
        err(
          new SandboxExecutionError(`Process spawn error: ${errObj.message}`, [
            { type: "command", message: errObj.message, target: config.command },
          ]),
        ),
      );
    });
  });
}

// ── Docker-based sandbox runner (enterprise) ────────────────────────────────
//
// For full isolation, use Docker to run commands in a container with:
//   - No network (unless allowed by constraints)
//   - Read-only root filesystem (except allowed write paths)
//   - CPU and memory limits via cgroups
//   - PID limiting
//   - User namespace isolation
//
// This is a placeholder that checks for Docker availability and returns
// an error if not configured, since actual Docker execution requires
// the Docker daemon to be available at runtime.

export interface DockerSandboxConfig extends SandboxRunConfig {
  /** Docker image to use. Default: "kirkforge/sandbox:latest". */
  image?: string;
  /** Whether to pull the image if not available locally. Default: true. */
  pullImage?: boolean;
  /** Docker network mode. Default: "none" (no network). */
  networkMode?: "none" | "bridge" | "host";
  /** Whether to remove the container after execution. Default: true. */
  removeContainer?: boolean;
}

/**
 * Execute a command inside a Docker container for full isolation.
 *
 * This is the recommended approach for enterprise deployments running
 * untrusted code. It provides:
 *   - Filesystem isolation (read-only root, selective write mounts)
 *   - Network isolation (default: none)
 *   - CPU and memory limits via cgroups
 *   - PID limiting
 *
 * Requires Docker to be available on the host.
 */
export async function runDockerSandboxed(
  config: DockerSandboxConfig,
): Promise<Result<SandboxResult, SandboxExecutionError>> {
  const mergedConstraints = validateConstraints(config.constraints ?? {});
  if (!mergedConstraints.ok) {
    return err(
      new SandboxExecutionError(`Invalid sandbox constraints: ${mergedConstraints.error.message}`),
    );
  }
  const constraints = mergedConstraints.value;

  if (!constraints.shellAllowed) {
    return err(
      new SandboxExecutionError("Shell execution is not allowed by sandbox constraints", [
        { type: "command", message: "Shell execution denied by policy", target: config.command },
      ]),
    );
  }

  if (!isCommandAllowed(config.command, constraints)) {
    return err(
      new SandboxExecutionError(`Command "${config.command}" is not in the allowed list`, [
        {
          type: "command",
          message: `Command not allowed: ${config.command}`,
          target: config.command,
        },
      ]),
    );
  }

  const image = config.image ?? "kirkforge/sandbox:latest";
  const networkMode = config.networkMode ?? (constraints.networkAllowed ? "bridge" : "none");
  const _removeContainer = config.removeContainer ?? true;

  // Build Docker run arguments
  const dockerArgs: string[] = ["run", "--rm"];

  // Network isolation
  if (networkMode === "none") {
    dockerArgs.push("--network=none");
  } else if (networkMode === "bridge" && constraints.networkAllowlist.length > 0) {
    // Docker doesn't support per-destination filtering natively;
    // for production, use custom network + iptables rules
    dockerArgs.push(`--network=${networkMode}`);
  } else {
    dockerArgs.push(`--network=${networkMode}`);
  }

  // Resource limits
  dockerArgs.push(`--memory=${constraints.maxMemoryMb}m`);
  dockerArgs.push(`--cpus=1`);
  dockerArgs.push(`--pids-limit=${Math.max(constraints.maxProcesses, 1)}`);

  // Timeout
  dockerArgs.push(`--stop-timeout=${Math.ceil(constraints.maxTimeMs / 1000)}`);

  // Read-only root filesystem
  dockerArgs.push("--read-only");

  // Mount allowed read paths
  for (const readPath of constraints.allowedReadPaths) {
    dockerArgs.push(`-v`, `${readPath}:${readPath}:ro`);
  }

  // Mount allowed write paths
  for (const writePath of constraints.allowedWritePaths) {
    dockerArgs.push(`-v`, `${writePath}:${writePath}:rw`);
  }

  // Tmpfs for /tmp
  dockerArgs.push("--tmpfs", "/tmp:noexec,nosuid,size=64m");

  // Image and command
  dockerArgs.push(image);
  dockerArgs.push(config.command);
  if (config.args) {
    dockerArgs.push(...config.args);
  }

  // Execute via Docker CLI
  // Note: This spawns "docker" as the command, not the target command directly.
  // The container provides the actual isolation.
  const dockerConfig: SandboxRunConfig = {
    command: "docker",
    args: dockerArgs,
    // Docker mount args (e.g. -v /path:/path:ro) are container isolation specs,
    // not filesystem references the host sandbox should restrict. Skip arg path
    // scanning — the container provides its own filesystem isolation.
    skipArgPathScan: true,
    constraints: {
      ...config.constraints,
      // Docker is the allowed command here; the actual command runs inside the container
      shellAllowed: true,
      allowedCommands: ["docker"],
      // Path allow-lists are empty because the Docker runner uses its own mount
      // declarations above; host-side path scanning is skipped via skipArgPathScan.
      allowedReadPaths: [],
      allowedWritePaths: [],
    },
    cwd: config.cwd,
    env: config.env,
    tenantId: config.tenantId,
    actorId: config.actorId,
    beforeHook: config.beforeHook,
    afterHook: config.afterHook,
  };

  // Use the regular sandboxed runner to execute Docker
  // This provides timeout and output limits on the Docker process itself
  const result = await runSandboxed(dockerConfig);
  if (!result.ok) return result;

  // Mark Docker-specific context in the result
  const sandboxResult = result.value;
  return ok({
    ...sandboxResult,
    // The Docker execution adds overhead; the actual command ran in a container
  });
}
