# @kirkforge/core-secrets

Chained secrets resolution: Vault → AWS → GCP → environment. Implements the secrets chain defined in the architecture with failover between providers.

## Key exports

- `resolveSecret(key)` — resolve a secret through the chain
- `SecretsConfig` — provider configuration
