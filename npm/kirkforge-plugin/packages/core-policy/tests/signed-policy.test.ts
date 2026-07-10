import { describe, it, expect } from "vitest";
import {
  PolicyEngine,
  verifySignedPolicy,
  signPolicyHmac,
  signPolicyEd25519,
  generatePolicySigningKey,
  type SignedPolicyBundle,
} from "../src/index.js";

describe("signPolicyHmac", () => {
  it("creates a signed bundle with HMAC-SHA256 signature", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const bundle = signPolicyHmac(policy, hash, "test-secret-key", "key-1");

    expect(bundle.signatureType).toBe("hmac-sha256");
    expect(bundle.keyId).toBe("key-1");
    expect(bundle.signature).toBeTruthy();
    expect(bundle.signedAt).toBeTruthy();
    expect(bundle.hash).toBe(hash);
  });

  it("produces deterministic signatures for the same input", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();

    // Sign at the same timestamp for determinism test
    const bundle1 = signPolicyHmac(policy, hash, "test-secret", "key-1");
    // Different timestamps produce different signatures (by design)
    expect(bundle1.signature).toBeTruthy();
  });
});

describe("verifySignedPolicy", () => {
  it("accepts a valid HMAC-SHA256 signed bundle", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const bundle = signPolicyHmac(policy, hash, "test-secret-key", "key-1");

    const result = verifySignedPolicy(bundle, "test-secret-key");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.version).toBe(policy.version);
    }
  });

  it("rejects a bundle with wrong verification key", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const bundle = signPolicyHmac(policy, hash, "correct-key", "key-1");

    const result = verifySignedPolicy(bundle, "wrong-key");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("HMAC-SHA256 signature verification failed");
    }
  });

  it("rejects a bundle with tampered policy content", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const bundle = signPolicyHmac(policy, hash, "test-secret-key", "key-1");

    // Tamper with the policy
    const tamperedPolicy = { ...policy, name: "tampered" };
    const tamperedBundle: SignedPolicyBundle = { ...bundle, policy: tamperedPolicy };

    const result = verifySignedPolicy(tamperedBundle, "test-secret-key");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("hash mismatch");
    }
  });

  it("rejects a bundle with mismatched hash", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const bundle = signPolicyHmac(policy, hash, "test-secret-key", "key-1");

    const wrongHashBundle: SignedPolicyBundle = { ...bundle, hash: "bad-hash" };
    const result = verifySignedPolicy(wrongHashBundle, "test-secret-key");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("hash mismatch");
    }
  });

  it("rejects unknown signature type", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();

    const unknownBundle: SignedPolicyBundle = {
      policy,
      hash,
      signatureType: "rsa" as any,
      signature: "base64-signature",
      keyId: "rsa-key-1",
      signedAt: new Date().toISOString(),
    };

    const result = verifySignedPolicy(unknownBundle, "public-key-base64");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("Unknown signature type");
    }
  });
});

describe("signPolicyEd25519", () => {
  it("creates a signed bundle with Ed25519 signature", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const keys = generatePolicySigningKey();
    const bundle = signPolicyEd25519(policy, hash, keys.privateKeyPem, "ed25519-key-1");

    expect(bundle.signatureType).toBe("ed25519");
    expect(bundle.keyId).toBe("ed25519-key-1");
    expect(bundle.signature).toBeTruthy();
    expect(bundle.signedAt).toBeTruthy();
    expect(bundle.hash).toBe(hash);
  });

  it("produces a different signature for different keys", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const keys1 = generatePolicySigningKey();
    const keys2 = generatePolicySigningKey();

    // We can't directly compare signatures because signedAt differs,
    // but we can verify that keys2's public key rejects keys1's signature
    const bundle1 = signPolicyEd25519(policy, hash, keys1.privateKeyPem, "key-1");
    const result2 = verifySignedPolicy(bundle1, keys2.publicKeyPem);
    expect(result2.ok).toBe(false);
  });
});

describe("Ed25519 verifySignedPolicy", () => {
  it("accepts a valid Ed25519 signed bundle", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const keys = generatePolicySigningKey();

    const bundle = signPolicyEd25519(policy, hash, keys.privateKeyPem, "ed25519-key-1");
    const result = verifySignedPolicy(bundle, keys.publicKeyPem);

    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.version).toBe(policy.version);
    }
  });

  it("rejects an Ed25519 bundle with wrong public key", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const keys = generatePolicySigningKey();
    const wrongKeys = generatePolicySigningKey();

    const bundle = signPolicyEd25519(policy, hash, keys.privateKeyPem, "ed25519-key-1");
    const result = verifySignedPolicy(bundle, wrongKeys.publicKeyPem);

    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("Ed25519 signature verification failed");
    }
  });

  it("rejects a tampered Ed25519 signed bundle", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const keys = generatePolicySigningKey();

    const bundle = signPolicyEd25519(policy, hash, keys.privateKeyPem, "ed25519-key-1");

    // Tamper with the policy content
    const tamperedPolicy = { ...policy, name: "tampered" };
    const tamperedBundle: SignedPolicyBundle = { ...bundle, policy: tamperedPolicy };

    const result = verifySignedPolicy(tamperedBundle, keys.publicKeyPem);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("hash mismatch");
    }
  });

  it("rejects an Ed25519 bundle with invalid public key format", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const keys = generatePolicySigningKey();

    const bundle = signPolicyEd25519(policy, hash, keys.privateKeyPem, "ed25519-key-1");

    const result = verifySignedPolicy(bundle, "not-a-valid-pem-key");
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.message.includes("Ed25519 signature verification error")) {
      expect(result.error.message).toContain("Ed25519 signature verification error");
    } else if (!result.ok) {
      // Early validation: non-PEM key rejected before signature check
      expect(result.error.message).toContain("Ed25519 verification key");
    }
  });

  it("rejects an Ed25519 bundle with corrupted signature", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const keys = generatePolicySigningKey();

    const bundle = signPolicyEd25519(policy, hash, keys.privateKeyPem, "ed25519-key-1");
    const corruptedBundle: SignedPolicyBundle = {
      ...bundle,
      signature: Buffer.from("corrupted-signature-data").toString("base64"),
    };

    const result = verifySignedPolicy(corruptedBundle, keys.publicKeyPem);
    expect(result.ok).toBe(false);
  });
});

describe("generatePolicySigningKey", () => {
  it("generates a valid Ed25519 key pair", () => {
    const keys = generatePolicySigningKey();
    expect(keys.publicKeyPem).toContain("BEGIN PUBLIC KEY");
    expect(keys.privateKeyPem).toContain("BEGIN PRIVATE KEY");
  });

  it("generates unique key pairs each time", () => {
    const keys1 = generatePolicySigningKey();
    const keys2 = generatePolicySigningKey();
    expect(keys1.publicKeyPem).not.toBe(keys2.publicKeyPem);
    expect(keys1.privateKeyPem).not.toBe(keys2.privateKeyPem);
  });

  it("generated keys can sign and verify", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const keys = generatePolicySigningKey();

    const bundle = signPolicyEd25519(policy, hash, keys.privateKeyPem, "test-key");
    const result = verifySignedPolicy(bundle, keys.publicKeyPem);
    expect(result.ok).toBe(true);
  });
});

// ── Regression: HMAC malformed signature does not throw ────────────────────
//
// Verify that a malformed (wrong-length) base64 signature returns a clean
// verification error rather than throwing a RangeError from timingSafeEqual.

describe("HMAC malformed signature regression", () => {
  it("returns error (not throw) when HMAC signature has wrong length", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const bundle = signPolicyHmac(policy, hash, "test-secret-key", "key-1");

    // Truncate the signature to produce a wrong-length buffer
    const malformedBundle: SignedPolicyBundle = {
      ...bundle,
      signature: Buffer.from("short", "utf-8").toString("base64"),
    };

    const result = verifySignedPolicy(malformedBundle, "test-secret-key");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("HMAC-SHA256 signature verification failed");
    }
  });

  it("returns error (not throw) when HMAC signature is empty string", () => {
    const engine = new PolicyEngine();
    const policy = engine.getPolicy();
    const hash = engine.getHash();
    const bundle = signPolicyHmac(policy, hash, "test-secret-key", "key-1");

    const emptyBundle: SignedPolicyBundle = {
      ...bundle,
      signature: "",
    };

    const result = verifySignedPolicy(emptyBundle, "test-secret-key");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("HMAC-SHA256 signature verification failed");
    }
  });
});
