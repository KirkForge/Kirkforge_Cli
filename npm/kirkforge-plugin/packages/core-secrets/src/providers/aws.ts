import type { SecretsProvider } from "../manager.js";
import { awsSigV4Sign } from "../aws-sigv4.js";

export interface AwsSecretsConfig {
  /** AWS region. */
  region: string;
  /** Optional explicit credentials. Falls back to default credential chain. */
  accessKeyId?: string;
  secretAccessKey?: string;
  sessionToken?: string;
}

/**
 * Resolves secrets from AWS Secrets Manager using SigV4-signed requests.
 * Falls back to `process.env` for credentials when not provided in config.
 */
export class AwsSecretsProvider implements SecretsProvider {
  readonly name = "aws-secrets-manager";
  private config: AwsSecretsConfig;

  constructor(config: AwsSecretsConfig) {
    this.config = config;
  }

  async get(key: string): Promise<string | null> {
    // Without explicit credentials, AWS requests can't be signed.
    // Fall back to the default credential chain via process.env or
    // return null so the chain moves to the next provider.
    const accessKeyId = this.config.accessKeyId ?? process.env.AWS_ACCESS_KEY_ID;
    const secretAccessKey = this.config.secretAccessKey ?? process.env.AWS_SECRET_ACCESS_KEY;
    const sessionToken = this.config.sessionToken ?? process.env.AWS_SESSION_TOKEN;

    if (!accessKeyId || !secretAccessKey) {
      // No credentials available — skip this provider so the chain
      // can fall through to GCP or env vars.
      return null;
    }

    try {
      const host = `secretsmanager.${this.config.region}.amazonaws.com`;
      const url = `https://${host}/`;
      const body = JSON.stringify({ SecretId: key });

      const { headers } = awsSigV4Sign({
        method: "POST",
        host,
        region: this.config.region,
        service: "secretsmanager",
        body,
        accessKeyId,
        secretAccessKey,
        sessionToken: sessionToken ?? undefined,
      });

      const res = await fetch(url, {
        method: "POST",
        headers,
        body,
        signal: AbortSignal.timeout(5000),
      });

      if (!res.ok) return null;
      const data = (await res.json()) as {
        SecretString?: string;
        SecretBinary?: string;
      };

      if (data.SecretString) {
        // Try JSON first (key-value pairs), then plain string
        try {
          const kv = JSON.parse(data.SecretString) as Record<string, string>;
          if (typeof kv[key] === "string") return kv[key]!;
          if (typeof kv.value === "string") return kv.value;
        } catch {
          return data.SecretString;
        }
      }
      return null;
    } catch {
      return null;
    }
  }
}
