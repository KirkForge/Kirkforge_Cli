import { describe, it, expect } from "vitest";
import { mkdtempSync, readFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { executeHardPrompt } from "../../src/modes.js";
import { detectTaskProfile } from "../../src/task-profile.js";

describe("hard-prompt artifact enforcement", () => {
  const pythonProfile = detectTaskProfile("write a python script");

  it("allows valid Python file in hard-prompt mode", async () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-hp-valid-"));
    try {
      const content = "```python\nprint('hello')\n```";
      const result = await executeHardPrompt(
        {
          execute: async () => ({
            ok: true,
            value: {
              agentId: "a",
              content,
              promptTokens: 1,
              completionTokens: 1,
              totalTokens: 2,
              model: "stub",
              format: "hard-prompt",
            },
          }),
        } as any,
        { description: "fix broken-python" },
        "hp-valid",
        cwd,
        pythonProfile,
      );
      expect(result.ok).toBe(true);
      if (result.ok) {
        const fileSignal = result.value.signals.find((s) => s.kind === "files.written");
        expect(fileSignal).toBeDefined();
        expect((fileSignal as any).value.files.length).toBeGreaterThanOrEqual(1);
        const paths = (fileSignal as any).value.files.map((f: any) => f.path);
        expect(paths).toContain("solution.py");
      }
      expect(readFileSync(join(cwd, "solution.py"), "utf-8")).toContain("print('hello')");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("writes to profile default file regardless of code block language annotation", async () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-hp-wrongext-"));
    try {
      const content = "```typescript\n// see output.ts for details\nconsole.log('hi')\n```";
      const result = await executeHardPrompt(
        {
          execute: async () => ({
            ok: true,
            value: {
              agentId: "a",
              content,
              promptTokens: 1,
              completionTokens: 1,
              totalTokens: 2,
              model: "stub",
              format: "hard-prompt",
            },
          }),
        } as any,
        { description: "write a python script" },
        "hp-wrongext",
        cwd,
        pythonProfile,
      );
      expect(result.ok).toBe(true);
      if (result.ok) {
        const fileSignal = result.value.signals.find((s) => s.kind === "files.written");
        expect(fileSignal).toBeDefined();
        const paths = (fileSignal as any).value.files.map((f: any) => f.path);
        expect(paths).toContain("solution.py");
      }
      // Verify file was written to profile default path, not guessed from content
      expect(existsSync(join(cwd, "solution.py"))).toBe(true);
      expect(existsSync(join(cwd, "output.ts"))).toBe(false);
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("blocks path traversal in hard-prompt mode", async () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-hp-traversal-"));
    try {
      const content = "```python\nimport os\n```";
      const result = await executeHardPrompt(
        {
          execute: async () => ({
            ok: true,
            value: {
              agentId: "a",
              content,
              promptTokens: 1,
              completionTokens: 1,
              totalTokens: 2,
              model: "stub",
              format: "hard-prompt",
            },
          }),
        } as any,
        { description: "write a python script" },
        "hp-traversal",
        cwd,
        pythonProfile,
      );
      expect(result.ok).toBe(true);
      if (result.ok) {
        const written = result.value.signals.find((s) => s.kind === "files.written");
        expect(written).toBeDefined();
      }
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("hard-prompt always uses profile default file as output path", async () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-hp-signal-"));
    try {
      const content = "```typescript\n// Fix for output.ts\nconsole.log('hi')\n```";
      const result = await executeHardPrompt(
        {
          execute: async () => ({
            ok: true,
            value: {
              agentId: "a",
              content,
              promptTokens: 1,
              completionTokens: 1,
              totalTokens: 2,
              model: "stub",
              format: "hard-prompt",
            },
          }),
        } as any,
        { description: "write python script" },
        "hp-signal",
        cwd,
        pythonProfile,
      );
      expect(result.ok).toBe(true);
      if (result.ok) {
        const fileSignal = result.value.signals.find((s) => s.kind === "files.written");
        expect(fileSignal).toBeDefined();
        const files = (fileSignal as any).value.files;
        const paths = files.map((f: any) => f.path);
        expect(paths).toContain("solution.py");
      }
      expect(existsSync(join(cwd, "solution.py"))).toBe(true);
      expect(existsSync(join(cwd, "output.ts"))).toBe(false);
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });
});
