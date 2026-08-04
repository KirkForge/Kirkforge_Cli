import type { SecretsProvider } from "../manager.js";

export interface VaultConfig {
  /** Vault server URL (e.g. https://vault.example.com:8200). */
  address: string;
  /** Vault token for authentication. */
  token: string;
  /** KV v2 mount path (default: "secret"). */
  mount?: string;
  /** Key path prefix to prepend to all lookups. */
  prefix?: string;
}

/**
 * Resolves secrets from HashiCorp Vault's KV v2 engine. Each path segment is
 * URL-encoded separately so `/` is preserved as the path separator.
 */
export class VaultSecretsProvider implements SecretsProvider {
  readonly name = "vault";
  private config: Required<VaultConfig>;

  constructor(config: VaultConfig) {
    this.config = {
      address: config.address.replace(/\/$/, ""),
      token: config.token,
      mount: config.mount ?? "secret",
      prefix: config.prefix ?? "",
    };
  }

  async get(key: string): Promise<string | null> {
    const fullPath = this.config.prefix ? `${this.config.prefix}/${key}` : key;
    // KV v2: encode each path segment separately, preserving / as path separators
    const encodedPath = fullPath
      .split("/")
      .map((s) => encodeURIComponent(s))
      .join("/");
    const url = `${this.config.address}/v1/${this.config.mount}/data/${encodedPath}`;

    try {
      const res = await fetch(url, {
        headers: { "X-Vault-Token": this.config.token },
        signal: AbortSignal.timeout(5000),
      });
      if (!res.ok) return null;
      const body = (await res.json()) as {
        data?: { data?: Record<string, string> };
      };
      const secretData = body?.data?.data;
      if (!secretData) return null;

      // Vault KV v2 stores key-value pairs. Try direct key lookup first,
      // then fall back to "value" field convention.
      if (typeof secretData[key] === "string") return secretData[key]!;
      if (typeof secretData.value === "string") return secretData.value;
      return null;
    } catch {
      return null;
    }
  }
}
