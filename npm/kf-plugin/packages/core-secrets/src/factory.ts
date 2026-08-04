import type { SecretsProvider } from "./manager.js";
import { SecretsManager } from "./manager.js";
import { EnvSecretsProvider } from "./providers/env.js";
import { VaultSecretsProvider } from "./providers/vault.js";
import { AwsSecretsProvider } from "./providers/aws.js";
import { GcpSecretsProvider } from "./providers/gcp.js";

/**
 * Build a SecretsManager from environment variables.
 * Detects which providers are configured and chains them in order:
 * 1. Vault (if VAULT_ADDR + VAULT_TOKEN set)
 * 2. AWS (if AWS_REGION set)
 * 3. GCP (if GCP_PROJECT_ID set)
 * 4. Env vars (always last as fallback)
 */
export function createSecretsManager(env?: Record<string, string | undefined>): SecretsManager {
  const e = env ?? (process.env as Record<string, string | undefined>);
  const providers: SecretsProvider[] = [];

  // Vault
  if (e.VAULT_ADDR && e.VAULT_TOKEN) {
    providers.push(
      new VaultSecretsProvider({
        address: e.VAULT_ADDR,
        token: e.VAULT_TOKEN,
        mount: e.VAULT_MOUNT,
        prefix: e.VAULT_SECRET_PREFIX,
      }),
    );
  }

  // AWS — only activate when both region and credentials are present
  if (e.AWS_REGION && e.AWS_ACCESS_KEY_ID && e.AWS_SECRET_ACCESS_KEY) {
    providers.push(
      new AwsSecretsProvider({
        region: e.AWS_REGION,
        accessKeyId: e.AWS_ACCESS_KEY_ID,
        secretAccessKey: e.AWS_SECRET_ACCESS_KEY,
        sessionToken: e.AWS_SESSION_TOKEN,
      }),
    );
  }

  // GCP — only activate when project and credentials (token or key file) are present
  if (e.GCP_PROJECT_ID && (e.GCP_ACCESS_TOKEN || e.GOOGLE_APPLICATION_CREDENTIALS)) {
    providers.push(
      new GcpSecretsProvider({
        projectId: e.GCP_PROJECT_ID,
        accessToken: e.GCP_ACCESS_TOKEN,
        credentialsFile: e.GOOGLE_APPLICATION_CREDENTIALS,
      }),
    );
  }

  // Env vars (always last)
  providers.push(new EnvSecretsProvider(e));

  return new SecretsManager(providers);
}
