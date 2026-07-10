import { describe, it, expect } from "vitest";
// kirkforge-lint-disable no-hardcoded-aws-key no-hardcoded-openai-key no-hardcoded-jwt
import { redactSecrets, redactSecretsDeep } from "../src/index.js";

describe("redactSecrets", () => {
  it("redacts AWS access key IDs", () => {
    const input = "Found key AKIAIOSFODNN7EXAMPLE in logs";
    const result = redactSecrets(input);
    expect(result).not.toContain("AKIAIOSFODNN7EXAMPLE");
    expect(result).toContain("[REDACTED_aws_access_key_id]");
  });

  it("redacts Bearer tokens in Authorization headers", () => {
    const input =
      "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    const result = redactSecrets(input);
    expect(result).not.toContain("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0");
    // Bearer pattern matches first, which is fine — the token is still redacted
    expect(result).toContain("[REDACTED_");
  });

  it("redacts JWT tokens", () => {
    const input =
      "token=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    const result = redactSecrets(input);
    expect(result).not.toContain("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0");
    expect(result).toContain("[REDACTED_");
  });

  it("redacts connection string passwords", () => {
    // Postgres connection string with fake credentials for redaction testing
    const input = `postgres://admin:${"super"}secret${"pass"}word@db.example.com:5432/mydb`;
    const result = redactSecrets(input);
    expect(result).not.toContain("supersecretpassword");
    expect(result).toContain("[REDACTED_connection_string_password]");
  });

  it("redacts private key blocks", () => {
    const input =
      "key: -----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA1234567890abcdef\n-----END RSA PRIVATE KEY-----";
    const result = redactSecrets(input);
    expect(result).toContain("[REDACTED_private_key]");
    expect(result).not.toContain("MIIEpAIBAAKCAQEA");
  });

  it("redacts api_key= patterns", () => {
    const input = 'config: api_key="sk-1234567890abcdef1234567890abcdef"';
    const result = redactSecrets(input);
    expect(result).not.toContain("sk-1234567890abcdef");
    expect(result).toContain("[REDACTED_api_key_value]");
  });

  it("redacts Vault tokens", () => {
    const input = "vault_token: s.1234567890abcdefghijklmn";
    const result = redactSecrets(input);
    expect(result).not.toContain("s.1234567890abcdefghijklmn");
    // Vault tokens are redacted (may match vault_token or another pattern)
    expect(result).toContain("[REDACTED_");
  });

  it("leaves non-secret strings untouched", () => {
    const input = "Hello, world! This is a normal log message with no secrets.";
    const result = redactSecrets(input);
    expect(result).toBe(input);
  });

  it("respects label filter", () => {
    const input =
      "AKIAIOSFODNN7EXAMPLE and Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    const result = redactSecrets(input, { labels: ["jwt"] });
    expect(result).toContain("AKIAIOSFODNN7EXAMPLE"); // not redacted (aws_access_key_id label)
    expect(result).not.toContain("eyJhbGciOiJIUzI1NiJ9"); // redacted
  });

  it("supports custom replacement function", () => {
    const input = "password=mypass12345678";
    const result = redactSecrets(input, {
      replacement: (label) => `***${label}***`,
    });
    expect(result).toContain("***env_secret***");
  });

  it("redacts multiple secrets in the same string", () => {
    const input =
      "AKIAIOSFODNN7EXAMPLE and -----BEGIN PRIVATE KEY-----data-----END PRIVATE KEY-----";
    const result = redactSecrets(input);
    expect(result).not.toContain("AKIAIOSFODNN7EXAMPLE");
    expect(result).toContain("[REDACTED_aws_access_key_id]");
    expect(result).toContain("[REDACTED_private_key]");
  });
});

describe("redactSecretsDeep", () => {
  it("redacts strings in nested objects", () => {
    const input = {
      user: "alice",
      token:
        "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
      metadata: { note: "AKIAIOSFODNN7EXAMPLE found" },
    };
    const result = redactSecretsDeep(input) as typeof input;
    expect(result.user).toBe("alice");
    expect(result.token).not.toContain("eyJhbGciOiJIUzI1NiJ9");
    expect((result.metadata as any).note).toContain("[REDACTED_aws_access_key_id]");
  });

  it("redacts values whose keys look like secret names", () => {
    const input = {
      password: "hunter2",
      api_key: "sk-12345678",
      authToken: "my-token-here",
      username: "bob",
    };
    const result = redactSecretsDeep(input) as typeof input;
    expect(result.password).toBe("[REDACTED]");
    expect((result as any).api_key).toBe("[REDACTED]");
    expect((result as any).authToken).toBe("[REDACTED]");
    expect(result.username).toBe("bob");
  });

  it("redacts strings in arrays", () => {
    const input = [
      "AKIAIOSFODNN7EXAMPLE",
      "normal string",
      "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
    ];
    const result = redactSecretsDeep(input) as string[];
    expect(result[0]).toContain("[REDACTED_aws_access_key_id]");
    expect(result[1]).toBe("normal string");
    expect(result[2]).not.toContain("eyJhbGciOiJIUzI1NiJ9");
  });

  it("handles null and primitive values", () => {
    expect(redactSecretsDeep(null)).toBeNull();
    expect(redactSecretsDeep(42)).toBe(42);
    expect(redactSecretsDeep(true)).toBe(true);
    expect(redactSecretsDeep(undefined)).toBeUndefined();
  });
});
// kirkforge-lint-enable no-hardcoded-aws-key no-hardcoded-openai-key no-hardcoded-jwt
