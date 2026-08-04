import { describe, it, expect } from "vitest";
import {
  validateConstraints,
  mergeConstraints,
  isCommandAllowed,
  isNetworkDestinationAllowed,
  isReadPathAllowed,
  isWritePathAllowed,
  createSandboxContext,
  DEFAULT_CONSTRAINTS,
  type SandboxConstraints,
} from "../src/index.js";

describe("validateConstraints", () => {
  it("accepts default constraints", () => {
    const result = validateConstraints({});
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.maxTimeMs).toBe(60000);
      expect(result.value.networkAllowed).toBe(false);
      expect(result.value.shellAllowed).toBe(false);
    }
  });

  it("accepts valid custom constraints", () => {
    const result = validateConstraints({ maxTimeMs: 10000, maxMemoryMb: 256 });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.maxTimeMs).toBe(10000);
      expect(result.value.maxMemoryMb).toBe(256);
    }
  });

  it("rejects maxTimeMs below 1000", () => {
    const result = validateConstraints({ maxTimeMs: 500 });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.message).toContain("maxTimeMs");
  });

  it("rejects maxMemoryMb below 16", () => {
    const result = validateConstraints({ maxMemoryMb: 8 });
    expect(result.ok).toBe(false);
  });

  it("rejects maxCpuMs below 1000", () => {
    const result = validateConstraints({ maxCpuMs: 100 });
    expect(result.ok).toBe(false);
  });

  it("rejects networkAllowlist when network not allowed", () => {
    const result = validateConstraints({
      networkAllowed: false,
      networkAllowlist: ["example.com:443"],
    });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.message).toContain("networkAllowlist");
  });

  it("rejects allowedCommands when shell not allowed", () => {
    const result = validateConstraints({ shellAllowed: false, allowedCommands: ["ls"] });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.message).toContain("allowedCommands");
  });

  it("rejects overlapping allowlist and denylist", () => {
    const result = validateConstraints({
      networkAllowed: true,
      networkAllowlist: ["evil.com:443"],
      networkDenylist: ["evil.com:443"],
    });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.message).toContain("overlap");
  });
});

describe("mergeConstraints", () => {
  it("tenant can tighten but not loosen base constraints", () => {
    const merged = mergeConstraints(DEFAULT_CONSTRAINTS, { maxTimeMs: 30000 });
    expect(merged.maxTimeMs).toBe(30000);
  });

  it("tenant cannot increase time beyond base", () => {
    const merged = mergeConstraints(DEFAULT_CONSTRAINTS, { maxTimeMs: 120000 });
    expect(merged.maxTimeMs).toBe(60000); // capped at base
  });

  it("tenant cannot enable network when base denies", () => {
    const merged = mergeConstraints(DEFAULT_CONSTRAINTS, { networkAllowed: true });
    expect(merged.networkAllowed).toBe(false);
  });

  it("tenant cannot enable shell when base denies", () => {
    const merged = mergeConstraints(DEFAULT_CONSTRAINTS, { shellAllowed: true });
    expect(merged.shellAllowed).toBe(false);
  });

  it("tenant denylist entries are merged with base", () => {
    const base: SandboxConstraints = {
      ...DEFAULT_CONSTRAINTS,
      networkAllowed: true,
      networkDenylist: ["malware.com"],
    };
    const merged = mergeConstraints(base as Required<SandboxConstraints>, {
      networkDenylist: ["phishing.com"],
    });
    expect(merged.networkDenylist).toContain("malware.com");
    expect(merged.networkDenylist).toContain("phishing.com");
  });

  it("tenant cannot disable secret redaction", () => {
    const merged = mergeConstraints(DEFAULT_CONSTRAINTS, { redactSecrets: false });
    expect(merged.redactSecrets).toBe(true);
  });
});

describe("isCommandAllowed", () => {
  const constraints: Required<SandboxConstraints> = {
    ...DEFAULT_CONSTRAINTS,
    shellAllowed: true,
    allowedCommands: ["git", "node", "npm"],
  };

  it("allows commands in the allowlist", () => {
    expect(isCommandAllowed("git", constraints)).toBe(true);
    expect(isCommandAllowed("node", constraints)).toBe(true);
  });

  it("denies commands not in the allowlist", () => {
    expect(isCommandAllowed("rm", constraints)).toBe(false);
    expect(isCommandAllowed("bash", constraints)).toBe(false);
  });

  it("denies all commands when shell is not allowed", () => {
    const noShell = { ...constraints, shellAllowed: false };
    expect(isCommandAllowed("git", noShell)).toBe(false);
  });

  it("denies all commands when allowlist is empty", () => {
    const emptyList = { ...constraints, allowedCommands: [] };
    expect(isCommandAllowed("git", emptyList)).toBe(false);
  });
});

describe("isNetworkDestinationAllowed", () => {
  const constraints: Required<SandboxConstraints> = {
    ...DEFAULT_CONSTRAINTS,
    networkAllowed: true,
    networkAllowlist: ["api.example.com:443", "registry.npmjs.org:443"],
  };

  it("allows destinations in the allowlist", () => {
    expect(isNetworkDestinationAllowed("api.example.com:443", constraints)).toBe(true);
  });

  it("denies destinations not in the allowlist when allowlist is non-empty", () => {
    expect(isNetworkDestinationAllowed("evil.com:443", constraints)).toBe(false);
  });

  it("denies all network when not allowed", () => {
    expect(isNetworkDestinationAllowed("api.example.com:443", DEFAULT_CONSTRAINTS)).toBe(false);
  });

  it("denylist takes precedence over allowlist", () => {
    const c: Required<SandboxConstraints> = {
      ...DEFAULT_CONSTRAINTS,
      networkAllowed: true,
      networkAllowlist: ["api.example.com:443"],
      networkDenylist: ["api.example.com:443"],
    };
    expect(isNetworkDestinationAllowed("api.example.com:443", c)).toBe(false);
  });
});

describe("filesystem path checks", () => {
  const constraints: Required<SandboxConstraints> = {
    ...DEFAULT_CONSTRAINTS,
    allowedReadPaths: ["/workspace/src", "/workspace/config"],
    allowedWritePaths: ["/workspace/src"],
  };

  it("allows read paths under allowed directories", () => {
    expect(isReadPathAllowed("/workspace/src/index.ts", constraints)).toBe(true);
    expect(isReadPathAllowed("/workspace/config/settings.json", constraints)).toBe(true);
  });

  it("denies read paths outside allowed directories", () => {
    expect(isReadPathAllowed("/etc/passwd", constraints)).toBe(false);
    expect(isReadPathAllowed("/workspace/test", constraints)).toBe(false);
  });

  it("allows write paths under allowed directories", () => {
    expect(isWritePathAllowed("/workspace/src/output.ts", constraints)).toBe(true);
  });

  it("denies write paths outside allowed directories", () => {
    expect(isWritePathAllowed("/workspace/config/settings.json", constraints)).toBe(false);
    expect(isWritePathAllowed("/etc/shadow", constraints)).toBe(false);
  });

  it("denies all read when allowed paths is empty", () => {
    const empty: Required<SandboxConstraints> = {
      ...DEFAULT_CONSTRAINTS,
      allowedReadPaths: [],
    };
    expect(isReadPathAllowed("/anything", empty)).toBe(false);
  });

  it("denies all write when allowed paths is empty", () => {
    const empty: Required<SandboxConstraints> = {
      ...DEFAULT_CONSTRAINTS,
      allowedWritePaths: [],
    };
    expect(isWritePathAllowed("/anything", empty)).toBe(false);
  });
});

describe("createSandboxContext", () => {
  it("creates context with default constraints", () => {
    const result = createSandboxContext("verify", [], { constraints: {} });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.tool).toBe("verify");
      expect(result.value.constraints.maxTimeMs).toBe(60000);
    }
  });

  it("rejects invalid constraints", () => {
    const result = createSandboxContext("verify", [], {
      constraints: { maxTimeMs: 100 },
    });
    expect(result.ok).toBe(false);
  });

  it("calls beforeHook when provided", () => {
    let called = false;
    const result = createSandboxContext("verify", ["--workspace", "/tmp"], {
      constraints: {},
      beforeHook: () => {
        called = true;
        return { ok: true as const, value: undefined };
      },
    });
    expect(called).toBe(true);
    expect(result.ok).toBe(true);
  });

  it("rejects when beforeHook returns error", () => {
    const result = createSandboxContext("verify", [], {
      constraints: {},
      beforeHook: () => ({
        ok: false as const,
        error: new Error("blocked") as any,
      }),
    });
    expect(result.ok).toBe(false);
  });

  it("uses tenant and actor IDs from config", () => {
    const result = createSandboxContext("verify", [], {
      constraints: {},
      tenantId: "t-123",
      actorId: "user-456",
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.tenantId).toBe("t-123");
      expect(result.value.actorId).toBe("user-456");
    }
  });
});

// ── Sandbox escape tests ─────────────────────────────────────────────────

describe("Sandbox escape prevention", () => {
  it("blocks path traversal in read paths", () => {
    const constraints: Required<SandboxConstraints> = {
      ...DEFAULT_CONSTRAINTS,
      allowedReadPaths: ["/workspace/src"],
    };
    expect(isReadPathAllowed("/workspace/src/../../../etc/passwd", constraints)).toBe(false);
  });

  it("blocks path traversal in write paths", () => {
    const constraints: Required<SandboxConstraints> = {
      ...DEFAULT_CONSTRAINTS,
      allowedWritePaths: ["/workspace/out"],
    };
    expect(isWritePathAllowed("/workspace/out/../../etc/shadow", constraints)).toBe(false);
  });

  it("blocks shell commands even with allowlist when shell is denied", () => {
    const constraints: Required<SandboxConstraints> = {
      ...DEFAULT_CONSTRAINTS,
      shellAllowed: false,
      allowedCommands: ["ls"],
    };
    // Even though "ls" is in allowedCommands, shell is not allowed
    expect(isCommandAllowed("ls", constraints)).toBe(false);
  });

  it("blocks all network when network is denied at base level", () => {
    const tenantConstraints: SandboxConstraints = {
      networkAllowed: true, // tenant trying to enable network
    };
    const merged = mergeConstraints(DEFAULT_CONSTRAINTS, tenantConstraints);
    expect(merged.networkAllowed).toBe(false);
    expect(isNetworkDestinationAllowed("any-host:443", merged)).toBe(false);
  });

  it("merges denylists to block additional destinations", () => {
    const base: SandboxConstraints = {
      ...DEFAULT_CONSTRAINTS,
      networkAllowed: true,
      networkDenylist: ["malware.com"],
    };
    const tenantConstraints: SandboxConstraints = {
      networkDenylist: ["phishing.com"],
    };
    const merged = mergeConstraints(base as Required<SandboxConstraints>, tenantConstraints);
    expect(isNetworkDestinationAllowed("malware.com", merged)).toBe(false);
    expect(isNetworkDestinationAllowed("phishing.com", merged)).toBe(false);
  });

  it("tenant cannot exceed base resource limits", () => {
    const result = mergeConstraints(DEFAULT_CONSTRAINTS, {
      maxTimeMs: 999999, // way above base
      maxMemoryMb: 999999,
    });
    expect(result.maxTimeMs).toBeLessThanOrEqual(DEFAULT_CONSTRAINTS.maxTimeMs);
    expect(result.maxMemoryMb).toBeLessThanOrEqual(DEFAULT_CONSTRAINTS.maxMemoryMb);
  });

  it("rejects constraints that allow shell by default", () => {
    // Default constraints should deny shell
    expect(DEFAULT_CONSTRAINTS.shellAllowed).toBe(false);
    expect(DEFAULT_CONSTRAINTS.allowedCommands).toEqual([]);
    expect(DEFAULT_CONSTRAINTS.networkAllowed).toBe(false);
    expect(DEFAULT_CONSTRAINTS.allowedReadPaths).toEqual([]);
    expect(DEFAULT_CONSTRAINTS.allowedWritePaths).toEqual([]);
  });

  it("beforeHook can enforce tenant-specific policy", () => {
    const result = createSandboxContext("rm", ["-rf", "/"], {
      constraints: { shellAllowed: true, allowedCommands: ["git", "npm", "node"] },
      beforeHook: (ctx) => {
        if (!isCommandAllowed(ctx.tool, ctx.constraints)) {
          return { ok: false as const, error: new Error(`Tool "${ctx.tool}" not allowed`) as any };
        }
        return { ok: true as const, value: undefined };
      },
    });
    // beforeHook rejects 'rm' because it's not in allowedCommands
    expect(result.ok).toBe(false);
  });
});
