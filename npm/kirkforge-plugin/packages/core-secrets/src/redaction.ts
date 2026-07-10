/**
 * Patterns that match common secret/key formats in strings.
 * These are intentionally broad to catch common secret shapes without
 * false positives on short random strings.
 */
const SECRET_PATTERNS: Array<{ pattern: RegExp; label: string }> = [
  // AWS access key IDs (AKIA...)
  { pattern: /AKIA[0-9A-Z]{16}/g, label: "aws_access_key_id" },
  // AWS secret access keys (40-char base64-ish after known prefix)
  { pattern: /\b[A-Za-z0-9/+=]{40}\b/g, label: "aws_secret_access_key" },
  // Generic API key patterns: key=xxx, api_key=xxx, apikey=xxx, token=xxx
  {
    pattern:
      /(?:api[_-]?key|apikey|secret[_-]?key|access[_-]?key|auth[_-]?token|bearer[_-]?token|private[_-]?key)\s*[=:]\s*["']?([A-Za-z0-9_\-./+=]{8,})["']?/gi,
    label: "api_key_value",
  },
  // Bearer tokens in Authorization headers
  { pattern: /Bearer\s+[A-Za-z0-9\-._~+/]+=*/g, label: "bearer_token" },
  // Vault tokens (s.xxxxx or hvs.xxxxx)
  { pattern: /(?:s\.|hvs\.)[A-Za-z0-9]{24}/g, label: "vault_token" },
  // Connection strings with passwords
  { pattern: /:\/\/[^:]+:[^@]+@/g, label: "connection_string_password" },
  // Private key blocks
  {
    pattern:
      /-----BEGIN\s+(?:RSA\s+)?PRIVATE\s+KEY-----[\s\S]*?-----END\s+(?:RSA\s+)?PRIVATE\s+KEY-----/g,
    label: "private_key",
  },
  // JWT tokens (three base64url segments separated by dots)
  { pattern: /eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g, label: "jwt" },
  // Generic long hex strings (32+ chars that look like tokens/keys)
  { pattern: /\b[0-9a-fA-F]{32,}\b/g, label: "hex_secret" },
  // Environment variable assignments for common secret names
  {
    pattern:
      /(?:PASSWORD|SECRET|TOKEN|API_KEY|PRIVATE_KEY|AUTH|CREDENTIAL)\s*[=:]\s*["']?([^\s"']{8,})["']?/gi,
    label: "env_secret",
  },
];

/**
 * Redact secrets from a string, replacing matches with [REDACTED_label].
 *
 * This is used to sanitize logs, error messages, audit trails, and tool output
 * before they are persisted or transmitted.
 *
 * @param input - The string to redact.
 * @param options - Optional overrides for which labels to redact and replacement text.
 * @returns The redacted string.
 */
export function redactSecrets(
  input: string,
  options?: {
    /** Specific labels to redact. If omitted, all patterns are applied. */
    labels?: string[];
    /** Replacement text format. Default: "[REDACTED_{label}]". */
    replacement?: (label: string) => string;
  },
): string {
  const labels = options?.labels;
  const replacement = options?.replacement ?? ((label: string) => `[REDACTED_${label}]`);

  let result = input;
  for (const { pattern, label } of SECRET_PATTERNS) {
    if (labels && !labels.includes(label)) continue;
    // Reset regex lastIndex for global patterns
    pattern.lastIndex = 0;
    result = result.replace(pattern, (_match) => replacement(label));
  }
  return result;
}

/**
 * Redact secrets from an arbitrary JSON-serializable value.
 * Recursively walks objects and arrays, redacting all string values.
 *
 * @param value - The value to redact.
 * @param options - Same options as redactSecrets.
 * @returns A deep copy of the value with secrets redacted.
 */
export function redactSecretsDeep(
  value: unknown,
  options?: Parameters<typeof redactSecrets>[1],
): unknown {
  if (typeof value === "string") {
    return redactSecrets(value, options);
  }
  if (Array.isArray(value)) {
    return value.map((item) => redactSecretsDeep(item, options));
  }
  if (value !== null && typeof value === "object") {
    const result: Record<string, unknown> = {};
    for (const [key, val] of Object.entries(value)) {
      // Also redact values whose keys look like secret names
      if (
        typeof val === "string" &&
        /(?:password|secret|token|key|auth|credential|private)/i.test(key)
      ) {
        result[key] = "[REDACTED]";
      } else {
        result[key] = redactSecretsDeep(val, options);
      }
    }
    return result;
  }
  return value;
}
