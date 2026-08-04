import { describe, it, expect } from "vitest";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { executeHardPrompt } from "../../src/modes.js";
import { executeArtifact } from "../../src/artifact-mode.js";
import { sha256Of } from "../../src/path-safety.js";
import { detectTaskProfile } from "../../src/task-profile.js";

describe("task profile routing", () => {
  it("detects Python and shell tasks before prompting", () => {
    expect(detectTaskProfile("fix broken-python pandas csv-to-parquet script").language).toBe(
      "python",
    );
    expect(detectTaskProfile("create-bucket using aws cli shell commands").language).toBe("shell");
  });

  it("persists unlabelled code blocks with the detected default extension", async () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-test-"));
    const agent = {
      execute: async () => ({
        ok: true,
        value: {
          agentId: "agent-test",
          content: "```\nprint('ok')\n```",
          promptTokens: 1,
          completionTokens: 1,
          totalTokens: 2,
          model: "stub",
          format: "hard-prompt",
        },
      }),
    };

    try {
      const result = await executeHardPrompt(
        agent as any,
        { description: "fix broken-python" },
        "task-1",
        cwd,
        detectTaskProfile("fix broken-python"),
      );
      expect(result.ok).toBe(true);
      expect(readFileSync(join(cwd, "solution.py"), "utf-8")).toContain("print('ok')");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  it("strips outer markdown fences from artifact file contents", async () => {
    const cwd = mkdtempSync(join(tmpdir(), "kirkforge-artifact-test-"));
    const agent = {
      execute: async () => ({
        ok: true,
        value: {
          agentId: "agent-test",
          content: JSON.stringify({
            type: "file_write",
            path: "solution.py",
            sha256: sha256Of("print('ok')\n"),
            content_b64: Buffer.from("print('ok')\n", "utf-8").toString("base64"),
          }),
          promptTokens: 1,
          completionTokens: 1,
          totalTokens: 2,
          model: "stub",
          format: "artifact",
        },
      }),
    };

    try {
      const result = await executeArtifact(
        agent as any,
        { description: "write solution.py" },
        "task-artifact",
        cwd,
        detectTaskProfile("broken-python"),
      );
      expect(result.ok).toBe(true);
      expect(readFileSync(join(cwd, "solution.py"), "utf-8")).toBe("print('ok')\n");
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  });
});
