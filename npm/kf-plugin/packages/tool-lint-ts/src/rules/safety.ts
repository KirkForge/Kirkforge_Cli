import type { LintRule } from "@kirkforge/tool-lint-core";

// kirkforge-lint-disable no-eval no-new-func no-hardcoded-openai-key no-dynamic-require
export const safetyRules: LintRule[] = [
  {
    id: "no-eval",
    category: "safety",
    severity: "critical",
    pattern: /\beval\s*\(/g,
    message:
      "eval() executes arbitrary code — use JSON.parse() for data, a sandboxed VM for dynamic scripts, or restructure to avoid runtime code generation",
  },
  {
    id: "no-implied-eval",
    category: "safety",
    severity: "high",
    pattern: /\b(setTimeout|setInterval)\s*\(\s*['"\`]/g,
    message:
      "String argument to setTimeout/setInterval is implied eval — pass a function reference instead: setTimeout(() => { ... }, ms)",
  },
  {
    id: "no-new-func",
    category: "safety",
    severity: "high",
    pattern: /\bnew\s+Function\s*\(/g,
    message:
      "new Function() compiles strings to executable code — use a regular function, closure, or strategy pattern instead",
  },
  {
    id: "no-process-env",
    category: "safety",
    severity: "low",
    pattern: /\bprocess\.env\.\w+/g,
    message:
      "Direct process.env access — consider centralizing via a typed config loader or secrets service",
  },
  {
    id: "no-dynamic-require",
    category: "safety",
    severity: "low",
    pattern: /\brequire\s*\(\s*[^'"\s]/g,
    message:
      "require() with a non-literal argument — use a static import or a loader map with known paths",
  },
  {
    id: "no-unsafe-regex",
    category: "safety",
    severity: "med",
    pattern: /\(\w+\+\)\+/g,
    message:
      "Potentially unsafe regex with nested quantifiers (ReDoS risk) — add a length limit, use atomic groups, or validate input size before matching",
  },
  {
    id: "no-shell-exec",
    category: "safety",
    severity: "high",
    pattern: /exec\s*\(\s*['"\`][^'"]*\$\{?[^}]*\}?[^'"]*['"\`]\s*[,)]/g,
    message:
      "Shell command built with string interpolation — use execFile() with a static command and an arguments array to prevent injection",
  },
  {
    id: "no-sql-inject",
    category: "safety",
    severity: "critical",
    pattern: /\`\s*(?:SELECT|INSERT|UPDATE|DELETE)\s+[^`]*\$\{[^}]+\}[^`]*\`(?![^;]*\?)/gi,
    message:
      "SQL query built with template literal interpolation — use parameterized queries ($1, ?) or a query builder (knex, kysely, drizzle)",
  },
  {
    id: "no-http-url",
    category: "safety",
    severity: "low",
    pattern: /http:\/\/[^\s'"]+/g,
    message:
      "Plain HTTP URL — use HTTPS; if this is a local/dev endpoint, add an explicit allowlist or comment",
  },
  {
    id: "no-hardcoded-aws-key",
    category: "safety",
    severity: "high",
    pattern: /AKIA[0-9A-Z]{16}/g,
    message:
      "Hardcoded AWS access key — move to environment variable (AWS_ACCESS_KEY_ID), IAM role, or secrets manager; rotate this key immediately",
  },
  {
    id: "no-hardcoded-openai-key",
    category: "safety",
    severity: "critical",
    pattern: /sk-[a-zA-Z0-9_-]{20,}/g,
    message:
      "Hardcoded OpenAI API key — move to OPENAI_API_KEY environment variable or encrypted config; rotate this key immediately",
  },
  {
    id: "no-hardcoded-gh-token",
    category: "safety",
    severity: "high",
    pattern: /ghp_[a-zA-Z0-9]{36}/g,
    message:
      "Hardcoded GitHub personal access token — use GITHUB_TOKEN env var, GitHub App installation token, or OIDC; rotate immediately",
  },
  {
    id: "no-hardcoded-gh-new",
    category: "safety",
    severity: "high",
    pattern: /github_pat_[a-zA-Z0-9_]{36,}/g,
    message:
      "Hardcoded GitHub fine-grained PAT — use GITHUB_TOKEN env var or OIDC; rotate immediately",
  },
  {
    id: "no-hardcoded-gitlab-token",
    category: "safety",
    severity: "high",
    pattern: /glpat-[a-zA-Z0-9_-]{20,}/g,
    message:
      "Hardcoded GitLab personal access token — move to GITLAB_TOKEN env var or CI/CD variables; rotate immediately",
  },
  {
    id: "no-hardcoded-stripe-live",
    category: "safety",
    severity: "critical",
    pattern: /sk_live_[a-zA-Z0-9]{24,}/g,
    message:
      "Hardcoded Stripe live secret key — move to STRIPE_SECRET_KEY env var; rotate this key immediately and check Stripe logs for unauthorized use",
  },
  {
    id: "no-hardcoded-stripe-test",
    category: "safety",
    severity: "med",
    pattern: /sk_test_[a-zA-Z0-9]{24,}/g,
    message:
      "Hardcoded Stripe test key — move to STRIPE_SECRET_KEY env var even in dev; prevents accidental commit of test credentials",
  },
  {
    id: "no-hardcoded-slack-token",
    category: "safety",
    severity: "high",
    pattern: /xox[bprs]-[a-zA-Z0-9-]+/g,
    message:
      "Hardcoded Slack token — move to SLACK_BOT_TOKEN or SLACK_APP_TOKEN env var; rotate immediately",
  },
  {
    id: "no-hardcoded-jwt",
    category: "safety",
    severity: "med",
    pattern: /eyJ[a-zA-Z0-9_-]{20,}\.[a-zA-Z0-9_-]{20,}\.[a-zA-Z0-9_-]{20,}/g,
    message:
      "Hardcoded JWT — JWTs should be generated at runtime from secrets, never committed; if this is a test fixture, add a comment marking it as non-sensitive",
  },
];
// kirkforge-lint-enable no-eval no-new-func no-hardcoded-openai-key no-dynamic-require
