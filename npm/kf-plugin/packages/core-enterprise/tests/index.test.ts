import { describe, it, expect } from "vitest";
import {
  isEnterpriseMode,
  validateEnterpriseMode,
  requireEnterpriseOrDev,
  enterpriseStartupGate,
} from "../src/index.js";

describe("Enterprise mode detection", () => {
  it("returns false when env var is not set", () => {
    expect(isEnterpriseMode({})).toBe(false);
  });

  it("returns true when KIRKFORGE_ENTERPRISE_MODE=1", () => {
    expect(isEnterpriseMode({ KIRKFORGE_ENTERPRISE_MODE: "1" })).toBe(true);
  });

  it("returns true when KIRKFORGE_ENTERPRISE_MODE=true", () => {
    expect(isEnterpriseMode({ KIRKFORGE_ENTERPRISE_MODE: "true" })).toBe(true);
  });

  it("returns true when KIRKFORGE_ENTERPRISE_MODE=yes", () => {
    expect(isEnterpriseMode({ KIRKFORGE_ENTERPRISE_MODE: "yes" })).toBe(true);
  });

  it("returns false when KIRKFORGE_ENTERPRISE_MODE=0", () => {
    expect(isEnterpriseMode({ KIRKFORGE_ENTERPRISE_MODE: "0" })).toBe(false);
  });
});

describe("validateEnterpriseMode", () => {
  it("fails when auth is not configured", () => {
    const result = validateEnterpriseMode({
      KIRKFORGE_ENTERPRISE_MODE: "1",
      MEMORY_BACKEND: "sqlite",
      POLICY_FILE_PATH: "/policy.json",
      AUDIT_SINK_TYPE: "file",
      AUDIT_FILE_PATH: "/tmp/audit.jsonl",
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      const criticalAuth = result.error.violations.find((v) => v.control === "auth");
      expect(criticalAuth).toBeDefined();
      expect(criticalAuth!.severity).toBe("critical");
    }
  });

  it("passes when all critical controls are configured", () => {
    const result = validateEnterpriseMode({
      KIRKFORGE_ENTERPRISE_MODE: "1",
      HEALTH_API_KEY: "a".repeat(32),
      MEMORY_BACKEND: "sqlite",
      POLICY_FILE_PATH: "/policy.json",
      AUDIT_SINK_TYPE: "file",
      AUDIT_FILE_PATH: "/tmp/audit.jsonl",
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.enabled).toBe(true);
      expect(result.value.auth.configured).toBe(true);
      expect(result.value.storage.durable).toBe(true);
    }
  });

  it("fails when storage is not durable", () => {
    const result = validateEnterpriseMode({
      KIRKFORGE_ENTERPRISE_MODE: "1",
      HEALTH_API_KEY: "a".repeat(32),
      MEMORY_BACKEND: "memory",
      POLICY_FILE_PATH: "/policy.json",
      AUDIT_SINK_TYPE: "file",
      AUDIT_FILE_PATH: "/tmp/audit.jsonl",
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      const storage = result.error.violations.find((v) => v.control === "storage");
      expect(storage).toBeDefined();
      expect(storage!.severity).toBe("critical");
    }
  });

  it("fails when policy is not configured", () => {
    const result = validateEnterpriseMode({
      KIRKFORGE_ENTERPRISE_MODE: "1",
      HEALTH_API_KEY: "a".repeat(32),
      MEMORY_BACKEND: "sqlite",
      AUDIT_SINK_TYPE: "file",
      AUDIT_FILE_PATH: "/tmp/audit.jsonl",
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      const policy = result.error.violations.find((v) => v.control === "policy");
      expect(policy).toBeDefined();
    }
  });

  it("warns when secrets fall through to env-only", () => {
    const result = validateEnterpriseMode({
      KIRKFORGE_ENTERPRISE_MODE: "1",
      HEALTH_API_KEY: "a".repeat(32),
      MEMORY_BACKEND: "sqlite",
      POLICY_FILE_PATH: "/policy.json",
      AUDIT_SINK_TYPE: "file",
      AUDIT_FILE_PATH: "/tmp/audit.jsonl",
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.secrets.envOnlyFallback).toBe(true);
    }
  });

  it("accepts OIDC issuer as auth", () => {
    const result = validateEnterpriseMode({
      KIRKFORGE_ENTERPRISE_MODE: "1",
      OIDC_ISSUER: "https://auth.example.com",
      OIDC_AUDIENCE: "kirkforge",
      MEMORY_BACKEND: "sqlite",
      POLICY_FILE_PATH: "/policy.json",
      AUDIT_SINK_TYPE: "file",
      AUDIT_FILE_PATH: "/tmp/audit.jsonl",
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.auth.configured).toBe(true);
    }
  });
});

describe("requireEnterpriseOrDev", () => {
  it("returns dev config when enterprise mode is off", () => {
    const result = requireEnterpriseOrDev({});
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.enabled).toBe(false);
    }
  });

  it("validates when enterprise mode is on", () => {
    const result = requireEnterpriseOrDev({ KIRKFORGE_ENTERPRISE_MODE: "1" });
    expect(result.ok).toBe(false);
  });
});

describe("enterpriseStartupGate", () => {
  it("returns dev config in dev mode", () => {
    const config = enterpriseStartupGate(undefined, {});
    expect(config.enabled).toBe(false);
  });

  it("throws in enterprise mode with missing controls", () => {
    expect(() => enterpriseStartupGate(undefined, { KIRKFORGE_ENTERPRISE_MODE: "1" })).toThrow();
  });

  it("succeeds in enterprise mode with all controls", () => {
    const config = enterpriseStartupGate(undefined, {
      KIRKFORGE_ENTERPRISE_MODE: "1",
      HEALTH_API_KEY: "a".repeat(32),
      MEMORY_BACKEND: "sqlite",
      POLICY_FILE_PATH: "/policy.json",
      AUDIT_SINK_TYPE: "file",
      AUDIT_FILE_PATH: "/tmp/audit.jsonl",
    });
    expect(config.enabled).toBe(true);
  });
});
