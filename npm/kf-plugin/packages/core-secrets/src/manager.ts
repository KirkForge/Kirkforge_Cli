import { ok, err, type Result } from "@kirkforge/core-types";

/**
 * Abstract secrets resolution. Each provider maps string keys to secret values.
 * Providers are tried in priority order; the first non-null result wins.
 */
export interface SecretsProvider {
  /** Human-readable name for logging/debugging. */
  readonly name: string;
  /** Resolve a secret by key. Returns null if the key is unknown to this provider. */
  get(key: string): Promise<string | null>;
}

/**
 * Chains multiple secrets providers. `get` returns the first non-null result
 * across all providers; `require` returns an `err` if none can satisfy the key.
 */
export class SecretsManager {
  private providers: SecretsProvider[];

  constructor(providers: SecretsProvider[]) {
    this.providers = providers;
  }

  /**
   * Resolve a secret key across all providers in priority order.
   * Returns the first non-null result, or null if no provider knows the key.
   */
  async get(key: string): Promise<string | null> {
    for (const p of this.providers) {
      try {
        const value = await p.get(key);
        if (value !== null) return value;
      } catch {
        // Provider failed — skip to next
      }
    }
    return null;
  }

  /**
   * Resolve a required secret. Returns err if no provider returns a value.
   */
  async require(key: string): Promise<Result<string, Error>> {
    const value = await this.get(key);
    if (value === null) {
      return err(new Error(`Secret "${key}" not found in any provider`));
    }
    return ok(value);
  }
}
