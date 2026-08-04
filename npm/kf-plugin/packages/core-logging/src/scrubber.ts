// ── Log scrubber: redacts sensitive patterns before transport write ──────

const SECRET_PATTERNS: Array<{ pattern: RegExp; replacement: string }> = [
  // API keys (sk-..., sk-ant-..., etc.)
  { pattern: /\b(sk-[A-Za-z0-9_-]{20,})\b/g, replacement: "sk-***REDACTED***" },
  { pattern: /\b(sk-ant-[A-Za-z0-9_-]{20,})\b/g, replacement: "sk-ant-***REDACTED***" },
  // Bearer tokens
  { pattern: /Bearer\s+[A-Za-z0-9._~+/-]+=*/g, replacement: "Bearer ***REDACTED***" },
  // AWS-style keys
  { pattern: /\b(AKIA[0-9A-Z]{16})\b/g, replacement: "AKIA***REDACTED***" },
  // Generic key=value secrets
  {
    pattern: /(api_key|apikey|api-key|secret|password|token)\s*[:=]\s*[^\s,}"]+/gi,
    replacement: "$1=***REDACTED***",
  },
  // JWT tokens
  {
    pattern: /\beyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}\b/g,
    replacement: "***JWT-REDACTED***",
  },
  // Private key headers
  {
    pattern:
      /-----BEGIN (?:RSA |EC )?PRIVATE KEY-----[\s\S]*?-----END (?:RSA |EC )?PRIVATE KEY-----/g,
    replacement: "***PRIVATE-KEY-REDACTED***",
  },
];

/**
 * Scrub sensitive data from a string. Use before writing to logs or external sinks.
 */
export function scrubSecrets(text: string): string {
  let result = text;
  for (const { pattern, replacement } of SECRET_PATTERNS) {
    result = result.replace(pattern, replacement);
  }
  return result;
}
