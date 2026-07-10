import { ok, err, type Result } from "@kirkforge/core-types";
import { normalize } from "node:path";
import { KirkForgeError } from "@kirkforge/core-errors";

// ── Sandboxed execution for KirkForge ─────────────────────────────────────────
//
// Provides constrained execution environments for tool/verifier invocations
// and generated code. Enterprise deployments MUST configure sandbox limits
// before running untrusted code.
//
// Design principles:
//   1. Deny-by-default: no network, no filesystem writes, no shell unless
//      explicitly allowed by policy.
//   2. Resource limits: CPU time, memory, wall-clock time, and process count
//      are enforced.
//   3. Audit: every sandbox invocation is logged with actor, tenant, tool,
//      and outcome.
//   4. Isolation: tenant-scoped sandbox policies prevent cross-tenant resource
//      leakage.

// ── Sandbox constraints ─────────────────────────────────────────────────────

export interface SandboxConstraints {
  /** Maximum wall-clock time for execution in milliseconds. Default: 60000. */
  maxTimeMs?: number;
  /** Maximum memory in MB. Default: 512. */
  maxMemoryMb?: number;
  /** Maximum CPU time in milliseconds. Default: 30000. */
  maxCpuMs?: number;
  /** Maximum number of child processes. Default: 0 (no forking). */
  maxProcesses?: number;
  /** Whether network egress is allowed. Default: false. */
  networkAllowed?: boolean;
  /** Allowed network destinations (host:port). Only relevant if networkAllowed is true. */
  // ⚠ ADVISORY on the bare-host runner: networkAllowlist is checked by
  // isNetworkDestinationAllowed() but NOT enforced at the process level.
  // A bare-host child process can make any network connection regardless of
  // this list. For enforcement, use runDockerSandboxed() with custom network
  // + iptables, or a microVM with egress filtering.
  networkAllowlist?: string[];
  /** Denied network destinations. Takes precedence over allowlist. */
  networkDenylist?: string[];
  /** Whether shell command execution is allowed. Default: false. */
  shellAllowed?: boolean;
  /** Allowed shell commands (only relevant if shellAllowed is true). */
  allowedCommands?: string[];
  /** Allowed filesystem read paths. Default: empty (no reads). */
  allowedReadPaths?: string[];
  /** Allowed filesystem write paths. Default: empty (no writes). */
  allowedWritePaths?: string[];
  /** Whether to redact secrets from tool output. Default: true. */
  redactSecrets?: boolean;
  /** Maximum output size in bytes. Default: 1MB. */
  maxOutputBytes?: number;
}

export const DEFAULT_CONSTRAINTS: Required<SandboxConstraints> = {
  maxTimeMs: 60000,
  maxMemoryMb: 512,
  maxCpuMs: 30000,
  maxProcesses: 0,
  networkAllowed: false,
  networkAllowlist: [],
  networkDenylist: [],
  shellAllowed: false,
  allowedCommands: [],
  allowedReadPaths: [],
  allowedWritePaths: [],
  redactSecrets: true,
  maxOutputBytes: 1024 * 1024,
};

// ── Sandbox execution result ────────────────────────────────────────────────

export interface SandboxResult {
  /** Whether execution completed within constraints. */
  success: boolean;
  /** Exit code (if available). */
  exitCode: number | null;
  /** Captured stdout. */
  stdout: string;
  /** Captured stderr. */
  stderr: string;
  /** Execution time in milliseconds. */
  durationMs: number;
  /** Peak memory usage in MB (if measurable). */
  peakMemoryMb: number | null;
  /** Constraint violations detected. */
  violations: SandboxViolation[];
  /** Whether the result was truncated due to output size limits. */
  truncated: boolean;
}

export interface SandboxViolation {
  /** Type of violation. */
  type:
    | "time"
    | "memory"
    | "network"
    | "filesystem"
    | "process"
    | "output_size"
    | "command"
    | "secret";
  /** Description of the violation. */
  message: string;
  /** The resource or target that was accessed beyond limits. */
  target?: string;
}

// ── Sandbox configuration ───────────────────────────────────────────────────

export interface SandboxConfig {
  /** Sandbox constraints for all invocations. */
  constraints: SandboxConstraints;
  /** Tenant ID for multi-tenant isolation. */
  tenantId?: string;
  /** Actor ID for audit logging. */
  actorId?: string;
  /** Pre-execution hook called before each invocation. */
  beforeHook?: (context: SandboxContext) => Result<void, SandboxError>;
  /** Post-execution hook called after each invocation. */
  afterHook?: (context: SandboxContext, result: SandboxResult) => void;
}

export interface SandboxContext {
  /** Tool or command being executed. */
  tool: string;
  /** Arguments passed to the tool. */
  args: string[];
  /** Tenant ID. */
  tenantId: string;
  /** Actor ID. */
  actorId: string;
  /** Constraints applied. */
  constraints: Required<SandboxConstraints>;
}

// ── Sandbox error ──────────────────────────────────────────────────────────

export class SandboxError extends KirkForgeError {
  constructor(code: string, message: string, context?: Record<string, unknown>) {
    super(code, message, context);
    this.name = "SandboxError";
  }
}

// ── Sandbox runtime ────────────────────────────────────────────────────────

/**
 * Validates sandbox constraints before execution.
 * Returns ok if constraints are valid, err with details if invalid.
 */
export function validateConstraints(
  constraints: SandboxConstraints,
): Result<Required<SandboxConstraints>, SandboxError> {
  const c: Required<SandboxConstraints> = {
    ...DEFAULT_CONSTRAINTS,
    ...constraints,
  };

  if (c.maxTimeMs < 1000) {
    return err(
      new SandboxError(
        "INVALID_CONSTRAINTS",
        `maxTimeMs must be at least 1000ms, got ${c.maxTimeMs}`,
      ),
    );
  }

  if (c.maxMemoryMb < 16) {
    return err(
      new SandboxError(
        "INVALID_CONSTRAINTS",
        `maxMemoryMb must be at least 16MB, got ${c.maxMemoryMb}`,
      ),
    );
  }

  if (c.maxCpuMs < 1000) {
    return err(
      new SandboxError(
        "INVALID_CONSTRAINTS",
        `maxCpuMs must be at least 1000ms, got ${c.maxCpuMs}`,
      ),
    );
  }

  if (c.maxProcesses < 0) {
    return err(
      new SandboxError("INVALID_CONSTRAINTS", `maxProcesses must be >= 0, got ${c.maxProcesses}`),
    );
  }

  // If network is not allowed, allowlist should be empty
  if (!c.networkAllowed && c.networkAllowlist.length > 0) {
    return err(
      new SandboxError(
        "INVALID_CONSTRAINTS",
        "networkAllowlist is set but networkAllowed is false. Set networkAllowed=true to use allowlist.",
      ),
    );
  }

  // If shell is not allowed, allowedCommands should be empty
  if (!c.shellAllowed && c.allowedCommands.length > 0) {
    return err(
      new SandboxError(
        "INVALID_CONSTRAINTS",
        "allowedCommands is set but shellAllowed is false. Set shellAllowed=true to allow commands.",
      ),
    );
  }

  // Denylist entries take precedence over allowlist
  const denySet = new Set(c.networkDenylist);
  const conflicts = c.networkAllowlist.filter((entry) => denySet.has(entry));
  if (conflicts.length > 0) {
    return err(
      new SandboxError(
        "INVALID_CONSTRAINTS",
        `networkAllowlist and networkDenylist overlap: ${conflicts.join(", ")}`,
      ),
    );
  }

  return ok(c);
}

/**
 * Merge tenant-specific constraints with base constraints.
 * Tenant constraints can only tighten (further restrict) the base constraints,
 * never loosen them. This ensures tenant isolation.
 */
export function mergeConstraints(
  base: Required<SandboxConstraints>,
  tenant: SandboxConstraints,
): Required<SandboxConstraints> {
  return {
    // Take the minimum (most restrictive) for resource limits
    maxTimeMs:
      tenant.maxTimeMs !== undefined ? Math.min(base.maxTimeMs, tenant.maxTimeMs) : base.maxTimeMs,
    maxMemoryMb:
      tenant.maxMemoryMb !== undefined
        ? Math.min(base.maxMemoryMb, tenant.maxMemoryMb)
        : base.maxMemoryMb,
    maxCpuMs:
      tenant.maxCpuMs !== undefined ? Math.min(base.maxCpuMs, tenant.maxCpuMs) : base.maxCpuMs,
    maxProcesses:
      tenant.maxProcesses !== undefined
        ? Math.min(base.maxProcesses, tenant.maxProcesses)
        : base.maxProcesses,
    // Tenant cannot enable what base denies
    networkAllowed: base.networkAllowed && (tenant.networkAllowed ?? true),
    // Tenant allowlist must be subset of base allowlist
    networkAllowlist: tenant.networkAllowlist
      ? tenant.networkAllowlist.filter(
          (e) => base.networkAllowlist.includes(e) || base.networkAllowlist.length === 0,
        )
      : base.networkAllowlist,
    // Merge denylists
    networkDenylist: [...new Set([...base.networkDenylist, ...(tenant.networkDenylist ?? [])])],
    // Tenant cannot enable shell if base denies it
    shellAllowed: base.shellAllowed && (tenant.shellAllowed ?? true),
    // Tenant commands must be subset of base commands
    allowedCommands: tenant.allowedCommands
      ? tenant.allowedCommands.filter(
          (c) => base.allowedCommands.includes(c) || base.allowedCommands.length === 0,
        )
      : base.allowedCommands,
    // Tenant read paths must be subset of base read paths
    allowedReadPaths: tenant.allowedReadPaths
      ? tenant.allowedReadPaths.filter(
          (p) => base.allowedReadPaths.includes(p) || base.allowedReadPaths.length === 0,
        )
      : base.allowedReadPaths,
    // Tenant write paths must be subset of base write paths
    allowedWritePaths: tenant.allowedWritePaths
      ? tenant.allowedWritePaths.filter(
          (p) => base.allowedWritePaths.includes(p) || base.allowedWritePaths.length === 0,
        )
      : base.allowedWritePaths,
    // Redaction cannot be disabled by tenant
    redactSecrets:
      base.redactSecrets || (tenant.redactSecrets !== undefined ? tenant.redactSecrets : true),
    // Take minimum for output size
    maxOutputBytes:
      tenant.maxOutputBytes !== undefined
        ? Math.min(base.maxOutputBytes, tenant.maxOutputBytes)
        : base.maxOutputBytes,
  };
}

/**
 * Check whether a command is allowed under the given constraints.
 */
export function isCommandAllowed(
  command: string,
  constraints: Required<SandboxConstraints>,
): boolean {
  if (!constraints.shellAllowed) return false;
  if (constraints.allowedCommands.length === 0) return false;
  return constraints.allowedCommands.includes(command);
}

/**
 * Check whether a network destination is allowed under the given constraints.
 */
export function isNetworkDestinationAllowed(
  destination: string,
  constraints: Required<SandboxConstraints>,
): boolean {
  if (!constraints.networkAllowed) return false;
  if (constraints.networkDenylist.includes(destination)) return false;
  if (constraints.networkAllowlist.length === 0) return true;
  return constraints.networkAllowlist.includes(destination);
}

/**
 * Check whether a filesystem path is allowed for reading under the given constraints.
 */
export function isReadPathAllowed(
  path: string,
  constraints: Required<SandboxConstraints>,
): boolean {
  if (constraints.allowedReadPaths.length === 0) return false;
  const normalized = normalize(path);
  // Reject paths that resolve outside their allowed root
  // by checking that the normalized path still starts with the allowed prefix
  return constraints.allowedReadPaths.some((allowed) => {
    const normalizedAllowed = normalize(allowed);
    return (
      normalized === normalizedAllowed ||
      (normalized.startsWith(normalizedAllowed + "/") && !normalized.includes(".."))
    );
  });
}

/**
 * Check whether a filesystem path is allowed for writing under the given constraints.
 */
export function isWritePathAllowed(
  path: string,
  constraints: Required<SandboxConstraints>,
): boolean {
  if (constraints.allowedWritePaths.length === 0) return false;
  const normalized = normalize(path);
  // Reject paths that resolve outside their allowed root
  return constraints.allowedWritePaths.some((allowed) => {
    const normalizedAllowed = normalize(allowed);
    return (
      normalized === normalizedAllowed ||
      (normalized.startsWith(normalizedAllowed + "/") && !normalized.includes(".."))
    );
  });
}

/**
 * Create a sandbox context for a given tool invocation.
 * Validates constraints and calls the before-hook if provided.
 */
export function createSandboxContext(
  tool: string,
  args: string[],
  config: SandboxConfig,
): Result<SandboxContext, SandboxError> {
  const mergedConstraints = validateConstraints(config.constraints);
  if (!mergedConstraints.ok) return err(mergedConstraints.error);

  const context: SandboxContext = {
    tool,
    args,
    tenantId: config.tenantId ?? "",
    actorId: config.actorId ?? "",
    constraints: mergedConstraints.value,
  };

  if (config.beforeHook) {
    const hookResult = config.beforeHook(context);
    if (!hookResult.ok) return err(hookResult.error as SandboxError);
  }

  return ok(context);
}
// ── Re-exports ───────────────────────────────────────────────────────────────

export {
  runSandboxed,
  runDockerSandboxed,
  SandboxExecutionError,
  type SandboxRunConfig,
  type DockerSandboxConfig,
} from "./runner.js";
