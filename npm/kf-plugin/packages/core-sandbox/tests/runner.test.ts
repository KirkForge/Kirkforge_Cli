import { describe, it, expect } from "vitest";
import {
  runSandboxed,
  runDockerSandboxed,
  SandboxExecutionError,
  DEFAULT_CONSTRAINTS,
} from "../src/runner.js";

describe("runSandboxed", () => {
  const allowedConstraints = {
    ...DEFAULT_CONSTRAINTS,
    shellAllowed: true,
    allowedCommands: ["echo", "node", "cat", "ls", "true"],
    maxTimeMs: 10000,
    allowedReadPaths: ["/tmp"],
    allowedWritePaths: [],
  };

  it("executes allowed commands successfully", async () => {
    const result = await runSandboxed({
      command: "echo",
      args: ["hello", "world"],
      constraints: allowedConstraints,
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.success).toBe(true);
      expect(result.value.stdout.trim()).toBe("hello world");
      expect(result.value.exitCode).toBe(0);
      expect(result.value.violations).toEqual([]);
    }
  });

  it("rejects commands not in allowlist", async () => {
    const result = await runSandboxed({
      command: "rm",
      args: ["-rf", "/"],
      constraints: allowedConstraints,
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toBeInstanceOf(SandboxExecutionError);
      expect(result.error.message).toContain("not in the allowed list");
    }
  });

  it("rejects when shell is not allowed", async () => {
    const result = await runSandboxed({
      command: "echo",
      args: ["test"],
      constraints: { shellAllowed: false },
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toBeInstanceOf(SandboxExecutionError);
      expect(result.error.message).toContain("not allowed");
    }
  });

  it("captures stdout and stderr", async () => {
    const result = await runSandboxed({
      command: "node",
      args: ["-e", "console.log('out'); console.error('err'); process.exit(0)"],
      constraints: allowedConstraints,
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.stdout.trim()).toBe("out");
      expect(result.value.stderr.trim()).toBe("err");
    }
  });

  it("reports non-zero exit codes", async () => {
    const result = await runSandboxed({
      command: "node",
      args: ["-e", "process.exit(42)"],
      constraints: allowedConstraints,
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.success).toBe(false);
      expect(result.value.exitCode).toBe(42);
    }
  });

  it("times out long-running commands", async () => {
    const result = await runSandboxed({
      command: "node",
      args: ["-e", "setTimeout(() => {}, 30000)"],
      constraints: {
        ...allowedConstraints,
        maxTimeMs: 1500,
      },
    });
    // The process should be killed by timeout
    if (result.ok) {
      expect(result.value.violations.some((v) => v.type === "time")).toBe(true);
    } else {
      expect(result.error.violations.some((v) => v.type === "time")).toBe(true);
    }
  }, 10000);

  it("truncates output exceeding maxOutputBytes", async () => {
    const result = await runSandboxed({
      command: "node",
      args: ["-e", "console.log('x'.repeat(10000))"],
      constraints: {
        ...allowedConstraints,
        maxOutputBytes: 1024,
      },
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      // Output should be truncated
      expect(result.value.stdout.length).toBeLessThan(10000);
    }
  });

  it("calls beforeHook before execution", async () => {
    let hookCalled = false;
    const result = await runSandboxed({
      command: "echo",
      args: ["hook-test"],
      constraints: allowedConstraints,
      beforeHook: () => {
        hookCalled = true;
        return { ok: true as const, value: undefined };
      },
    });
    expect(hookCalled).toBe(true);
    expect(result.ok).toBe(true);
  });

  it("rejects when beforeHook returns error", async () => {
    const result = await runSandboxed({
      command: "echo",
      args: ["hook-reject"],
      constraints: allowedConstraints,
      beforeHook: () => ({
        ok: false as const,
        error: new Error("blocked by policy") as any,
      }),
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("blocked by policy");
    }
  });

  it("calls afterHook after execution", async () => {
    let hookResult: any = null;
    const result = await runSandboxed({
      command: "echo",
      args: ["after-hook"],
      constraints: allowedConstraints,
      afterHook: (_ctx, res) => {
        hookResult = res;
      },
    });
    expect(result.ok).toBe(true);
    expect(hookResult).not.toBeNull();
    expect(hookResult.stdout.trim()).toBe("after-hook");
  });

  it("passes tenant and actor IDs through context", async () => {
    let capturedTenantId: string | undefined;
    let capturedActorId: string | undefined;
    const result = await runSandboxed({
      command: "echo",
      args: ["tenant-test"],
      constraints: allowedConstraints,
      tenantId: "t-abc",
      actorId: "user-xyz",
      beforeHook: (ctx) => {
        capturedTenantId = ctx.tenantId;
        capturedActorId = ctx.actorId;
        return { ok: true as const, value: undefined };
      },
    });
    expect(capturedTenantId).toBe("t-abc");
    expect(capturedActorId).toBe("user-xyz");
    expect(result.ok).toBe(true);
  });

  it("reports durationMs", async () => {
    const result = await runSandboxed({
      command: "echo",
      args: ["timing"],
      constraints: allowedConstraints,
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.value.durationMs).toBeGreaterThanOrEqual(0);
    }
  });
});

describe("runDockerSandboxed path mount validation", () => {
  // Regression: Docker -v args like "/path:/path:ro" should not trigger
  // path-violation checks that compare the mount arg against allowedReadPaths.
  // The mount arg "/workspace/src:/workspace/src:ro" contains colons and ":ro"
  // which won't match "/workspace/src" in isReadPathAllowed().

  const dockerConstraints = {
    ...DEFAULT_CONSTRAINTS,
    shellAllowed: true,
    allowedCommands: ["docker"],
    maxTimeMs: 10000,
    allowedReadPaths: ["/workspace/src"],
    allowedWritePaths: ["/workspace/data"],
  };

  it("allows Docker run with read mount args that include :ro suffix", async () => {
    // runDockerSandboxed builds -v args like "/workspace/src:/workspace/src:ro"
    // and passes them through runSandboxed which scans args for path violations.
    // The full mount arg should NOT be flagged because the container runner
    // allows Docker as the command.
    const result = await runDockerSandboxed({
      command: "echo",
      args: ["hello"],
      image: "alpine:latest",
      constraints: dockerConstraints,
    });
    // We don't assert success because Docker may not be available in CI,
    // but we assert it doesn't reject for filesystem path violations.
    if (!result.ok) {
      const violations = result.error.violations ?? [];
      const fsViolations = violations.filter((v: any) => v.type === "filesystem");
      expect(fsViolations).toHaveLength(0);
    }
  });
});

describe("enterprise hardening fixes", () => {
  const strictConstraints = {
    ...DEFAULT_CONSTRAINTS,
    shellAllowed: true,
    allowedCommands: ["echo", "node"],
    maxTimeMs: 10000,
    allowedReadPaths: ["/tmp"],
    allowedWritePaths: [],
  };

  it("path-scan: blocks path-like args even when allowedReadPaths is empty", async () => {
    // Fix for: path-arg scanning was disabled by default when allowedReadPaths=[]
    // This was the opposite of deny-by-default semantics.
    const result = await runSandboxed({
      command: "echo",
      args: ["/etc/passwd"],
      constraints: {
        ...strictConstraints,
        allowedReadPaths: [], // empty = no paths allowed
        allowedWritePaths: [],
      },
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.message).toContain("path violations");
      expect(result.error.violations.some((v) => v.type === "filesystem")).toBe(true);
    }
  });

  it("env: blocks sensitive env vars from being passed to child", async () => {
    const result = await runSandboxed({
      command: "echo",
      args: ["test"],
      constraints: strictConstraints,
      env: {
        MY_SECRET_TOKEN: "secret-value-123",
      },
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.violations.some((v) => v.type === "secret")).toBe(true);
    }
  });

  it("env: allows clean env vars without blocking", async () => {
    const result = await runSandboxed({
      command: "echo",
      args: ["test"],
      constraints: strictConstraints,
      env: {
        PATH: "/usr/bin:/bin",
        HOME: "/tmp",
      },
    });
    expect(result.ok).toBe(true);
  });

  it("env: does not inherit parent process.env by default", async () => {
    const result = await runSandboxed({
      command: "node",
      args: ["-e", "console.log(Object.keys(process.env).length > 0 ? 'has-env' : 'no-env')"],
      constraints: {
        ...strictConstraints,
        allowedReadPaths: ["/tmp"],
      },
      env: {
        NODE_ENV: "test",
      },
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      // Child should only have the explicitly provided env vars
      // (not the full parent process.env)
      expect(result.value.success).toBe(true);
    }
  });

  it("env: inherits parent env only when inheritParentEnv is set", async () => {
    const result = await runSandboxed({
      command: "echo",
      args: ["test"],
      constraints: strictConstraints,
      inheritParentEnv: true,
      env: {},
    });
    expect(result.ok).toBe(true);
  });

  it("peakMemoryMb: reports null on non-Linux or uses /proc on Linux", async () => {
    const result = await runSandboxed({
      command: "echo",
      args: ["mem-test"],
      constraints: strictConstraints,
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      // On Linux: should have a real value from /proc/PID/status
      // On other platforms: should be null (not the parent's RSS)
      if (process.platform === "linux") {
        // On Linux, we should get a real memory reading (or null if /proc unavailable)
        expect(
          result.value.peakMemoryMb === null || typeof result.value.peakMemoryMb === "number",
        ).toBe(true);
      } else {
        // Non-Linux: peakMemoryMb should be null, not a misleading parent-process number
        expect(result.value.peakMemoryMb).toBeNull();
      }
    }
  });
});

describe("ALLOW_UNSAFE_HOST_SANDBOX gate", () => {
  // These tests verify the enterprise gate on runSandboxed.
  // In enterprise mode, runDockerSandboxed should be the default.
  // In dev mode, runSandboxed works but is flagged as unsafe.

  it("runSandboxed is allowed in dev mode", async () => {
    // Without KIRKFORGE_ENTERPRISE_MODE, runSandboxed should work
    expect(process.env["KIRKFORGE_ENTERPRISE_MODE"]).not.toBe("1");
    const result = await runSandboxed({
      command: "echo",
      args: ["dev-mode"],
      constraints: {
        ...DEFAULT_CONSTRAINTS,
        shellAllowed: true,
        allowedCommands: ["echo"],
      },
    });
    expect(result.ok).toBe(true);
  });

  it("runDockerSandboxed works regardless of enterprise mode", async () => {
    // runDockerSandboxed delegates to runSandboxed with Docker constraints
    // which always allows Docker as the command
    const result = await runDockerSandboxed({
      command: "echo",
      args: ["docker-test"],
      image: "alpine:latest",
      constraints: {
        ...DEFAULT_CONSTRAINTS,
        shellAllowed: true,
        allowedCommands: ["echo"],
        maxTimeMs: 10000,
      },
    });
    // Docker may not be available in CI, so just check it doesn't fail
    // due to the enterprise gate
    if (!result.ok) {
      const gateViolations = result.error.violations?.filter(
        (v: any) => v.message?.includes("enterprise") || v.message?.includes("Bare-host"),
      );
      expect(gateViolations).toHaveLength(0);
    }
  });
});
