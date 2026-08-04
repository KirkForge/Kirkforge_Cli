import type { SecretsProvider } from "../manager.js";

/**
 * Reads secrets from environment variables. The fallback provider in any
 * chain — env vars are checked last so more authoritative sources (Vault,
 * AWS, GCP) can win when they know a key.
 */
export class EnvSecretsProvider implements SecretsProvider {
  readonly name = "env";
  private env: Record<string, string | undefined>;

  constructor(env?: Record<string, string | undefined>) {
    this.env = env ?? (process.env as Record<string, string | undefined>);
  }

  async get(key: string): Promise<string | null> {
    return this.env[key] ?? null;
  }
}
