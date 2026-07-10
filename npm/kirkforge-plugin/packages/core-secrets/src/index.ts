// Thin barrel — all implementation lives in sibling modules.

export type { SecretsProvider } from "./manager.js";
export { SecretsManager } from "./manager.js";

export { EnvSecretsProvider } from "./providers/env.js";

export type { VaultConfig } from "./providers/vault.js";
export { VaultSecretsProvider } from "./providers/vault.js";

export { sha256Hex, hmacSha256, awsSigV4Sign } from "./aws-sigv4.js";
export type { SigV4SignOptions } from "./aws-sigv4.js";

export type { AwsSecretsConfig } from "./providers/aws.js";
export { AwsSecretsProvider } from "./providers/aws.js";

export type { GcpSecretsConfig } from "./providers/gcp.js";
export { GcpSecretsProvider } from "./providers/gcp.js";

export { redactSecrets, redactSecretsDeep } from "./redaction.js";

export type { TenantKeyVersion, TenantKeyProviderConfig } from "./tenant-key-provider.js";
export { TenantKeyProvider } from "./tenant-key-provider.js";

export { createSecretsManager } from "./factory.js";
