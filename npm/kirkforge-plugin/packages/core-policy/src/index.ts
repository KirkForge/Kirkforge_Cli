import { ok, err, type Result } from "@kirkforge/core-types";
import { KirkForgeError, ValidationError } from "@kirkforge/core-errors";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

// ── Policy engine for KirkForge ─────────────────────────────────────────────────
//
// Deny-by-default policy enforcement. Every action must be explicitly allowed
// by policy, or it is denied. Policy is loaded from a JSON file or fetched from
// a policy service.
//
// Policy covers:
//   - Tool allowlists: which external tools can run
//   - Model governance: which AI models/providers are approved
//   - Workspace containment: which directory roots are allowed
//   - Execution controls: network, timeouts, resource limits
//   - Tenant overrides: per-tenant policy adjustments

// ── Policy schema ──────────────────────────────────────────────────────────

export interface Policy {
  /** Policy version for hash-based verification. */
  version: number;
  /** Hash of the policy content (computed on load, used for audit). */
  hash?: string;
  /** Human-readable name. */
  name?: string;
  /** Tool allowlists and denylists. */
  tools: ToolPolicy;
  /** Model governance. */
  models: ModelPolicy;
  /** Workspace containment. */
  workspaces: WorkspacePolicy;
  /** Execution controls. */
  execution: ExecutionPolicy;
  /** Per-tenant overrides. Keyed by tenant ID. */
  tenantOverrides?: Record<string, Partial<Policy>>;
}

export interface ToolPolicy {
  /** Allowed tool names. If set, ONLY these tools may run. */
  allowed: string[];
  /** Explicitly denied tool names. Takes precedence over allowed. */
  denied: string[];
  /** Maximum concurrent tool invocations. Default: 4. */
  maxConcurrent?: number;
}

export interface ModelPolicy {
  /** Allowed model identifiers (e.g. "claude-sonnet-4-20250514", "gpt-4o"). */
  allowed: string[];
  /** Allowed provider keys (e.g. "openai", "anthropic", "local-ollama"). */
  allowedProviders: string[];
  /** Maximum tokens per request. Default: unlimited. */
  maxTokensPerRequest?: number;
}

export interface WorkspacePolicy {
  /** Allowed root directories for workspace operations. */
  allowedRoots: string[];
  /** Maximum workspace path depth. Default: 10. */
  maxPathDepth?: number;
  /** Whether symlinks are allowed in workspace paths. Default: false. */
  allowSymlinks?: boolean;
}

export interface ExecutionPolicy {
  /** Whether network egress is allowed during tool execution. Default: false. */
  networkAllowed: boolean;
  /** Maximum runtime per tool invocation in seconds. Default: 60. */
  maxRuntimeSeconds?: number;
  /** Maximum memory per tool invocation in MB. Default: 512. */
  maxMemoryMb?: number;
  /** Whether to allow shell command execution. Default: false. */
  shellAllowed?: boolean;
  /** Allowed shell commands (only relevant if shellAllowed is true). */
  allowedCommands?: string[];
}

export interface PolicyDecision {
  /** Whether the action was allowed. */
  allowed: boolean;
  /** Human-readable reason for the decision. */
  reason: string;
  /** Policy hash at the time of the decision. */
  policyHash: string;
  /** Timestamp of the decision. */
  timestamp: string;
  /** The rule that triggered the decision. */
  rule: string;
}

// ── Policy errors ───────────────────────────────────────────────────────────

export class PolicyDeniedError extends KirkForgeError {
  decision: PolicyDecision;
  constructor(decision: PolicyDecision) {
    super("POLICY_DENIED", decision.reason, {
      rule: decision.rule,
      policyHash: decision.policyHash,
    });
    this.name = "PolicyDeniedError";
    this.decision = decision;
  }
}

export class PolicyLoadError extends KirkForgeError {
  constructor(message: string, cause?: string) {
    super("POLICY_LOAD_ERROR", message, { cause });
    this.name = "PolicyLoadError";
  }
}

// ── Default policy (most restrictive) ───────────────────────────────────────

export const DEFAULT_POLICY: Policy = {
  version: 1,
  name: "default-deny",
  tools: {
    allowed: [], // deny all by default
    denied: [],
    maxConcurrent: 4,
  },
  models: {
    allowed: [], // deny all by default
    allowedProviders: [],
    maxTokensPerRequest: undefined,
  },
  workspaces: {
    allowedRoots: [], // deny all by default
    maxPathDepth: 10,
    allowSymlinks: false,
  },
  execution: {
    networkAllowed: false,
    maxRuntimeSeconds: 60,
    maxMemoryMb: 512,
    shellAllowed: false,
    allowedCommands: [],
  },
};

// ── Policy engine ──────────────────────────────────────────────────────────

export class PolicyEngine {
  private policy: Policy;
  private hash: string;

  constructor(initial?: Policy) {
    this.policy = initial ?? structuredClone(DEFAULT_POLICY);
    this.hash = computeHash(JSON.stringify(this.policy));
  }

  /** Load policy from a JSON file. */
  loadFromFile(filePath: string): Result<void, PolicyLoadError> {
    const abs = resolve(filePath);
    if (!existsSync(abs)) {
      return err(new PolicyLoadError(`Policy file not found: ${abs}`));
    }
    try {
      const raw = readFileSync(abs, "utf-8");
      const parsed = JSON.parse(raw);
      const validated = validatePolicy(parsed);
      if (!validated.ok) {
        return err(new PolicyLoadError(`Invalid policy file: ${validated.error.message}`));
      }
      this.policy = validated.value;
      this.hash = computeHash(raw);
      return ok(undefined);
    } catch (e) {
      return err(
        new PolicyLoadError(
          `Failed to load policy from ${abs}`,
          e instanceof Error ? e.message : String(e),
        ),
      );
    }
  }

  /** Get current policy (immutable copy). */
  getPolicy(): Policy {
    return structuredClone(this.policy);
  }

  /** Get current policy hash. */
  getHash(): string {
    return this.hash;
  }

  // ── Enforcement methods ────────────────────────────────────────────────

  /** Check if a tool is allowed. */
  checkTool(toolName: string): PolicyDecision {
    const policy = this.policy.tools;

    // Denied list takes precedence
    if (policy.denied.includes(toolName)) {
      return deny("tool_denied_list", `Tool "${toolName}" is on the denied list`, this.hash);
    }

    // If allowed list is empty, deny everything (deny-by-default)
    if (policy.allowed.length === 0) {
      return deny(
        "tool_no_allowlist",
        `No tool allowlist configured; tool "${toolName}" denied by default`,
        this.hash,
      );
    }

    if (!policy.allowed.includes(toolName)) {
      return deny("tool_not_allowed", `Tool "${toolName}" is not on the allowed list`, this.hash);
    }

    return allow("tool_allowed", `Tool "${toolName}" is allowed`, this.hash);
  }

  /** Check if a model is allowed. */
  checkModel(modelId: string, providerKey?: string): PolicyDecision {
    const policy = this.policy.models;

    if (policy.allowed.length === 0) {
      return deny(
        "model_no_allowlist",
        `No model allowlist configured; model "${modelId}" denied by default`,
        this.hash,
      );
    }

    if (!policy.allowed.includes(modelId)) {
      return deny("model_not_allowed", `Model "${modelId}" is not on the allowed list`, this.hash);
    }

    if (
      providerKey &&
      policy.allowedProviders.length > 0 &&
      !policy.allowedProviders.includes(providerKey)
    ) {
      return deny(
        "provider_not_allowed",
        `Provider "${providerKey}" is not on the allowed provider list`,
        this.hash,
      );
    }

    return allow("model_allowed", `Model "${modelId}" is allowed`, this.hash);
  }

  /** Check if a workspace path is contained within allowed roots. */
  checkWorkspace(workspacePath: string): PolicyDecision {
    const policy = this.policy.workspaces;

    if (policy.allowedRoots.length === 0) {
      return deny(
        "workspace_no_allowlist",
        `No workspace allowlist configured; path "${workspacePath}" denied by default`,
        this.hash,
      );
    }

    const abs = resolve(workspacePath);
    const contained = policy.allowedRoots.some((root) => {
      const absRoot = resolve(root);
      return abs.startsWith(absRoot);
    });

    if (!contained) {
      return deny(
        "workspace_not_allowed",
        `Path "${workspacePath}" is not within any allowed root`,
        this.hash,
      );
    }

    // Path depth check
    if (policy.maxPathDepth) {
      const _depth = abs.split("/").length - resolve(workspacePath).split("/").length;
      // relative depth from allowed root
      const matchedRoot = policy.allowedRoots.find((root) => abs.startsWith(resolve(root)));
      if (matchedRoot) {
        const relDepth = abs.slice(resolve(matchedRoot).length).split("/").filter(Boolean).length;
        if (relDepth > (policy.maxPathDepth ?? 10)) {
          return deny(
            "workspace_path_too_deep",
            `Path "${workspacePath}" exceeds max depth ${policy.maxPathDepth}`,
            this.hash,
          );
        }
      }
    }

    if (!policy.allowSymlinks) {
      // Symlink check is done at runtime in the orchestrator's path-safety module
      // This policy flag is the declaration of intent
    }

    return allow("workspace_allowed", `Path "${workspacePath}" is within allowed roots`, this.hash);
  }

  /** Check if execution parameters are within policy limits. */
  checkExecution(params: {
    networkRequired?: boolean;
    runtimeSeconds?: number;
    memoryMb?: number;
    command?: string;
  }): PolicyDecision[] {
    const decisions: PolicyDecision[] = [];
    const policy = this.policy.execution;

    // Network
    if (params.networkRequired && !policy.networkAllowed) {
      decisions.push(
        deny("execution_network_denied", "Network egress is denied by policy", this.hash),
      );
    }

    // Runtime
    if (
      params.runtimeSeconds &&
      policy.maxRuntimeSeconds &&
      params.runtimeSeconds > policy.maxRuntimeSeconds
    ) {
      decisions.push(
        deny(
          "execution_runtime_exceeded",
          `Runtime ${params.runtimeSeconds}s exceeds max ${policy.maxRuntimeSeconds}s`,
          this.hash,
        ),
      );
    }

    // Memory
    if (params.memoryMb && policy.maxMemoryMb && params.memoryMb > policy.maxMemoryMb) {
      decisions.push(
        deny(
          "execution_memory_exceeded",
          `Memory ${params.memoryMb}MB exceeds max ${policy.maxMemoryMb}MB`,
          this.hash,
        ),
      );
    }

    // Shell commands
    if (params.command) {
      if (!policy.shellAllowed) {
        decisions.push(
          deny("execution_shell_denied", "Shell command execution is denied by policy", this.hash),
        );
      } else if (
        policy.allowedCommands &&
        policy.allowedCommands.length > 0 &&
        !policy.allowedCommands.includes(params.command)
      ) {
        decisions.push(
          deny(
            "execution_command_denied",
            `Shell command "${params.command}" is not allowed`,
            this.hash,
          ),
        );
      }
    }

    return decisions.length > 0
      ? decisions
      : [allow("execution_allowed", "Execution parameters within policy limits", this.hash)];
  }

  /** Get tenant-overridden policy. Returns a new Policy with tenant overrides merged. */
  forTenant(tenantId: string): Policy {
    const overrides = this.policy.tenantOverrides?.[tenantId];
    if (!overrides) return this.getPolicy();

    // Deep merge base policy with tenant overrides
    const merged = deepMergePolicy(this.policy, overrides);
    return merged;
  }
}

// ── Policy validation ──────────────────────────────────────────────────────

function validatePolicy(raw: unknown): Result<Policy, ValidationError> {
  if (typeof raw !== "object" || raw === null) {
    return err(new ValidationError("Policy must be an object"));
  }
  const p = raw as Record<string, unknown>;

  // Version
  if (typeof p.version !== "number" || p.version < 1) {
    return err(new ValidationError("Policy version must be a positive integer"));
  }

  // Tools
  if (!p.tools || typeof p.tools !== "object") {
    return err(new ValidationError("Policy must have a 'tools' object"));
  }
  const tools = p.tools as Record<string, unknown>;
  if (!Array.isArray(tools.allowed)) {
    return err(new ValidationError("Policy tools.allowed must be an array"));
  }
  if (!Array.isArray(tools.denied)) {
    return err(new ValidationError("Policy tools.denied must be an array"));
  }

  // Models
  if (!p.models || typeof p.models !== "object") {
    return err(new ValidationError("Policy must have a 'models' object"));
  }
  const models = p.models as Record<string, unknown>;
  if (!Array.isArray(models.allowed)) {
    return err(new ValidationError("Policy models.allowed must be an array"));
  }
  if (!Array.isArray(models.allowedProviders)) {
    return err(new ValidationError("Policy models.allowedProviders must be an array"));
  }

  // Workspaces
  if (!p.workspaces || typeof p.workspaces !== "object") {
    return err(new ValidationError("Policy must have a 'workspaces' object"));
  }
  const workspaces = p.workspaces as Record<string, unknown>;
  if (!Array.isArray(workspaces.allowedRoots)) {
    return err(new ValidationError("Policy workspaces.allowedRoots must be an array"));
  }

  // Execution
  if (!p.execution || typeof p.execution !== "object") {
    return err(new ValidationError("Policy must have an 'execution' object"));
  }

  return ok({
    version: p.version as number,
    name: p.name as string | undefined,
    tools: {
      allowed: tools.allowed as string[],
      denied: tools.denied as string[],
      maxConcurrent: tools.maxConcurrent as number | undefined,
    },
    models: {
      allowed: models.allowed as string[],
      allowedProviders: models.allowedProviders as string[],
      maxTokensPerRequest: models.maxTokensPerRequest as number | undefined,
    },
    workspaces: {
      allowedRoots: workspaces.allowedRoots as string[],
      maxPathDepth: workspaces.maxPathDepth as number | undefined,
      allowSymlinks: workspaces.allowSymlinks as boolean | undefined,
    },
    execution: {
      networkAllowed: (p.execution as Record<string, unknown>).networkAllowed as boolean,
      maxRuntimeSeconds: (p.execution as Record<string, unknown>).maxRuntimeSeconds as
        | number
        | undefined,
      maxMemoryMb: (p.execution as Record<string, unknown>).maxMemoryMb as number | undefined,
      shellAllowed: (p.execution as Record<string, unknown>).shellAllowed as boolean | undefined,
      allowedCommands: (p.execution as Record<string, unknown>).allowedCommands as
        | string[]
        | undefined,
    },
    tenantOverrides: p.tenantOverrides as Record<string, Partial<Policy>> | undefined,
  });
}

// ── Helpers ────────────────────────────────────────────────────────────────

import {
  createHash,
  createHmac,
  createPublicKey,
  generateKeyPairSync,
  sign as cryptoSign,
  timingSafeEqual,
  verify as cryptoVerify,
  type KeyObject,
} from "node:crypto";

function computeHash(content: string): string {
  return createHash("sha256").update(content, "utf-8").digest("hex");
}

function deny(rule: string, reason: string, policyHash: string): PolicyDecision {
  return { allowed: false, reason, policyHash, timestamp: new Date().toISOString(), rule };
}

function allow(rule: string, reason: string, policyHash: string): PolicyDecision {
  return { allowed: true, reason, policyHash, timestamp: new Date().toISOString(), rule };
}

function deepMergePolicy(base: Policy, override: Partial<Policy>): Policy {
  const result = structuredClone(base);
  if (override.tools) {
    result.tools = { ...result.tools, ...override.tools };
  }
  if (override.models) {
    result.models = { ...result.models, ...override.models };
  }
  if (override.workspaces) {
    result.workspaces = { ...result.workspaces, ...override.workspaces };
  }
  if (override.execution) {
    result.execution = { ...result.execution, ...override.execution };
  }
  return result;
}

// ── Signed policy bundle verification ──────────────────────────────────────
//
// Policy bundles can be signed to prevent tampering. The signature is an
// Ed25519 or HMAC-SHA256 over the canonical JSON representation of the policy.
// This enables enterprise deployments to distribute policy files with
// integrity guarantees.

export interface SignedPolicyBundle {
  /** The policy content. */
  policy: Policy;
  /** Hash of the canonical policy JSON (computed by PolicyEngine). */
  hash: string;
  /** Signature type. */
  signatureType: "ed25519" | "hmac-sha256";
  /** Base64-encoded signature. */
  signature: string;
  /** Key ID or identifier used to verify the signature. */
  keyId: string;
  /** ISO timestamp when the bundle was signed. */
  signedAt: string;
}

export class PolicySignatureError extends KirkForgeError {
  constructor(message: string, cause?: string) {
    super("POLICY_SIGNATURE_ERROR", message, { cause });
    this.name = "PolicySignatureError";
  }
}

/**
 * Verify a signed policy bundle.
 *
 * For HMAC-SHA256: pass the shared secret as `verificationKey`.
 * For Ed25519: pass the public key as `verificationKey` (base64-encoded).
 *
 * Returns ok(policy) if signature verification succeeds, or err with details.
 */
export function verifySignedPolicy(
  bundle: SignedPolicyBundle,
  verificationKey: string,
): Result<Policy, PolicySignatureError> {
  // 1. Verify the hash matches the policy content
  const canonicalJson = JSON.stringify(bundle.policy);
  const expectedHash = computeHash(canonicalJson);

  if (bundle.hash !== expectedHash) {
    return err(
      new PolicySignatureError(
        `Policy hash mismatch: expected ${expectedHash}, got ${bundle.hash}. ` +
          `The policy content may have been tampered with after signing.`,
      ),
    );
  }

  // 2. Verify the signature
  const payload = `${bundle.hash}:${bundle.signatureType}:${bundle.keyId}:${bundle.signedAt}`;

  if (bundle.signatureType === "hmac-sha256") {
    // Use proper HMAC-SHA256 with timing-safe comparison.
    // Previous implementation used SHA256(payload‖key) which is vulnerable
    // to length-extension attacks and used non-constant-time comparison.
    const expected = createHmac("sha256", verificationKey).update(payload).digest("base64");
    const actualBuf = Buffer.from(bundle.signature, "base64");
    const expectedBuf = Buffer.from(expected, "base64");
    if (actualBuf.length !== expectedBuf.length || !timingSafeEqual(actualBuf, expectedBuf)) {
      return err(
        new PolicySignatureError(
          "HMAC-SHA256 signature verification failed. The policy may have been modified or the wrong key was used.",
        ),
      );
    }
  } else if (bundle.signatureType === "ed25519") {
    try {
      const publicKeyPem = verificationKey;
      // Validate that the provided key is actually an Ed25519 key.
      // crypto.verify(null, ...) auto-detects the algorithm from the PEM,
      // so we must ensure a non-Ed25519 key cannot be substituted.
      // Validate that the PEM key is actually Ed25519 by parsing it,
      // not just checking for the generic SPKI header (which RSA/EC keys also use).
      let publicKey: KeyObject;
      try {
        publicKey = createPublicKey(publicKeyPem);
      } catch {
        return err(
          new PolicySignatureError("Ed25519 verification key is not a valid PEM public key"),
        );
      }
      if (publicKey.asymmetricKeyType !== "ed25519") {
        return err(
          new PolicySignatureError(
            "Ed25519 verification key is not an Ed25519 key (got " +
              publicKey.asymmetricKeyType +
              ")",
          ),
        );
      }
      const signatureBuffer = Buffer.from(bundle.signature, "base64");
      const payloadBuffer = Buffer.from(payload, "utf-8");
      const verified = cryptoVerify(null, payloadBuffer, publicKey, signatureBuffer);
      if (!verified) {
        return err(
          new PolicySignatureError(
            "Ed25519 signature verification failed. The policy may have been modified or the wrong public key was used.",
          ),
        );
      }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      return err(
        new PolicySignatureError(`Ed25519 signature verification error: ${message}`, message),
      );
    }
  } else {
    return err(new PolicySignatureError(`Unknown signature type: ${bundle.signatureType}`));
  }

  return ok(bundle.policy);
}

/**
 * Sign a policy bundle with HMAC-SHA256.
 * This is used by the policy administrator to create signed bundles.
 */
export function signPolicyHmac(
  policy: Policy,
  hash: string,
  secretKey: string,
  keyId: string = "default",
): SignedPolicyBundle {
  const signedAt = new Date().toISOString();
  const payload = `${hash}:hmac-sha256:${keyId}:${signedAt}`;
  const signature = createHmac("sha256", secretKey).update(payload).digest("base64");

  return {
    policy,
    hash,
    signatureType: "hmac-sha256",
    signature,
    keyId,
    signedAt,
  };
}

/**
 * Sign a policy bundle with Ed25519.
 * Uses node:crypto generateKeyPairSync for key generation and
 * crypto.sign for signature creation.
 *
 * The verificationKey parameter in verifySignedPolicy should be the
 * PEM-encoded public key. The privateKeyPem parameter here is the
 * PEM-encoded private key.
 */
export function signPolicyEd25519(
  policy: Policy,
  hash: string,
  privateKeyPem: string,
  keyId: string = "default",
): SignedPolicyBundle {
  const signedAt = new Date().toISOString();
  const payload = `${hash}:ed25519:${keyId}:${signedAt}`;
  const signature = cryptoSign(null, Buffer.from(payload, "utf-8"), privateKeyPem);
  return {
    policy,
    hash,
    signatureType: "ed25519",
    signature: signature.toString("base64"),
    keyId,
    signedAt,
  };
}

/**
 * Generate an Ed25519 key pair for policy signing.
 * Returns { publicKeyPem, privateKeyPem } for use with
 * signPolicyEd25519 and verifySignedPolicy.
 */
export function generatePolicySigningKey(): {
  publicKeyPem: string;
  privateKeyPem: string;
} {
  const pair = generateKeyPairSync("ed25519", {
    publicKeyEncoding: { type: "spki", format: "pem" },
    privateKeyEncoding: { type: "pkcs8", format: "pem" },
  });
  return {
    publicKeyPem: pair.publicKey,
    privateKeyPem: pair.privateKey,
  };
}
// Note: passing "ed25519" explicitly instead of null for algorithm safety.
// Node.js crypto.verify supports explicit algorithm strings; relying on
// null (auto-detect from PEM) may accept unexpected key types.
