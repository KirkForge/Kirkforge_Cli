import type { LintRule } from "@kirkforge/tool-lint-core";

export const safetyRules: LintRule[] = [
  // kirkforge-lint-disable no-eval no-hardcoded-openai-key
  {
    id: "no-eval-exec",
    category: "safety",
    severity: "critical",
    pattern: /\b(?:eval|exec)\s*\(/g,
    message:
      "eval()/exec() executes arbitrary code — use ast.literal_eval() for data, or restructure to avoid runtime code generation entirely",
  },
  {
    id: "no-os-system",
    category: "safety",
    severity: "high",
    pattern: /\bos\.system\s*\(/g,
    message:
      "os.system() passes a string to the shell — use subprocess.run(['cmd', 'arg']) with shell=False and an argument list",
  },
  {
    id: "no-subprocess-shell",
    category: "safety",
    severity: "high",
    pattern: /\bsubprocess\..*shell\s*=\s*True/g,
    message:
      "shell=True opens a shell and risks injection — use shell=False with a list of arguments: subprocess.run(['cmd', 'arg'])",
  },
  {
    id: "no-pickle",
    category: "safety",
    severity: "high",
    pattern: /\bpickle\.loads?\s*\(/g,
    message:
      "pickle deserialization is unsafe for untrusted data — use json, msgpack, or protobuf for serialization",
  },
  {
    id: "no-yaml-load",
    category: "safety",
    severity: "high",
    pattern: /\byaml\.load\s*\(/g,
    message:
      "yaml.load() can execute arbitrary code — use yaml.safe_load() instead; no known use case requires full yaml.load()",
  },
  {
    id: "no-request-verify-false",
    category: "safety",
    severity: "high",
    pattern: /\bverify\s*=\s*False/g,
    message:
      "SSL certificate verification disabled — never disable in production; for dev use a custom CA bundle or set REQUESTS_CA_BUNDLE",
  },
  {
    id: "no-hardcoded-password",
    category: "safety",
    severity: "high",
    pattern: /(?:password|passwd|secret|api_key|apikey|auth_token)\s*[:=]\s*['"][^'"]+['"]/gi,
    message:
      "Hardcoded credential — use os.environ.get('KEY'), python-dotenv, or a secrets manager (AWS Secrets Manager, Vault)",
  },
  {
    id: "no-hardcoded-token",
    category: "safety",
    severity: "critical",
    pattern:
      /(?:sk-[a-zA-Z0-9_-]{20,}|ghp_[a-zA-Z0-9]{36}|github_pat_[a-zA-Z0-9_]{36,}|glpat-[a-zA-Z0-9_-]{20,}|xox[bprs]-[a-zA-Z0-9-]+)/g,
    message:
      "Hardcoded API token (OpenAI/GitHub/GitLab/Slack) — move to environment variable or secrets manager; rotate this token immediately",
  },
  {
    id: "no-hardcoded-aws-key",
    category: "safety",
    severity: "high",
    pattern: /AKIA[0-9A-Z]{16}/g,
    message:
      "Hardcoded AWS access key — use boto3 default credential chain (env vars, IAM role, ~/.aws/credentials); rotate this key immediately",
  },
  {
    id: "no-hardcoded-stripe-live",
    category: "safety",
    severity: "critical",
    pattern: /sk_live_[a-zA-Z0-9]{24,}/g,
    message:
      "Hardcoded Stripe live secret key — move to STRIPE_SECRET_KEY env var; rotate immediately and check Stripe dashboard for unauthorized activity",
  },
  {
    id: "no-hardcoded-stripe-test",
    category: "safety",
    severity: "med",
    pattern: /sk_test_[a-zA-Z0-9]{24,}/g,
    message:
      "Hardcoded Stripe test key — use STRIPE_SECRET_KEY env var even in test; prevents accidental commit to public repos",
  },
  {
    id: "no-hardcoded-jwt",
    category: "safety",
    severity: "med",
    pattern: /eyJ[a-zA-Z0-9_-]{20,}\.[a-zA-Z0-9_-]{20,}\.[a-zA-Z0-9_-]{20,}/g,
    message:
      "Hardcoded JWT — generate JWTs at runtime from a signing key; if this is a test fixture, add a comment: # test fixture — not a real secret",
  },
  {
    id: "no-http-url",
    category: "safety",
    severity: "low",
    pattern: /http:\/\/[^\s'\"]+/g,
    message:
      "Plain HTTP URL — use HTTPS; for localhost/dev, use http://localhost explicitly or add a # no-verify comment",
  },
  {
    id: "no-sql-inject-fstring",
    category: "safety",
    severity: "critical",
    pattern: /f['\"]\s*(?:SELECT|INSERT|UPDATE|DELETE)\s+.*\{/gi,
    message:
      "SQL query built with f-string interpolation — use parameterized queries: cursor.execute('SELECT ... WHERE id = %s', (id,))",
  },
  // kirkforge-lint-enable no-eval no-hardcoded-openai-key
];
