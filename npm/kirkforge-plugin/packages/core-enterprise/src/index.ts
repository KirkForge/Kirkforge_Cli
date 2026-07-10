import { ok, err, type Result } from "@kirkforge/core-types";
import { KirkForgeError } from "@kirkforge/core-errors";

// ── Enterprise mode ────────────────────────────────────────────────────────
//
// When KIRKFORGE_ENTERPRISE_MODE=1, the system MUST fail to start unless all
// required enterprise controls are configured. This prevents the silent-
// fallback-to-dev-mode behavior that would be catastrophic in a shared
// multi-tenant deployment.
//
// Required controls in enterprise mode:
//   1. Auth — at least one auth method configured (OIDC issuer or API key)
//   2. Audit — audit sink configured (not just in-memory events)
//   3. Policy — policy source configured (file path or service URL)
//   4. Durable storage — memory store backend must not be in-memory fallback
//   5. Secrets — secrets provider chain should not fall through to env-only

// ── Types ──────────────────────────────────────────────────────────────────

export interface EnterpriseConfig {
  /** Whether enterprise mode is active. Read from KIRKFORGE_ENTERPRISE_MODE env. */
  enabled: boolean;
  /** Auth configuration. */
  auth: AuthConfig;
  /** Audit sink configuration. */
  audit: AuditConfig;
  /** Policy configuration. */
  policy: PolicyConfig;
  /** Storage configuration. */
  storage: StorageConfig;
  /** Secrets configuration. */
  secrets: SecretsConfig;
}

export interface AuthConfig {
  oidcIssuer?: string;
  oidcAudience?: string;
  apiKey?: string;
  readonly configured: boolean;
}

export interface AuditConfig {
  sinkType: "file" | "syslog" | "http" | "none";
  filePath?: string;
  httpUrl?: string;
  readonly configured: boolean;
}

export interface PolicyConfig {
  filePath?: string;
  serviceUrl?: string;
  readonly configured: boolean;
}

export interface StorageConfig {
  backend: "sqlite" | "file" | "memory";
  readonly durable: boolean;
}

export interface SecretsConfig {
  providers: string[];
  readonly envOnlyFallback: boolean;
}

export interface EnterpriseViolation {
  control: string;
  severity: "critical" | "warning";
  message: string;
  remediation: string;
}

export class EnterpriseModeError extends KirkForgeError {
  violations: EnterpriseViolation[];
  constructor(violations: EnterpriseViolation[]) {
    const msg = violations.map((v) => `[${v.severity}] ${v.control}: ${v.message}`).join("; ");
    super("ENTERPRISE_MODE_VIOLATION", `Enterprise mode validation failed: ${msg}`, {
      violations,
    });
    this.name = "EnterpriseModeError";
    this.violations = violations;
  }
}

// ── Enterprise mode detection ──────────────────────────────────────────────

const ENTERPRISE_ENV_KEY = "KIRKFORGE_ENTERPRISE_MODE";

/** Check if enterprise mode is enabled via environment variable. */
export function isEnterpriseMode(envOverride?: Record<string, string | undefined>): boolean {
  const e = envOverride ?? process.env;
  const val = e[ENTERPRISE_ENV_KEY];
  return val === "1" || val === "true" || val === "yes";
}

// ── Validation ────────────────────────────────────────────────────────────

/**
 * Validate enterprise mode requirements.
 * Returns ok(config) if all critical controls are met, or err with violations.
 */
export function validateEnterpriseMode(
  env?: Record<string, string | undefined>,
): Result<EnterpriseConfig, EnterpriseModeError> {
  const e = env ?? (process.env as Record<string, string | undefined>);
  const violations: EnterpriseViolation[] = [];

  // ── Auth ──────────────────────────────────────────────────────────────
  const oidcIssuer = e.OIDC_ISSUER ?? e["KIRKFORGE_OIDC_ISSUER"];
  const oidcAudience = e.OIDC_AUDIENCE ?? e["KIRKFORGE_OIDC_AUDIENCE"];
  const apiKey = e.HEALTH_API_KEY ?? e["KIRKFORGE_API_KEY"];
  const authConfigured = !!(oidcIssuer || (apiKey && apiKey.length >= 32));
  const authConfig: AuthConfig = {
    oidcIssuer,
    oidcAudience,
    apiKey: apiKey ? "***" : undefined,
    get configured() {
      return authConfigured;
    },
  };

  if (!authConfigured) {
    violations.push({
      control: "auth",
      severity: "critical",
      message: "No auth method configured. Requires OIDC issuer or API key (>=32 chars).",
      remediation: "Set OIDC_ISSUER or HEALTH_API_KEY environment variable.",
    });
  } else if (apiKey && apiKey.length < 32) {
    violations.push({
      control: "auth",
      severity: "warning",
      message: "API key is shorter than 32 characters. Consider a stronger key for production.",
      remediation: "Generate a key with at least 32 characters: openssl rand -hex 24",
    });
  }

  // ── Audit ────────────────────────────────────────────────────────────
  const auditSinkType = e.AUDIT_SINK_TYPE ?? e["KIRKFORGE_AUDIT_SINK"] ?? "none";
  const auditFilePath = e.AUDIT_FILE_PATH;
  const auditHttpUrl = e.AUDIT_HTTP_URL;
  const auditConfigured =
    auditSinkType !== "none" &&
    auditSinkType !== "memory" &&
    (auditSinkType === "file" ? !!auditFilePath : auditSinkType === "http" ? !!auditHttpUrl : true);
  const auditConfig: AuditConfig = {
    sinkType: auditSinkType as AuditConfig["sinkType"],
    filePath: auditFilePath,
    httpUrl: auditHttpUrl,
    get configured() {
      return auditConfigured;
    },
  };

  if (!auditConfigured) {
    violations.push({
      control: "audit",
      severity: "critical",
      message: "No durable audit sink configured. In-memory events are not sufficient.",
      remediation:
        "Set AUDIT_SINK_TYPE=file or http, and provide AUDIT_FILE_PATH or AUDIT_HTTP_URL.",
    });
  }

  // ── Policy ───────────────────────────────────────────────────────────
  const policyFilePath = e.POLICY_FILE_PATH ?? e["KIRKFORGE_POLICY_FILE"];
  const policyServiceUrl = e.POLICY_SERVICE_URL ?? e["KIRKFORGE_POLICY_URL"];
  const policyConfigured = !!(policyFilePath || policyServiceUrl);
  const policyConfig: PolicyConfig = {
    filePath: policyFilePath,
    serviceUrl: policyServiceUrl,
    get configured() {
      return policyConfigured;
    },
  };

  if (!policyConfigured) {
    violations.push({
      control: "policy",
      severity: "critical",
      message: "No policy source configured. Enterprise mode requires explicit policy.",
      remediation: "Set POLICY_FILE_PATH or POLICY_SERVICE_URL.",
    });
  }

  // ── Storage ──────────────────────────────────────────────────────────
  const storageBackend = e.MEMORY_BACKEND ?? e["KIRKFORGE_MEMORY_BACKEND"] ?? "memory";
  const isDurable = storageBackend === "sqlite";
  const storageConfig: StorageConfig = {
    backend: storageBackend as StorageConfig["backend"],
    get durable() {
      return isDurable;
    },
  };

  if (!isDurable) {
    violations.push({
      control: "storage",
      severity: "critical",
      message: `Memory backend is "${storageBackend}". Enterprise mode requires "sqlite" for durability.`,
      remediation: "Set MEMORY_BACKEND=sqlite and ensure the database path is configured.",
    });
  }

  // ── Secrets ──────────────────────────────────────────────────────────
  const secretsProviders: string[] = [];
  let envOnlyFallback = true;
  if (e.VAULT_ADDR && e.VAULT_TOKEN) secretsProviders.push("vault");
  if (e.AWS_REGION && e.AWS_ACCESS_KEY_ID && e.AWS_SECRET_ACCESS_KEY) secretsProviders.push("aws");
  if (e.GCP_PROJECT_ID) secretsProviders.push("gcp");
  if (secretsProviders.length === 0) {
    secretsProviders.push("env");
  } else {
    secretsProviders.push("env");
    envOnlyFallback = false;
  }
  const secretsConfig: SecretsConfig = {
    providers: secretsProviders,
    get envOnlyFallback() {
      return envOnlyFallback;
    },
  };

  if (envOnlyFallback) {
    violations.push({
      control: "secrets",
      severity: "warning",
      message: "Secrets chain falls through to env-only. Consider configuring Vault or cloud KMS.",
      remediation: "Configure VAULT_ADDR+VAULT_TOKEN, AWS credentials, or GCP_PROJECT_ID.",
    });
  }

  const config: EnterpriseConfig = {
    enabled: true,
    auth: authConfig,
    audit: auditConfig,
    policy: policyConfig,
    storage: storageConfig,
    secrets: secretsConfig,
  };

  const criticalViolations = violations.filter((v) => v.severity === "critical");
  if (criticalViolations.length > 0) {
    return err(new EnterpriseModeError(violations));
  }

  return ok(config);
}

/**
 * Check enterprise mode and validate. Returns dev-mode config if enterprise
 * mode is off, or validates if on.
 */
export function requireEnterpriseOrDev(
  env?: Record<string, string | undefined>,
): Result<EnterpriseConfig, EnterpriseModeError> {
  if (!isEnterpriseMode(env)) {
    return ok({
      enabled: false,
      auth: { configured: false },
      audit: { configured: false, sinkType: "none" },
      policy: { configured: false },
      storage: { backend: "memory", durable: false },
      secrets: { providers: ["env"], envOnlyFallback: true },
    });
  }
  return validateEnterpriseMode(env);
}

// ── Startup gate ───────────────────────────────────────────────────────────

export interface StartupLogger {
  info: (msg: string) => void;
  warn: (msg: string) => void;
  error: (msg: string) => void;
}

/**
 * Startup gate: call this at application boot. In enterprise mode, it will
 * throw if critical controls are missing. In dev mode, it logs warnings.
 */
/**
 * Startup gate: call this at application boot. In enterprise mode, it will
 * throw if critical controls are missing. In dev mode, it logs warnings.
 *
 * @deprecated Use requireEnterpriseOrDev() instead. The CLI bootstrap already
 * uses requireEnterpriseOrDev directly — this function was dead code that has
 * been reconnected to the same validation path for backward compatibility.
 */
export function enterpriseStartupGate(
  logger?: StartupLogger,
  env?: Record<string, string | undefined>,
): EnterpriseConfig {
  const log: StartupLogger = logger ?? {
    info: (m: string) => console.log(m),
    warn: (m: string) => console.warn(m),
    error: (m: string) => console.error(m),
  };

  const result = requireEnterpriseOrDev(env);
  if (!result.ok) {
    for (const v of result.error.violations) {
      if (v.severity === "critical") {
        log.error(`[enterprise] CRITICAL: ${v.control} — ${v.message}`);
        log.error(`[enterprise]   Remediation: ${v.remediation}`);
      } else {
        log.warn(`[enterprise] WARNING: ${v.control} — ${v.message}`);
        log.warn(`[enterprise]   Remediation: ${v.remediation}`);
      }
    }
    throw result.error;
  }

  const config = result.value;
  if (isEnterpriseMode(env)) {
    log.info("[enterprise] Enterprise mode ENABLED. All critical controls validated.");
  } else {
    log.info("[enterprise] Running in developer mode. Enterprise controls not enforced.");
  }
  return config;
}

// ── Per-tenant quotas and rate limiting ──────────────────────────────────────

export {
  TenantQuota,
  DEFAULT_QUOTA,
  QuotaUsage,
  QuotaExceededError,
  QuotaManager,
  QuotaAction,
  RateLimiter,
  RateLimitConfig,
} from "./quotas.js";

// ── Quota persistence ──────────────────────────────────────────────────────

export {
  QuotaPersistence,
  QuotaPersistenceError,
  type QuotaPersistenceConfig,
} from "./quota-persistence.js";
